import { execFile, execFileSync } from "node:child_process";
import { createHash } from "node:crypto";
import { existsSync, unlinkSync } from "node:fs";
import {
	assertOnlyExplicitStagedPaths,
	conciseCommandResult,
	validateExplicitPaths,
	validateSafeBranchCreateInputs,
	validateSafeBranchSwitchInputs,
	validateSafeCommitInputs,
	summarizeBranchPullRequestStatus,
	validateSafePushInputs,
} from "./auto-review-git-safety.mjs";
import type {
	ExtensionAPI,
	ToolCallEvent,
} from "@earendil-works/pi-coding-agent";
import { isToolCallEventType } from "@earendil-works/pi-coding-agent";
import { Type } from "typebox";

type RgrStage = "red" | "green" | "refactor";

type RgrCycle = {
	behavior: string;
	test: string;
	command?: string;
	failingOutput?: string;
	failingOutputHash?: string;
	greenCommand?: string;
	greenOutputHash?: string;
	stage: RgrStage;
};

type AutoReviewEntry =
	| { kind: "rgr_start"; cycle: RgrCycle }
	| {
			kind: "rgr_record_red";
			command: string;
			output: string;
			outputHash: string;
	  }
	| {
			kind: "rgr_mark_green";
			command: string;
			output: string;
			outputHash: string;
			reason?: string;
	  }
	| { kind: "rgr_mark_refactor"; verification: string }
	| { kind: "touch"; path: string }
	| { kind: "verification"; status: string }
	| { kind: "forgejo_feedback"; summary: string }
	| { kind: "allow_main_edits"; reason: string };

const ENTRY_TYPE = "auto-review-guardrails";

const TEXT_RESULT = (text: string, details?: Record<string, unknown>) => ({
	content: [{ type: "text" as const, text }],
	details,
});

function normalizePath(path: string): string {
	return path.replaceAll("\\", "/");
}

function isProductionRustPath(path: string): boolean {
	return /(^|\/)crates\/[^/]+\/src\/.*\.rs$/.test(normalizePath(path));
}

function isLikelyTestPath(path: string): boolean {
	const normalized = normalizePath(path);
	return (
		/(^|\/)(tests|benches)\//.test(normalized) ||
		/(^|\/)crates\/[^/]+\/tests\//.test(normalized)
	);
}

function isNonBehavioralPath(path: string): boolean {
	const normalized = normalizePath(path);
	return (
		/(^|\/)(docs|deploy)\//.test(normalized) ||
		/(^|\/)README\.md$/.test(normalized) ||
		/(^|\/)CHANGELOG\.md$/.test(normalized) ||
		/\.md$/.test(normalized)
	);
}

function isRecord(value: unknown): value is Record<string, unknown> {
	return typeof value === "object" && value !== null;
}

function filePathFromInput(input: unknown): string | undefined {
	if (!isRecord(input)) return undefined;
	const path = input.path ?? input.filePath ?? input.file_path;
	return typeof path === "string" ? path : undefined;
}

function isEditTool(event: ToolCallEvent): boolean {
	return (
		event.toolName === "edit" ||
		event.toolName === "write" ||
		/apply_patch/i.test(event.toolName)
	);
}

function isTodoTool(event: ToolCallEvent): boolean {
	return /todo(write|update)?$/i.test(event.toolName);
}

function rejectsWaterfallTodo(input: unknown): boolean {
	const text = JSON.stringify(input ?? "").toLowerCase();
	const componentWords = [
		"model",
		"handler",
		"route",
		"repository",
		"service",
		"then add tests",
	];
	const hasComponents =
		componentWords.filter((word) => text.includes(word)).length >= 2;
	return (
		hasComponents &&
		!text.includes("red") &&
		!text.includes("failing test") &&
		!text.includes("rgr")
	);
}

function hashOutput(output: string): string {
	return createHash("sha256").update(output).digest("hex").slice(0, 16);
}

function forgejoInlineReplyPayload(comment: {
	body: string;
	path: string;
	position: number;
}) {
	return {
		body: comment.body,
		path: comment.path,
		new_position: comment.position,
		old_position: 0,
	};
}

function gitOutput(args: string[]): string | undefined {
	try {
		return execFileSync("git", args, {
			cwd: process.cwd(),
			encoding: "utf8",
			stdio: ["ignore", "pipe", "ignore"],
			maxBuffer: 1024 * 1024,
		}).trim();
	} catch {
		return undefined;
	}
}

function currentBranch(): string | undefined {
	return gitOutput(["branch", "--show-current"]);
}

type BranchPullRequest = {
	number?: number;
	index?: number;
	state?: string;
	merged?: boolean;
	merged_at?: string | null;
	title?: string;
	body?: string;
	head?: { ref?: string };
};

async function branchPullRequests(branch: string): Promise<BranchPullRequest[]> {
	const token = process.env.FORGEJO_TOKEN;
	if (!token) {
		throw new Error(
			"FORGEJO_TOKEN is required so safe_commit/safe_push can check PR state for the current branch.",
		);
	}
	const matches: BranchPullRequest[] = [];
	for (let page = 1; ; page += 1) {
		const response = await fetch(
			`https://git.johnwilger.com/api/v1/repos/jwilger/auto_review/pulls?state=all&limit=50&page=${page}`,
			{ headers: { Authorization: `token ${token}` } },
		);
		if (!response.ok) {
			throw new Error(
				`Forgejo PR lookup failed with status ${response.status}; safe_commit/safe_push cannot verify branch PR state.`,
			);
		}
		const pulls = (await response.json()) as BranchPullRequest[];
		matches.push(...pulls.filter((pull) => pull.head?.ref === branch));
		if (pulls.length < 50) return matches;
	}
}

function runCommand(
	command: string,
	args: string[],
): Promise<{ status: number; output: string }> {
	return new Promise((resolve) => {
		execFile(
			command,
			args,
			{
				cwd: process.cwd(),
				encoding: "utf8",
				maxBuffer: 10 * 1024 * 1024,
			},
			(error, stdout, stderr) => {
				const output = [stdout, stderr].filter(Boolean).join("");
				resolve({
					status:
						error && typeof error === "object" && "code" in error
							? Number(error.code) || 1
							: 0,
					output,
				});
			},
		);
	});
}

function runGitCommand(
	args: string[],
): Promise<{ status: number; output: string }> {
	return runCommand("git", args);
}

function outputTail(output: string): string {
	const lines = output.trim().split(/\r?\n/).filter(Boolean);
	return lines.slice(-20).join("\n");
}

async function assertGitSuccess(args: string[]): Promise<string> {
	const result = await runGitCommand(args);
	if (result.status !== 0) {
		throw new Error(
			[
				`git ${args.join(" ")} failed with status ${result.status}`,
				outputTail(result.output),
			]
				.filter(Boolean)
				.join("\n"),
		);
	}
	return result.output;
}

async function stageExplicitPaths(paths: string[]): Promise<void> {
	await assertGitSuccess(["add", "--", ...paths]);
}

function stagedPaths(): string[] {
	const staged = gitOutput(["diff", "--cached", "--name-only"]);
	return staged ? staged.split("\n").filter(Boolean) : [];
}

function ensureNoPreStagedPaths(): void {
	const paths = stagedPaths();
	if (paths.length) {
		throw new Error(
			`safe_commit refuses to include pre-staged paths: ${paths.join(", ")}`,
		);
	}
}

function ensureCleanAfterCommit(): void {
	const dirty = dirtyStatus();
	if (dirty.count) {
		throw new Error(
			[
				"safe_commit completed but the working tree is dirty after hooks ran.",
				...dirty.preview.map((line) => `  ${line}`),
			].join("\n"),
		);
	}
}

function ensureLefthookInstalled(): void {
	if (!existsSync("lefthook.yml")) return;
	try {
		// Keep Pi sessions aligned with the dev shell's `lefthook install` setup.
		execFileSync("lefthook", ["install"], {
			cwd: process.cwd(),
			stdio: "ignore",
		});
	} catch (error) {
		console.warn(`auto_review guardrail: lefthook install failed: ${error}`);
	}
}

function dirtyStatus(): { count: number; preview: string[] } {
	const status = gitOutput(["status", "--short"]);
	if (!status) return { count: 0, preview: [] };
	const lines = status.split("\n").filter(Boolean);
	return { count: lines.length, preview: lines.slice(0, 10) };
}

function readEntryData(entry: unknown): AutoReviewEntry | undefined {
	if (!isRecord(entry)) return undefined;
	if (entry.type !== "custom" || entry.customType !== ENTRY_TYPE)
		return undefined;
	const data = entry.data;
	if (!isRecord(data) || typeof data.kind !== "string") return undefined;
	return data as AutoReviewEntry;
}

export default function autoReviewGuardrails(pi: ExtensionAPI) {
	let cycle: RgrCycle | undefined;
	const touchedFiles = new Set<string>();
	let verification: string | undefined;
	const forgejoFeedback: string[] = [];
	let mainEditOverride: string | undefined;

	function persist(entry: AutoReviewEntry): void {
		pi.appendEntry(ENTRY_TYPE, entry);
	}

	function applyEntry(entry: AutoReviewEntry): void {
		switch (entry.kind) {
			case "rgr_start":
				cycle = entry.cycle;
				break;
			case "rgr_record_red":
				if (cycle) {
					cycle = {
						...cycle,
						command: entry.command,
						failingOutput: entry.output,
						failingOutputHash: entry.outputHash,
						stage: "red",
					};
				}
				break;
			case "rgr_mark_green":
				if (cycle) {
					cycle = {
						...cycle,
						greenCommand: entry.command,
						greenOutputHash: entry.outputHash,
						stage: "green",
					};
				}
				verification = entry.output;
				break;
			case "rgr_mark_refactor":
				verification = entry.verification;
				cycle = undefined;
				break;
			case "touch":
				touchedFiles.add(entry.path);
				break;
			case "verification":
				verification = entry.status;
				break;
			case "forgejo_feedback":
				forgejoFeedback.push(entry.summary);
				break;
			case "allow_main_edits":
				mainEditOverride = entry.reason;
				break;
		}
	}

	function recordTouchedFile(path: string): void {
		touchedFiles.add(path);
		persist({ kind: "touch", path });
	}

	function recordVerification(status: string): void {
		verification = status;
		persist({ kind: "verification", status });
	}

	function recordForgejoFeedback(summary: string): void {
		forgejoFeedback.push(summary);
		persist({ kind: "forgejo_feedback", summary });
	}

	function sessionContext(): string[] {
		const context: string[] = [];
		if (cycle) context.push(`Active RGR cycle: ${JSON.stringify(cycle)}`);
		if (touchedFiles.size)
			context.push(
				`Touched files: ${Array.from(touchedFiles).sort().join(", ")}`,
			);
		if (verification) context.push(`Verification status: ${verification}`);
		if (forgejoFeedback.length)
			context.push(
				`Unresolved Forgejo feedback: ${forgejoFeedback.join("; ")}`,
			);
		if (mainEditOverride)
			context.push(`Main-branch edit override: ${mainEditOverride}`);
		return context;
	}

	pi.on("session_start", (_event, ctx) => {
		ensureLefthookInstalled();

		cycle = undefined;
		touchedFiles.clear();
		verification = undefined;
		forgejoFeedback.length = 0;
		mainEditOverride = undefined;

		for (const entry of ctx.sessionManager.getEntries()) {
			const data = readEntryData(entry);
			if (data) applyEntry(data);
		}
	});

	pi.on("before_agent_start", (event) => {
		const context = sessionContext();
		if (!context.length) return undefined;
		return {
			systemPrompt: `${event.systemPrompt}\n\nauto_review preserved guardrail context:\n${context.map((line) => `- ${line}`).join("\n")}`,
		};
	});

	pi.registerTool({
		name: "rgr_start",
		label: "RGR Start",
		description:
			"Start an auto_review RED-GREEN-REFACTOR cycle for one behavior.",
		promptSnippet:
			"Start a RED-GREEN-REFACTOR ledger for one observable behavior.",
		promptGuidelines: [
			"Use rgr_start before behavior production edits in auto_review.",
		],
		parameters: Type.Object({
			behavior: Type.String({ description: "Observable behavior under test" }),
			test: Type.String({ description: "Specific failing test name or path" }),
			command: Type.Optional(
				Type.String({ description: "Focused command expected to show RED" }),
			),
		}),
		async execute(_toolCallId, params) {
			cycle = {
				behavior: params.behavior,
				test: params.test,
				command: params.command,
				stage: "red",
			};
			persist({ kind: "rgr_start", cycle });
			return TEXT_RESULT(
				`RGR cycle started for ${params.behavior}. Record observed RED output before production edits.`,
				{ stage: "red", command: params.command },
			);
		},
	});

	pi.registerTool({
		name: "rgr_record_red",
		label: "RGR Record RED",
		description:
			"Record observed failing test output for the active RGR cycle.",
		promptSnippet: "Record actual failing output before production Rust edits.",
		promptGuidelines: [
			"Use rgr_record_red only with copied output from a real focused command run.",
		],
		parameters: Type.Object({
			command: Type.String({ description: "Focused test command that failed" }),
			output: Type.String({
				description: "Copied failing output from the actual run",
			}),
		}),
		async execute(_toolCallId, params) {
			if (!cycle) throw new Error("Start an RGR cycle before recording RED.");
			const outputHash = hashOutput(params.output);
			cycle = {
				...cycle,
				command: params.command,
				failingOutput: params.output,
				failingOutputHash: outputHash,
				stage: "red",
			};
			persist({
				kind: "rgr_record_red",
				command: params.command,
				output: params.output,
				outputHash,
			});
			return TEXT_RESULT(
				"RED recorded. Minimum production edits are now allowed for this cycle.",
				{
					stage: "red",
					command: params.command,
					outputHash,
				},
			);
		},
	});

	pi.registerTool({
		name: "rgr_mark_green",
		label: "RGR Mark GREEN",
		description:
			"Mark the active RGR cycle green after the focused test passes.",
		promptSnippet:
			"Record passing focused verification for an active RGR cycle.",
		promptGuidelines: [
			"Use rgr_mark_green with the same command recorded for RED unless you include an explicit reason.",
		],
		parameters: Type.Object({
			command: Type.String({ description: "Focused test command that passed" }),
			output: Type.String({
				description: "Passing test output or concise verification summary",
			}),
			reason: Type.Optional(
				Type.String({
					description: "Reason if GREEN used a command different from RED",
				}),
			),
		}),
		async execute(_toolCallId, params) {
			if (!cycle?.failingOutput)
				throw new Error("Cannot mark GREEN before observed RED is recorded.");
			if (cycle.command && cycle.command !== params.command && !params.reason) {
				throw new Error(
					"GREEN command must match the recorded RED command unless a reason is supplied.",
				);
			}
			const outputHash = hashOutput(params.output);
			cycle = {
				...cycle,
				greenCommand: params.command,
				greenOutputHash: outputHash,
				stage: "green",
			};
			recordVerification(params.output);
			persist({
				kind: "rgr_mark_green",
				command: params.command,
				output: params.output,
				outputHash,
				reason: params.reason,
			});
			return TEXT_RESULT(
				"GREEN recorded. Refactoring is allowed with tests green.",
				{
					stage: "green",
					command: params.command,
					outputHash,
					reason: params.reason,
				},
			);
		},
	});

	pi.registerTool({
		name: "rgr_mark_refactor",
		label: "RGR Mark REFACTOR",
		description: "Mark refactor completion and clear the active RGR cycle.",
		promptSnippet:
			"Record post-refactor verification and clear the RGR ledger.",
		parameters: Type.Object({
			verification: Type.String({
				description: "Verification run after refactor",
			}),
		}),
		async execute(_toolCallId, params) {
			recordVerification(params.verification);
			cycle = undefined;
			persist({ kind: "rgr_mark_refactor", verification: params.verification });
			return TEXT_RESULT("REFACTOR recorded. RGR cycle complete.", {
				stage: "refactor",
			});
		},
	});

	pi.registerTool({
		name: "rgr_status",
		label: "RGR Status",
		description: "Inspect active RGR and verification context.",
		promptSnippet:
			"Inspect active RGR state, touched files, verification, and Forgejo feedback.",
		parameters: Type.Object({}),
		async execute() {
			const items = sessionContext();
			return TEXT_RESULT(
				items.length
					? items.join("\n")
					: "No active RGR cycle recorded for this session.",
			);
		},
	});

	pi.registerTool({
		name: "forgejo_inline_reply_payload",
		label: "Forgejo Inline Reply Payload",
		description:
			"Build the Forgejo inline review reply payload using comment.position as new_position.",
		promptSnippet:
			"Build a Forgejo inline review reply payload with old_position set to 0.",
		promptGuidelines: [
			"Use forgejo_inline_reply_payload before replying to an existing Forgejo inline review thread via REST.",
		],
		parameters: Type.Object({
			body: Type.String({ description: "Reply body" }),
			path: Type.String({ description: "Original inline comment path" }),
			position: Type.Integer({
				minimum: 0,
				description: "Original inline comment position field",
			}),
		}),
		async execute(_toolCallId, params) {
			return TEXT_RESULT(
				JSON.stringify(forgejoInlineReplyPayload(params), null, 2),
			);
		},
	});

	pi.registerTool({
		name: "forgejo_feedback_status",
		label: "Forgejo Feedback Status",
		description:
			"Record or summarize unresolved Forgejo feedback status for preserved context.",
		promptSnippet:
			"Record unresolved Forgejo review feedback so it survives context changes.",
		parameters: Type.Object({
			summary: Type.String({ description: "Feedback status summary" }),
		}),
		async execute(_toolCallId, params) {
			recordForgejoFeedback(params.summary);
			return TEXT_RESULT(`Forgejo feedback status recorded: ${params.summary}`);
		},
	});

	pi.registerTool({
		name: "forgejo_review_api_recipe",
		label: "Forgejo Review API Recipe",
		description:
			"Return the Forgejo API recipe for listing reviews/comments and replying inline.",
		promptSnippet: "Show Forgejo review comment endpoints for inline replies.",
		parameters: Type.Object({
			owner: Type.String(),
			repo: Type.String(),
			pr: Type.Integer({ minimum: 1 }),
		}),
		async execute(_toolCallId, params) {
			return TEXT_RESULT(
				[
					`GET /api/v1/repos/${params.owner}/${params.repo}/pulls/${params.pr}/reviews`,
					`GET /api/v1/repos/${params.owner}/${params.repo}/pulls/${params.pr}/reviews/{review_id}/comments`,
					`POST /api/v1/repos/${params.owner}/${params.repo}/pulls/${params.pr}/reviews/{review_id}/comments`,
					"Payload: { body, path: comment.path, new_position: comment.position, old_position: 0 }",
				].join("\n"),
			);
		},
	});

	pi.registerTool({
		name: "auto_review_allow_main_edits",
		label: "Allow Main Edits",
		description:
			"Record explicit user authorization to edit files while on main.",
		promptSnippet:
			"Record a user-authorized main-branch edit override for auto_review.",
		promptGuidelines: [
			"Use auto_review_allow_main_edits only after the user explicitly authorizes editing on main.",
		],
		parameters: Type.Object({
			reason: Type.String({
				description: "User authorization or reason for editing on main",
			}),
		}),
		async execute(_toolCallId, params) {
			mainEditOverride = params.reason;
			persist({ kind: "allow_main_edits", reason: params.reason });
			return TEXT_RESULT(
				`Main-branch edit override recorded: ${params.reason}`,
			);
		},
	});

	pi.registerTool({
		name: "safe_commit",
		label: "Safe Commit",
		description:
			"Stage explicit paths and create a signed/hooked git commit with post-hook cleanliness checks.",
		promptSnippet:
			"Use safe_commit instead of bash git add/git commit for auto_review commits.",
		promptGuidelines: [
			"Use safe_commit for commits so only explicit paths are staged and hooks/signing are preserved.",
		],
		parameters: Type.Object({
			paths: Type.Array(
				Type.String({ description: "Explicit file path to stage" }),
				{ minItems: 1, description: "Explicit file paths to stage" },
			),
			message: Type.String({ description: "Commit subject" }),
			body: Type.Optional(Type.String({ description: "Commit body" })),
		}),
		async execute(_toolCallId, params) {
			const branch = currentBranch();
			const { paths } = validateSafeCommitInputs({
				paths: params.paths,
				currentBranch: branch,
				branchPullRequests: branch ? await branchPullRequests(branch) : [],
			});
			ensureNoPreStagedPaths();
			await stageExplicitPaths(paths);
			const staged = stagedPaths();
			if (staged.length === 0) {
				throw new Error(
					"safe_commit found no staged changes after staging explicit paths.",
				);
			}
			assertOnlyExplicitStagedPaths(paths, staged);

			const commitArgs = ["commit", "-m", params.message];
			if (params.body?.trim()) commitArgs.push("-m", params.body.trim());
			const output = await assertGitSuccess(commitArgs);
			ensureCleanAfterCommit();
			const head = gitOutput(["rev-parse", "--short", "HEAD"]);
			const result = conciseCommandResult({
				toolName: "safe_commit",
				summary: [
					`safe_commit created ${head ?? "a commit"}.`,
					`staged paths: ${paths.join(", ")}`,
				]
					.filter(Boolean)
					.join("\n"),
				output,
			});
			return TEXT_RESULT(result.text, {
				commit: head,
				paths,
				outputPath: result.outputPath,
			});
		},
	});

	pi.registerTool({
		name: "safe_create_branch",
		label: "Safe Create Branch",
		description:
			"Create and switch to a non-main branch after validating the working tree is clean.",
		promptSnippet:
			"Use safe_create_branch instead of bash git checkout -b/switch -c for auto_review branch creation.",
		parameters: Type.Object({
			branch: Type.String({ description: "Non-main branch to create" }),
		}),
		async execute(_toolCallId, params) {
			const dirty = dirtyStatus();
			const createTarget = validateSafeBranchCreateInputs({
				branch: params.branch,
				currentBranch: currentBranch(),
				dirtyCount: dirty.count,
			});
			const output = await assertGitSuccess([
				"switch",
				"--create",
				createTarget.branch,
			]);
			const result = conciseCommandResult({
				toolName: "safe_create_branch",
				summary: `safe_create_branch created and switched to ${createTarget.branch}.`,
				output,
			});
			return TEXT_RESULT(result.text, {
				branch: createTarget.branch,
				outputPath: result.outputPath,
			});
		},
	});

	pi.registerTool({
		name: "safe_switch_branch",
		label: "Safe Switch Branch",
		description:
			"Switch from the current branch to a non-main branch after validating the working tree is clean.",
		promptSnippet:
			"Use safe_switch_branch instead of bash git checkout/switch for auto_review branch changes.",
		parameters: Type.Object({
			branch: Type.String({ description: "Non-main branch to switch to" }),
		}),
		async execute(_toolCallId, params) {
			const dirty = dirtyStatus();
			const switchTarget = validateSafeBranchSwitchInputs({
				branch: params.branch,
				currentBranch: currentBranch(),
				dirtyCount: dirty.count,
			});
			const output = await assertGitSuccess(["switch", switchTarget.branch]);
			const result = conciseCommandResult({
				toolName: "safe_switch_branch",
				summary: `safe_switch_branch switched to ${switchTarget.branch}.`,
				output,
			});
			return TEXT_RESULT(result.text, {
				branch: switchTarget.branch,
				outputPath: result.outputPath,
			});
		},
	});

	pi.registerTool({
		name: "safe_push",
		label: "Safe Push",
		description:
			"Push the current branch without shelling out through bash; force-with-lease requires explicit justification.",
		promptSnippet:
			"Use safe_push instead of bash git push for auto_review branch updates.",
		promptGuidelines: [
			"Use safe_push for normal pushes; request forceWithLease only when explicitly authorized and justified.",
		],
		parameters: Type.Object({
			remote: Type.Optional(
				Type.String({ description: "Remote name", default: "origin" }),
			),
			branch: Type.Optional(
				Type.String({ description: "Branch name; defaults to current branch" }),
			),
			forceWithLease: Type.Optional(
				Type.Boolean({ description: "Use --force-with-lease" }),
			),
			justification: Type.Optional(
				Type.String({ description: "Required when forceWithLease is true" }),
			),
			prMetadataReviewed: Type.Optional(
				Type.Boolean({
					description:
						"Required when an open PR exists for this branch; confirms its title and description cover all branch commits",
				}),
			),
		}),
		async execute(_toolCallId, params) {
			const remotes = (gitOutput(["remote"]) ?? "").split("\n").filter(Boolean);
			const remoteUrls = Object.fromEntries(
				remotes.map((remote) => [
					remote,
					(gitOutput(["remote", "get-url", "--all", remote]) ?? "")
						.split("\n")
						.filter(Boolean),
				]),
			);
			const pushUrls = Object.fromEntries(
				remotes.map((remote) => [
					remote,
					(gitOutput(["remote", "get-url", "--push", "--all", remote]) ?? "")
						.split("\n")
						.filter(Boolean),
				]),
			);
			const current = currentBranch();
			const pullRequests = current ? await branchPullRequests(current) : [];
			const push = validateSafePushInputs({
				remote: params.remote ?? "origin",
				branch: params.branch,
				currentBranch: current,
				configuredRemotes: remotes,
				remoteUrls,
				pushUrls,
				forceWithLease: params.forceWithLease,
				justification: params.justification,
				branchPullRequests: pullRequests,
				prMetadataReviewed: params.prMetadataReviewed,
			});
			const { remote, branch } = push;

			const pushArgs = push.forceWithLease
				? ["push", "--force-with-lease", remote, `HEAD:refs/heads/${branch}`]
				: ["push", remote, branch];
			const output = await assertGitSuccess(pushArgs);
			const result = conciseCommandResult({
				toolName: "safe_push",
				summary: [
					`safe_push pushed ${branch} to ${remote}.`,
					summarizeBranchPullRequestStatus(pullRequests),
					push.forceWithLease
						? `force-with-lease: ${push.justification}`
						: undefined,
				]
					.filter(Boolean)
					.join("\n"),
				output,
			});
			return TEXT_RESULT(result.text, {
				remote,
				branch,
				forceWithLease: push.forceWithLease,
				outputPath: result.outputPath,
			});
		},
	});

	pi.registerTool({
		name: "safe_unstage",
		label: "Safe Unstage",
		description: "Remove explicit files from the Git index without shelling out through bash.",
		promptSnippet:
			"Use safe_unstage to unstage explicit paths before safe_commit when needed.",
		parameters: Type.Object({
			paths: Type.Array(
				Type.String({ description: "Explicit file path to unstage" }),
				{ minItems: 1, description: "Explicit file paths to unstage" },
			),
		}),
		async execute(_toolCallId, params) {
			const paths = validateExplicitPaths(params.paths);
			const output = await assertGitSuccess(["restore", "--staged", "--", ...paths]);
			const result = conciseCommandResult({
				toolName: "safe_unstage",
				summary: `safe_unstage unstaged: ${paths.join(", ")}`,
				output,
			});
			return TEXT_RESULT(result.text, { paths, outputPath: result.outputPath });
		},
	});

	pi.registerTool({
		name: "safe_remove",
		label: "Safe Remove",
		description: "Delete explicit files without shelling out through bash.",
		promptSnippet:
			"Use safe_remove instead of shell rm when deleting explicit files in auto_review.",
		parameters: Type.Object({
			paths: Type.Array(
				Type.String({ description: "Explicit file path to delete" }),
				{ minItems: 1, description: "Explicit file paths to delete" },
			),
		}),
		async execute(_toolCallId, params) {
			const paths = validateExplicitPaths(params.paths);
			for (const path of paths) unlinkSync(path);
			return TEXT_RESULT(`safe_remove deleted: ${paths.join(", ")}`, { paths });
		},
	});

	pi.registerTool({
		name: "verify_harness",
		label: "Verify Harness",
		description: "Run the auto_review Pi guardrail contract tests.",
		promptSnippet:
			"Use verify_harness for focused verification of Pi guardrail/tool changes.",
		parameters: Type.Object({}),
		async execute() {
			const testPath = "tests/pi_guardrails/contract_test.mjs";
			const result = await runCommand("node", [testPath]);
			if (result.status !== 0) {
				throw new Error(
					[
						`verify_harness failed: node ${testPath} exited with status ${result.status}`,
						outputTail(result.output),
					]
						.filter(Boolean)
						.join("\n"),
				);
			}
			const concise = conciseCommandResult({
				toolName: "verify_harness",
				summary: `verify_harness passed: node ${testPath}`,
				output: result.output,
			});
			return TEXT_RESULT(concise.text, { outputPath: concise.outputPath });
		},
	});

	pi.registerTool({
		name: "verify_release_tooling",
		label: "Verify Release Tooling",
		description: "Run the auto_review release tooling dry-run tests.",
		promptSnippet:
			"Use verify_release_tooling for focused verification of release workflow/tooling changes.",
		parameters: Type.Object({}),
		async execute() {
			const testPath = "tests/release_tooling_test.sh";
			const result = await runCommand("bash", [testPath]);
			if (result.status !== 0) {
				throw new Error(
					[
						`verify_release_tooling failed: bash ${testPath} exited with status ${result.status}`,
						outputTail(result.output),
					]
						.filter(Boolean)
						.join("\n"),
				);
			}
			const concise = conciseCommandResult({
				toolName: "verify_release_tooling",
				summary: `verify_release_tooling passed: bash ${testPath}`,
				output: result.output,
			});
			return TEXT_RESULT(concise.text, { outputPath: concise.outputPath });
		},
	});

	pi.registerTool({
		name: "toolchain_status",
		label: "Toolchain Status",
		description:
			"Report auto_review branch, toolchain environment, and dirty status.",
		promptSnippet:
			"Inspect branch and Nix/Rust toolchain state before edits or PR preparation.",
		parameters: Type.Object({}),
		async execute() {
			const dirty = dirtyStatus();
			return TEXT_RESULT(
				[
					`branch: ${currentBranch() ?? "unknown"}`,
					`CARGO_HOME: ${process.env.CARGO_HOME ?? "<unset>"}`,
					`RUSTUP_HOME: ${process.env.RUSTUP_HOME ?? "<unset>"}`,
					`dirty files: ${dirty.count}`,
					...dirty.preview.map((line) => `  ${line}`),
				].join("\n"),
			);
		},
	});

	pi.on("tool_call", async (event) => {
		if (isToolCallEventType("bash", event)) {
			return {
				block: true,
				reason:
					"Direct bash execution is disabled. Use an existing typed Pi tool, or add or modify a typed Pi tool for the needed capability and cover that tool with the same RGR/test/review process as production code.",
			};
		}

		if (isEditTool(event)) {
			const path = filePathFromInput(event.input);
			if (path) recordTouchedFile(path);

			if (path && currentBranch() === "main" && !mainEditOverride) {
				return {
					block: true,
					reason:
						"Branch-first gate: create/switch to a PR branch before editing, or record explicit user authorization with auto_review_allow_main_edits.",
				};
			}

			if (
				path &&
				isProductionRustPath(path) &&
				!isLikelyTestPath(path) &&
				!isNonBehavioralPath(path) &&
				!cycle?.failingOutput
			) {
				return {
					block: true,
					reason:
						"RGR gate: production Rust edits under crates/*/src require observed RED output recorded with rgr_record_red.",
				};
			}
		}

		if (isTodoTool(event) && rejectsWaterfallTodo(event.input)) {
			return {
				block: true,
				reason:
					"RGR plan gate: behavior work todo lists must name failing tests, not component-waterfall tasks.",
			};
		}

		return undefined;
	});

	pi.on("tool_result", async (event) => {
		if (!/^(rgr_|forgejo_)/i.test(event.toolName)) return undefined;
		const details = isRecord(event.details) ? { ...event.details } : {};
		return { details: { ...details, autoReviewContextPreserved: true } };
	});
}

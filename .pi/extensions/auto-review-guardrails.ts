import { execFileSync, spawnSync } from "node:child_process";
import { createHash } from "node:crypto";
import { existsSync } from "node:fs";
import {
	assertOnlyExplicitStagedPaths,
	validateExplicitPaths,
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

function runGitCommand(args: string[]): { status: number; output: string } {
	const result = spawnSync("git", args, {
		cwd: process.cwd(),
		encoding: "utf8",
		stdio: ["ignore", "pipe", "pipe"],
		maxBuffer: 10 * 1024 * 1024,
	});
	const output = [result.stdout, result.stderr].filter(Boolean).join("");
	return { status: result.status ?? 1, output };
}

function outputTail(output: string): string {
	const lines = output.trim().split(/\r?\n/).filter(Boolean);
	return lines.slice(-20).join("\n");
}

function assertGitSuccess(args: string[]): string {
	const result = runGitCommand(args);
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

function stageExplicitPaths(paths: string[]): void {
	assertGitSuccess(["add", "--", ...validateExplicitPaths(paths)]);
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
			const paths = validateExplicitPaths(params.paths);
			ensureNoPreStagedPaths();
			stageExplicitPaths(paths);
			const staged = stagedPaths();
			if (staged.length === 0) {
				throw new Error(
					"safe_commit found no staged changes after staging explicit paths.",
				);
			}
			assertOnlyExplicitStagedPaths(paths, staged);

			const commitArgs = ["commit", "-m", params.message];
			if (params.body?.trim()) commitArgs.push("-m", params.body.trim());
			const output = assertGitSuccess(commitArgs);
			ensureCleanAfterCommit();
			const head = gitOutput(["rev-parse", "--short", "HEAD"]);
			return TEXT_RESULT(
				[
					`safe_commit created ${head ?? "a commit"}.`,
					`staged paths: ${paths.join(", ")}`,
					outputTail(output),
				]
					.filter(Boolean)
					.join("\n"),
				{ commit: head, paths },
			);
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
			const push = validateSafePushInputs({
				remote: params.remote ?? "origin",
				branch: params.branch,
				currentBranch: currentBranch(),
				configuredRemotes: remotes,
				remoteUrls,
				pushUrls,
				forceWithLease: params.forceWithLease,
				justification: params.justification,
			});
			const { remote, branch } = push;

			const pushArgs = push.forceWithLease
				? ["push", "--force-with-lease", remote, `HEAD:refs/heads/${branch}`]
				: ["push", remote, branch];
			const output = assertGitSuccess(pushArgs);
			return TEXT_RESULT(
				[
					`safe_push pushed ${branch} to ${remote}.`,
					push.forceWithLease
						? `force-with-lease: ${push.justification}`
						: undefined,
					outputTail(output),
				]
					.filter(Boolean)
					.join("\n"),
				{ remote, branch, forceWithLease: push.forceWithLease },
			);
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

import { existsSync, mkdtempSync, statSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

const READ_ONLY_GIT_SUBCOMMANDS = new Set([
	"diff",
	"grep",
	"log",
	"ls-files",
	"merge-base",
	"rev-parse",
	"show",
	"status",
]);

export function conciseCommandResult({ toolName, summary, output }) {
	const dir = mkdtempSync(join(tmpdir(), `${toolName}-`));
	const outputPath = join(dir, "output.txt");
	writeFileSync(outputPath, output || "", "utf8");
	return {
		outputPath,
		text: [summary, `Full output: ${outputPath}`].filter(Boolean).join("\n"),
	};
}

export function shellWords(command) {
	const words = [];
	let current = "";
	let quote;
	let escaped = false;
	for (const char of command) {
		if (escaped) {
			current += char;
			escaped = false;
			continue;
		}
		if (char === "\\") {
			escaped = true;
			continue;
		}
		if (quote) {
			if (char === quote) quote = undefined;
			else current += char;
			continue;
		}
		if (char === "'" || char === '"') {
			quote = char;
			continue;
		}
		if (/\s/.test(char)) {
			if (current) {
				words.push(current);
				current = "";
			}
			continue;
		}
		current += char;
	}
	if (current) words.push(current);
	return words;
}

function skipGitGlobalOptionValue(token) {
	return [
		"-C",
		"-c",
		"--git-dir",
		"--work-tree",
		"--namespace",
		"--config-env",
	].includes(token);
}

function blocksAliasInjection(option, value) {
	return option === "-c" && /^alias\./.test(value ?? "");
}

export function blocksDirectGitMutationCommand(command) {
	const words = shellWords(command);
	for (let index = 0; index < words.length; index += 1) {
		if (words[index] !== "git") continue;
		let cursor = index + 1;
		while (cursor < words.length && words[cursor]?.startsWith("-")) {
			const option = words[cursor] ?? "";
			cursor += 1;
			if (skipGitGlobalOptionValue(option) && cursor < words.length) {
				if (blocksAliasInjection(option, words[cursor])) return true;
				cursor += 1;
			}
		}
		const subcommand = words[cursor] ?? "";
		if (!subcommand || !READ_ONLY_GIT_SUBCOMMANDS.has(subcommand)) return true;
	}
	return false;
}

function pathHasShellGlob(path) {
	return /[*?[\]{}]/.test(path);
}

export function validateExplicitPaths(paths) {
	const cleaned = paths.map((path) => path.trim()).filter(Boolean);
	if (!cleaned.length)
		throw new Error("safe_commit requires explicit file paths.");
	for (const path of cleaned) {
		const isDirectory = existsSync(path) && statSync(path).isDirectory();
		if (
			path === "." ||
			path === "-A" ||
			path === "-u" ||
			path.startsWith("-") ||
			path.startsWith(":") ||
			path.endsWith("/") ||
			pathHasShellGlob(path) ||
			isDirectory
		) {
			throw new Error(
				`safe_commit refuses non-explicit staging path ${JSON.stringify(path)}. Pass concrete file paths.`,
			);
		}
	}
	return cleaned;
}

export function assertOnlyExplicitStagedPaths(requestedPaths, stagedPaths) {
	const requested = new Set(
		requestedPaths.map((path) => path.replaceAll("\\", "/")),
	);
	const unexpected = stagedPaths.filter(
		(path) => !requested.has(path.replaceAll("\\", "/")),
	);
	if (unexpected.length) {
		throw new Error(
			`safe_commit refuses pre-staged or implicit paths: ${unexpected.join(", ")}`,
		);
	}
}

function validateSafeGitName(value, label) {
	const trimmed = value.trim();
	if (
		!trimmed ||
		trimmed.startsWith("-") ||
		/[\s:\\~^?*[\]]/.test(trimmed) ||
		trimmed.includes("..") ||
		trimmed.includes("@{") ||
		trimmed.startsWith("/") ||
		trimmed.endsWith("/")
	) {
		throw new Error(
			`safe_push refuses unsafe ${label} ${JSON.stringify(value)}.`,
		);
	}
	return trimmed;
}

function parseRemoteUrl(value) {
	try {
		const parsed = new URL(value);
		return {
			scheme: parsed.protocol.replace(/:$/, ""),
			host: parsed.hostname,
			path: parsed.pathname.replace(/^\/+/, ""),
		};
	} catch {
		const scpLike = /^(?:[^@\s]+@)?([^:\s]+):(.+)$/.exec(value);
		if (!scpLike) return undefined;
		return {
			scheme: "ssh",
			host: scpLike[1],
			path: scpLike[2].replace(/^\/+/, ""),
		};
	}
}

function isAllowedForgejoRemote(value) {
	const parsed = parseRemoteUrl(value);
	if (!parsed) return false;
	return (
		["https", "ssh"].includes(parsed.scheme) &&
		parsed.host === "git.johnwilger.com" &&
		/^jwilger\/auto_review(?:\.git)?$/.test(parsed.path)
	);
}

function validateSafeNonMainBranch(branch, toolName) {
	const safeBranch = validateSafeGitName(branch, "branch");
	if (safeBranch === "main") {
		throw new Error(`${toolName} refuses to target main.`);
	}
	return safeBranch;
}

function normalizedPullRequestState(pr) {
	if (pr?.merged === true || pr?.merged_at) return "merged";
	return String(pr?.state ?? "").toLowerCase();
}

function closedBranchPullRequests(branchPullRequests = []) {
	return branchPullRequests.filter((pr) =>
		["closed", "merged"].includes(normalizedPullRequestState(pr)),
	);
}

function openBranchPullRequests(branchPullRequests = []) {
	return branchPullRequests.filter(
		(pr) => normalizedPullRequestState(pr) === "open",
	);
}

export function summarizeBranchPullRequestStatus(branchPullRequests = []) {
	const open = openBranchPullRequests(branchPullRequests);
	if (!open.length) return undefined;
	return open
		.map((pr) => {
			const number = pr.number ?? pr.index ?? "?";
			const title = pr.title ? `\nTitle: ${pr.title}` : "";
			const body = pr.body ? `\nDescription: ${pr.body}` : "";
			return `Review PR #${number} title and description to ensure they cover all commits on this branch before pushing.${title}${body}`;
		})
		.join("\n\n");
}

function assertNoClosedBranchPullRequests(toolName, branchPullRequests = []) {
	const closed = closedBranchPullRequests(branchPullRequests);
	if (!closed.length) return;
	const refs = closed
		.map((pr) => `#${pr.number ?? pr.index ?? "?"} (${normalizedPullRequestState(pr)})`)
		.join(", ");
	throw new Error(
		`${toolName} refuses to use this branch because it is associated with non-open PR(s): ${refs}. Create a new branch from freshly pulled main and cherry-pick the needed commits instead.`,
	);
}

export function validateSafeCommitInputs({
	paths,
	currentBranch,
	branchPullRequests = [],
}) {
	if (!currentBranch)
		throw new Error("safe_commit could not determine the current branch.");
	if (currentBranch === "main") {
		throw new Error("safe_commit refuses to commit on main.");
	}
	assertNoClosedBranchPullRequests("safe_commit", branchPullRequests);
	return { paths: validateExplicitPaths(paths), branch: currentBranch };
}

export function validateSafeBranchCreateInputs({
	branch,
	currentBranch,
	dirtyCount,
}) {
	if (!currentBranch)
		throw new Error(
			"safe_create_branch could not determine the current branch.",
		);
	if (dirtyCount > 0) {
		throw new Error(
			"safe_create_branch refuses to create branches with a dirty working tree.",
		);
	}
	return { branch: validateSafeNonMainBranch(branch, "safe_create_branch") };
}

export function validateSafeBranchSwitchInputs({
	branch,
	currentBranch,
	dirtyCount,
}) {
	if (!currentBranch)
		throw new Error(
			"safe_switch_branch could not determine the current branch.",
		);
	if (dirtyCount > 0) {
		throw new Error(
			"safe_switch_branch refuses to switch branches with a dirty working tree.",
		);
	}
	return { branch: validateSafeNonMainBranch(branch, "safe_switch_branch") };
}

export function validateSafePushInputs({
	remote = "origin",
	branch,
	currentBranch,
	configuredRemotes,
	remoteUrls = {},
	pushUrls = {},
	forceWithLease = false,
	justification,
	branchPullRequests = [],
	prMetadataReviewed = false,
}) {
	const safeRemote = validateSafeGitName(remote, "remote");
	if (!configuredRemotes.includes(safeRemote)) {
		throw new Error(
			`safe_push refuses unknown remote ${JSON.stringify(remote)}.`,
		);
	}
	const remoteUrlValues = [remoteUrls[safeRemote] ?? ""].flat().filter(Boolean);
	const pushUrlValues = [pushUrls[safeRemote] ?? remoteUrlValues]
		.flat()
		.filter(Boolean);
	const everyDestination = [...remoteUrlValues, ...pushUrlValues];
	if (
		!everyDestination.length ||
		!everyDestination.every(isAllowedForgejoRemote)
	) {
		throw new Error(
			`safe_push refuses unexpected remote URL for ${safeRemote}.`,
		);
	}
	if (!currentBranch)
		throw new Error("safe_push could not determine the current branch.");
	if (currentBranch === "main")
		throw new Error("safe_push refuses direct pushes to main.");
	const requestedBranch = branch
		? validateSafeGitName(branch, "branch")
		: currentBranch;
	if (requestedBranch !== currentBranch) {
		throw new Error(
			"safe_push only pushes the current branch; omit branch or pass the current branch.",
		);
	}
	if (forceWithLease && !justification?.trim()) {
		throw new Error(
			"safe_push requires justification for force-with-lease pushes.",
		);
	}
	assertNoClosedBranchPullRequests("safe_push", branchPullRequests);
	const openPullRequests = openBranchPullRequests(branchPullRequests);
	if (openPullRequests.length && prMetadataReviewed !== true) {
		throw new Error(
			`safe_push requires prMetadataReviewed=true after verifying the open PR title and description cover all commits on this branch. ${summarizeBranchPullRequestStatus(openPullRequests)}`,
		);
	}
	return {
		remote: safeRemote,
		branch: currentBranch,
		forceWithLease: Boolean(forceWithLease),
		justification: justification?.trim(),
		openPullRequests: openPullRequests.map((pr) => ({
			number: pr.number ?? pr.index,
			title: pr.title,
			body: pr.body,
		})),
	};
}

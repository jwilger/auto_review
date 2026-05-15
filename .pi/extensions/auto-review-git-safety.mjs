import { existsSync, statSync } from "node:fs";

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

export function validateSafeCommitInputs({ paths, currentBranch }) {
	if (!currentBranch)
		throw new Error("safe_commit could not determine the current branch.");
	if (currentBranch === "main") {
		throw new Error("safe_commit refuses to commit on main.");
	}
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
	return {
		remote: safeRemote,
		branch: currentBranch,
		forceWithLease: Boolean(forceWithLease),
		justification: justification?.trim(),
	};
}

#!/usr/bin/env node
import assert from "node:assert/strict";
import { mkdtempSync, mkdirSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import {
	assertOnlyExplicitStagedPaths,
	blocksDirectGitMutationCommand,
	validateExplicitPaths,
	validateSafeBranchSwitchInputs,
	validateSafePushInputs,
} from "../../.pi/extensions/auto-review-git-safety.mjs";

function rejects(fn, description) {
	assert.throws(fn, Error, description);
}

assert.equal(blocksDirectGitMutationCommand("git status --short"), false);
assert.equal(blocksDirectGitMutationCommand("git diff -- flake.nix"), false);
assert.equal(blocksDirectGitMutationCommand("git add flake.nix"), true);
assert.equal(
	blocksDirectGitMutationCommand("git -C /tmp/repo add flake.nix"),
	true,
);
assert.equal(
	blocksDirectGitMutationCommand("git -c user.name=test commit -m msg"),
	true,
);
assert.equal(
	blocksDirectGitMutationCommand("git --git-dir .git push origin main"),
	true,
);
assert.equal(
	blocksDirectGitMutationCommand(
		"git -c alias.ship='!git push origin HEAD' ship",
	),
	true,
);
assert.equal(blocksDirectGitMutationCommand("git branch -D topic"), true);
assert.equal(
	blocksDirectGitMutationCommand(
		"git remote set-url origin ssh://git@evil.example/repo.git",
	),
	true,
);
assert.equal(blocksDirectGitMutationCommand("git stash"), true);

const workdir = mkdtempSync(join(tmpdir(), "auto-review-git-safety-"));
try {
	process.chdir(workdir);
	writeFileSync("file.txt", "content\n");
	mkdirSync("directory");

	assert.deepEqual(validateExplicitPaths(["file.txt"]), ["file.txt"]);
	assert.deepEqual(
		validateSafeBranchSwitchInputs({
			branch: "fix/issue-207-spawnsync-guardrails",
			currentBranch: "main",
			dirtyCount: 0,
		}),
		{ branch: "fix/issue-207-spawnsync-guardrails" },
	);
	for (const branch of [
		"main",
		"-bad",
		"bad branch",
		"../bad",
		"bad..branch",
		"bad@{branch",
		"/bad",
		"bad/",
	]) {
		rejects(
			() =>
				validateSafeBranchSwitchInputs({
					branch,
					currentBranch: "main",
					dirtyCount: 0,
				}),
			`rejects unsafe branch switch target ${branch}`,
		);
	}
	rejects(
		() =>
			validateSafeBranchSwitchInputs({
				branch: "topic",
				currentBranch: "main",
				dirtyCount: 1,
			}),
		"rejects branch switch with dirty working tree",
	);
	rejects(
		() =>
			validateSafeBranchSwitchInputs({
				branch: "topic",
				currentBranch: undefined,
				dirtyCount: 0,
			}),
		"requires current branch before branch switch",
	);
	for (const path of [
		".",
		"-A",
		"-u",
		"--all",
		"directory",
		"directory/",
		"*",
		":(glob)**/*.rs",
		":!file.txt",
		":/file.txt",
	]) {
		rejects(
			() => validateExplicitPaths([path]),
			`rejects non-explicit path ${path}`,
		);
	}

	assert.doesNotThrow(() =>
		assertOnlyExplicitStagedPaths(["file.txt"], ["file.txt"]),
	);
	rejects(
		() =>
			assertOnlyExplicitStagedPaths(["file.txt"], ["file.txt", "other.txt"]),
		"rejects pre-staged or implicit paths",
	);

	const forgejoRemote = {
		origin: ["ssh://forgejo@git.johnwilger.com:2222/jwilger/auto_review.git"],
	};
	assert.deepEqual(
		validateSafePushInputs({
			remote: "origin",
			currentBranch: "topic",
			configuredRemotes: ["origin"],
			remoteUrls: forgejoRemote,
			pushUrls: forgejoRemote,
		}),
		{
			remote: "origin",
			branch: "topic",
			forceWithLease: false,
			justification: undefined,
		},
	);
	rejects(
		() =>
			validateSafePushInputs({
				remote: "../repo",
				currentBranch: "topic",
				configuredRemotes: ["origin"],
				remoteUrls: forgejoRemote,
				pushUrls: forgejoRemote,
			}),
		"rejects path-like remote",
	);
	rejects(
		() =>
			validateSafePushInputs({
				remote: "--mirror",
				currentBranch: "topic",
				configuredRemotes: ["origin"],
				remoteUrls: forgejoRemote,
				pushUrls: forgejoRemote,
			}),
		"rejects option-like remote",
	);
	rejects(
		() =>
			validateSafePushInputs({
				remote: "backup",
				currentBranch: "topic",
				configuredRemotes: ["origin"],
				remoteUrls: forgejoRemote,
				pushUrls: forgejoRemote,
			}),
		"rejects unknown remote",
	);
	rejects(
		() =>
			validateSafePushInputs({
				remote: "origin",
				branch: "main",
				currentBranch: "topic",
				configuredRemotes: ["origin"],
				remoteUrls: forgejoRemote,
				pushUrls: forgejoRemote,
			}),
		"rejects non-current branch push",
	);
	rejects(
		() =>
			validateSafePushInputs({
				remote: "origin",
				currentBranch: "topic",
				configuredRemotes: ["origin"],
				remoteUrls: forgejoRemote,
				pushUrls: forgejoRemote,
				forceWithLease: true,
			}),
		"requires force-with-lease justification",
	);
	rejects(
		() =>
			validateSafePushInputs({
				remote: "origin",
				currentBranch: "main",
				configuredRemotes: ["origin"],
				remoteUrls: forgejoRemote,
				pushUrls: forgejoRemote,
			}),
		"rejects direct main push",
	);
	rejects(
		() =>
			validateSafePushInputs({
				remote: "origin",
				currentBranch: "topic",
				configuredRemotes: ["origin"],
				remoteUrls: { origin: ["../somewhere"] },
				pushUrls: forgejoRemote,
			}),
		"rejects unexpected remote URL",
	);
	rejects(
		() =>
			validateSafePushInputs({
				remote: "origin",
				currentBranch: "topic",
				configuredRemotes: ["origin"],
				remoteUrls: {
					origin: [
						"https://evil.example/git.johnwilger.com/jwilger/auto_review.git",
					],
				},
				pushUrls: forgejoRemote,
			}),
		"rejects evil host with allowed path suffix",
	);
	rejects(
		() =>
			validateSafePushInputs({
				remote: "origin",
				currentBranch: "topic",
				configuredRemotes: ["origin"],
				remoteUrls: forgejoRemote,
				pushUrls: {
					origin: ["ssh://git@evil.example/jwilger/auto_review.git"],
				},
			}),
		"rejects unsafe push URL even when fetch URL is safe",
	);
	rejects(
		() =>
			validateSafePushInputs({
				remote: "origin",
				currentBranch: "topic",
				configuredRemotes: ["origin"],
				remoteUrls: forgejoRemote,
				pushUrls: {
					origin: [
						"ssh://forgejo@git.johnwilger.com:2222/jwilger/auto_review.git",
						"ssh://git@evil.example/jwilger/auto_review.git",
					],
				},
			}),
		"rejects unsafe additional push URL",
	);
	for (const remoteUrl of [
		"http://git.johnwilger.com/jwilger/auto_review.git",
		"git://git.johnwilger.com/jwilger/auto_review.git",
		"file://git.johnwilger.com/jwilger/auto_review.git",
		"ftp://git.johnwilger.com/jwilger/auto_review.git",
	]) {
		rejects(
			() =>
				validateSafePushInputs({
					remote: "origin",
					currentBranch: "topic",
					configuredRemotes: ["origin"],
					remoteUrls: { origin: [remoteUrl] },
					pushUrls: forgejoRemote,
				}),
			`rejects unsafe remote scheme ${remoteUrl}`,
		);
	}
} finally {
	process.chdir("/");
	rmSync(workdir, { recursive: true, force: true });
}

console.log("pi guardrails contract tests passed");

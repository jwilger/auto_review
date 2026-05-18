import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import test from "node:test";

const AGENTS_DIR = path.join(process.cwd(), ".opencode", "agents");
const RULES_DIR = path.join(process.cwd(), ".opencode", "rules");
const POLICY_PATTERN = /project-local\s+relative\s+paths?/i;

function subagentPromptFiles() {
  return fs
    .readdirSync(AGENTS_DIR)
    .filter((file) => file.endsWith(".md"))
    .map((file) => ({
      file,
      content: fs.readFileSync(path.join(AGENTS_DIR, file), "utf8"),
    }))
    .filter((entry) => /^\s*mode:\s*subagent\b/im.test(entry.content) && /^\s*bash:\s*allow/im.test(entry.content));
}

function ruleFiles() {
  return fs
    .readdirSync(RULES_DIR)
    .filter((file) => file.endsWith(".md"))
    .map((file) => ({
      file,
      content: fs.readFileSync(path.join(RULES_DIR, file), "utf8"),
    }));
}

test("issue #249: relative path policy should live in one shared rule", () => {
  const sharedPolicyFiles = ruleFiles().filter((entry) => POLICY_PATTERN.test(entry.content));
  const duplicatedPolicyFiles = subagentPromptFiles().filter((entry) => POLICY_PATTERN.test(entry.content));

  assert.equal(
    sharedPolicyFiles.length,
    1,
    `Expected exactly one shared .opencode/rules/*.md file to state the policy, but found ${sharedPolicyFiles.length} in:\n${sharedPolicyFiles
      .map((entry) => `- ${path.join(".opencode", "rules", entry.file)}`)
      .join("\n")}`,
  );

  assert.equal(
    duplicatedPolicyFiles.length,
    0,
    `Subagent prompts should use the shared rule and avoid duplicating the policy, but found duplicates in:\n${duplicatedPolicyFiles
      .map((entry) => `- ${path.join(".opencode", "agents", entry.file)}`)
      .join("\n")}`,
  );
});

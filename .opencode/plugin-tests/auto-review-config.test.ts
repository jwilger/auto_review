import assert from "node:assert/strict";
import fs from "node:fs";
import test from "node:test";

const projectLocalPluginPaths = [
  "./.opencode/plugins/auto-review-discipline.ts",
  "./.opencode/plugins/auto-review-forgejo.ts",
  "./.opencode/plugins/auto-review-context.ts",
  "./.opencode/plugins/auto-review-toolchain.ts",
];

test("opencode config explicitly loads project-local plugins", () => {
  const config = JSON.parse(fs.readFileSync("opencode.json", "utf8"));

  assert.ok(
    Array.isArray(config.plugin),
    "opencode.json must define a plugin array so API sessions load project-local tools deterministically",
  );
  for (const pluginPath of projectLocalPluginPaths) {
    assert.ok(
      config.plugin.includes(pluginPath),
      `opencode.json must explicitly load ${pluginPath} so project-local plugin behavior is available when auto-discovery is not reflected in API tool registration`,
    );
  }
});

test("project-local plugins export the server entrypoint expected by opencode", async () => {
  for (const pluginPath of projectLocalPluginPaths) {
    const plugin = await import(new URL(`../../${pluginPath}`, import.meta.url).href);

    assert.equal(
      typeof plugin.server,
      "function",
      `${pluginPath} must export a server plugin entrypoint so opencode registers its tools/hooks`,
    );
  }
});

test("only rgr-test-reviewer is instructed to invoke RED approval", () => {
  const reviewerContract = fs.readFileSync(
    ".opencode/agents/rgr-test-reviewer.md",
    "utf8",
  );
  const nonReviewerContracts = [
    ".opencode/agents/rgr-test-author.md",
    ".opencode/agents/rgr-diagnostic-implementer.md",
    ".opencode/agents/rgr-implementation-reviewer.md",
  ].map((contractPath) => fs.readFileSync(contractPath, "utf8"));

  assert.match(
    reviewerContract,
    /\brgr_approve_red\b/,
    "rgr-test-reviewer must explicitly mention rgr_approve_red so RED approval authority is delegated to the reviewer role",
  );

  for (const contract of nonReviewerContracts) {
    assert.doesNotMatch(
      contract,
      /\bcall\s+`?rgr_approve_red`?\b/i,
      "non-reviewer RGR agents must not be instructed to call rgr_approve_red",
    );
  }
});

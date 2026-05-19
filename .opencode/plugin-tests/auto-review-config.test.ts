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

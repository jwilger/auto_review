#!/usr/bin/env bash
set -u

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=tests/release_tooling/lib.sh
source "$SCRIPT_DIR/lib.sh"
RELEASE_TOOLING_SUITE_NAME="release tooling: apply_patch edit gate changed paths"

test_apply_patch_edit_gate_checks_every_changed_path() {
	local plugin output status
	plugin="$ROOT/.opencode/plugins/auto-review-discipline.ts"

	if output="$(node - "$plugin" 2>&1 <<'JS'
const fs = require("node:fs");
const vm = require("node:vm");

const pluginPath = process.argv[2];
const source = fs.readFileSync(pluginPath, "utf8");

function functionSource(name) {
  const declaration = `function ${name}`;
  const start = source.indexOf(declaration);
  if (start === -1) throw new Error(`${name} helper is required`);
  return balancedSource(start);
}

function balancedSource(start) {
  let depth = 0;
  for (let index = source.indexOf("{", start); index < source.length; index += 1) {
    const char = source[index];
    if (char === "{") depth += 1;
    if (char === "}") depth -= 1;
    if (depth === 0) return source.slice(start, index + 1);
  }
  throw new Error("could not extract balanced source");
}

function stripTypeScript(snippet) {
  return snippet
    .replace(/\): string \| undefined/g, ")")
    .replace(/\): boolean/g, ")")
    .replace(/: unknown/g, "")
    .replace(/: string\[\]/g, "")
    .replace(/: string/g, "")
    .replace(/ as Record<string, unknown>/g, "")
    .replace(/ as string/g, "");
}

const hookMarker = '"tool.execute.before": async (input, output) =>';
const hookStart = source.indexOf(hookMarker);
if (hookStart === -1) throw new Error("tool.execute.before hook is required");
const hookSource = balancedSource(source.indexOf("async", hookStart));

const touched = [];
const context = {
  recordTouchedFile(_sessionID, path) { touched.push(path); },
  isProductionRustPath(path) { return path.startsWith("crates/") && path.includes("/src/") && path.endsWith(".rs"); },
  isLikelyTestPath() { return false; },
  isNonBehavioralPath() { return false; },
  getCycle() { return undefined; },
  setCycle() { throw new Error("setCycle should not be called before RED approval"); },
  touched,
};

vm.runInNewContext(`${stripTypeScript(functionSource("filePathFromArgs"))}\n${stripTypeScript(functionSource("changedPathsFromArgs"))}\n${stripTypeScript(functionSource("isEditTool"))}\nhook = ${hookSource};`, context);

(async () => {
  let error;
  await context.hook(
    { tool: "apply_patch", sessionID: "session-1" },
    {
      args: {
        patchText: "*** Begin Patch\n*** Update File: docs/OPERATIONS.md\n@@\n-old\n+new\n*** Update File: crates/example/src/lib.rs\n@@\n-old\n+new\n*** End Patch",
      },
    },
  ).catch((caught) => { error = caught; });

  const expectedTouched = ["docs/OPERATIONS.md", "crates/example/src/lib.rs"];
  if (JSON.stringify(touched) !== JSON.stringify(expectedTouched)) {
    throw new Error(`tool.execute.before recorded touched paths ${JSON.stringify(touched)}; expected ${JSON.stringify(expectedTouched)}`);
  }
  if (!error || !String(error.message).includes("RGR gate: production Rust edits")) {
    throw new Error(`tool.execute.before did not apply the RGR production edit gate to every apply_patch path; observed error: ${error && error.message}`);
  }
})().catch((error) => {
  console.error(error.message);
  process.exit(1);
});
JS
	)"; then
		status=0
	else
		status=$?
	fi

	if [[ $status -eq 0 ]]; then
		pass "apply_patch edit gate checks every changed path"
	else
		fail "apply_patch edit gate checks every changed path ($output)"
	fi
}

test_apply_patch_edit_gate_checks_every_changed_path

if [[ $failures -eq 0 ]]; then
	printf '%s passed\n' "$RELEASE_TOOLING_SUITE_NAME"
	exit 0
fi

printf '%s failed: %s assertion(s) failed\n' "$RELEASE_TOOLING_SUITE_NAME" "$failures"
exit 1

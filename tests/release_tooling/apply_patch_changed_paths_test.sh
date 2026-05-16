#!/usr/bin/env bash
set -u

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=tests/release_tooling/lib.sh
source "$SCRIPT_DIR/lib.sh"
RELEASE_TOOLING_SUITE_NAME="release tooling: apply_patch changed paths"


test_apply_patch_update_file_patch_text_returns_changed_path() {
	local plugin output status
	plugin="$ROOT/.opencode/plugins/auto-review-discipline.ts"

	if output="$(node - "$plugin" 2>&1 <<'JS'
const fs = require("node:fs");
const vm = require("node:vm");

const pluginPath = process.argv[2];
const source = fs.readFileSync(pluginPath, "utf8");
const declaration = "function changedPathsFromArgs";
const start = source.indexOf(declaration);
if (start === -1) {
  throw new Error("changedPathsFromArgs(args): string[] helper is required");
}

let depth = 0;
let end = -1;
for (let index = source.indexOf("{", start); index < source.length; index += 1) {
  const char = source[index];
  if (char === "{") depth += 1;
  if (char === "}") depth -= 1;
  if (depth === 0) {
    end = index + 1;
    break;
  }
}

let helperSource = source.slice(start, end)
  .replace(/: unknown/g, "")
  .replace(/: string\[\]/g, "")
  .replace(/: Array<string>/g, "")
  .replace(/ as Record<string, unknown>/g, "")
  .replace(/ as string/g, "");

const context = {};
vm.runInNewContext(`${helperSource}; result = changedPathsFromArgs({ patchText: "*** Begin Patch\\n*** Update File: crates/example/src/lib.rs\\n@@\\n-old\\n+new\\n*** End Patch" });`, context);

const expected = ["crates/example/src/lib.rs"];
if (JSON.stringify(context.result) !== JSON.stringify(expected)) {
  throw new Error(`changedPathsFromArgs returned ${JSON.stringify(context.result)} for an Update File patchText; expected ${JSON.stringify(expected)}`);
}
JS
	)"; then
		status=0
	else
		status=$?
	fi

	if [[ $status -eq 0 ]]; then
		pass "apply_patch Update File patchText returns the changed path"
	else
		fail "apply_patch Update File patchText returns the changed path ($output)"
	fi
}

test_apply_patch_update_file_patch_text_returns_changed_path

if [[ $failures -eq 0 ]]; then
	printf '%s passed\n' "$RELEASE_TOOLING_SUITE_NAME"
	exit 0
fi

printf '%s failed: %s assertion(s) failed\n' "$RELEASE_TOOLING_SUITE_NAME" "$failures"
exit 1

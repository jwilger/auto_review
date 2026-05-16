#!/usr/bin/env bash
set -u

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=tests/release_tooling/lib.sh
source "$SCRIPT_DIR/lib.sh"
RELEASE_TOOLING_SUITE_NAME="release tooling: rgr record red multiple failures"

test_rgr_record_red_rejects_red_output_with_multiple_failing_tests() {
	local shared output status
	shared="$ROOT/.opencode/plugins/lib/shared.ts"

	if output="$(node --input-type=module - "$shared" 2>&1 <<'JS'
import { readFile, writeFile, mkdtemp } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { pathToFileURL } from 'node:url';

const sharedPath = process.argv[2];
const tempDir = await mkdtemp(join(tmpdir(), 'auto-review-shared-'));
const modulePath = join(tempDir, 'shared.mjs');
let source = await readFile(sharedPath, 'utf8');

source = source
  .replace(/export type RgrStage[\s\S]*?;\n\n/, '')
  .replace(/export type RgrCycle[\s\S]*?;\n\n/, '')
  .replace(/new Map<.*>\(\)/g, 'new Map()')
  .replace(/new Set<.*>\(\)/g, 'new Set()')
  .replace(/ as Record<string, unknown>/g, '')
  .replace(/forgejoInlineReplyPayload\(comment: \{[^)]*\}\)/, 'forgejoInlineReplyPayload(comment)')
  .replace(/function (\w+)\(([^)]*)\)\s*:\s*[^ {]+(?:\s*\|\s*[^ {]+)*/g, 'function $1($2)')
  .replace(/([,(]\s*\w+)\??:\s*[^,)=]+/g, '$1')
  .replace(/const (\w+)\s*:\s*[^=]+=/g, 'const $1 =');

await writeFile(modulePath, source);
const shared = await import(pathToFileURL(modulePath).href);
const validate = shared.validateRgrRedEvidence;

if (typeof validate !== 'function') {
  throw new Error('validateRgrRedEvidence export is missing');
}

const representativeMultipleFailureOutput = `
failures:
    review::records_focused_red_only
    review::rejects_multi_failure_red
    review::rejects_multi_failure_red_from_nextest_summary
    review::rejects_multi_failure_red_from_cargo_summary
    review::rejects_multi_failure_red_with_multiple_names
    review::rejects_multi_failure_red_with_repeated_failures_header
    review::rejects_multi_failure_red_with_numbered_failures
    review::rejects_multi_failure_red_with_interleaved_logs
    review::rejects_multi_failure_red_with_doc_tests
    review::rejects_multi_failure_red_with_package_prefix
    review::rejects_multi_failure_red_with_workspace_summary

test result: FAILED. 11 failed; 0 passed; 0 ignored; finished in 0.01s
`;

try {
  validate(representativeMultipleFailureOutput);
} catch (_error) {
  process.exit(0);
}

throw new Error('validateRgrRedEvidence accepted RED output containing multiple failing tests');
JS
	)"; then
		status=0
	else
		status=$?
	fi

	if [[ $status -eq 0 ]]; then
		pass "rgr_record_red rejects multiple failing tests"
	else
		fail "rgr_record_red rejects multiple failing tests ($output)"
	fi
}

test_rgr_record_red_rejects_red_output_with_multiple_failing_tests

if [[ $failures -eq 0 ]]; then
	printf '%s passed\n' "$RELEASE_TOOLING_SUITE_NAME"
	exit 0
fi

printf '%s failed: %s assertion(s) failed\n' "$RELEASE_TOOLING_SUITE_NAME" "$failures"
exit 1

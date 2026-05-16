#!/usr/bin/env bash
set -u

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "$SCRIPT_DIR/lib.sh"

test_implementation_reviewer_routes_new_behavior_gaps_to_orchestrator() {
	local implementation_reviewer
	implementation_reviewer="$ROOT/.opencode/agents/rgr-implementation-reviewer.md"

	# This instructional guardrail is executable project policy: implementation reviewers must
	# preserve RGR ownership by routing newly discovered untested behavior back for RED authoring.
	assert_file_contains \
		"$implementation_reviewer" \
		'If you discover a behavior gap not covered by the GREEN test, route it back to the orchestrator for a new RED test; do not ask the implementer to make untested behavior changes.' \
		"implementation reviewer routes newly discovered behavior gaps to orchestrator RED instead of implementer changes"
}

run_tests test_implementation_reviewer_routes_new_behavior_gaps_to_orchestrator

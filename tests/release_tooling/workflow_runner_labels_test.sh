#!/usr/bin/env bash
set -u

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=tests/release_tooling/lib.sh
source "$SCRIPT_DIR/lib.sh"
RELEASE_TOOLING_SUITE_NAME="release tooling: workflow runner labels"

test_workflow_jobs_use_workload_specific_runner_labels() {
  local output status

  output="$(python3 - "$ROOT" <<'PY'
import pathlib
import re
import sys

root = pathlib.Path(sys.argv[1])
workflow_dir = root / '.forgejo' / 'workflows'
errors = []

expected_jobs = {
    'release-prepare.yml': {
        'release-prepare': 'docker-release',
    },
    'release-publish.yml': {
        'release-publish': 'docker-release',
    },
    'ci.yml': {
        'flake-check': 'ubuntu-24.04',
        'semantic-review': 'ubuntu-24.04',
        'pr-artifact-build': 'docker-release',
        'pr-packages': 'ubuntu-24.04',
    },
}


def job_bodies(workflow):
    return {
        match.group('name'): match.group('body')
        for match in re.finditer(
            r'(?ms)^  (?P<name>[a-zA-Z0-9_-]+):\n(?P<body>.*?)(?=^  [a-zA-Z0-9_-]+:|\Z)',
            workflow,
        )
    }


for workflow_name, expected_runner_labels in expected_jobs.items():
    workflow_path = workflow_dir / workflow_name
    if not workflow_path.exists():
        errors.append(f'{workflow_name} is missing')
        continue

    jobs = job_bodies(workflow_path.read_text())
    for job_name, expected_label in expected_runner_labels.items():
        body = jobs.get(job_name)
        if body is None:
            errors.append(f'{workflow_name} is missing the {job_name} job')
            continue
        match = re.search(r'(?m)^    runs-on:\s*(?P<label>[^\n#]+)', body)
        if not match:
            errors.append(f'{workflow_name} {job_name} must declare runs-on: {expected_label}')
            continue
        actual_label = match.group('label').strip().strip('"\'')
        if actual_label != expected_label:
            errors.append(f'{workflow_name} {job_name} must use runs-on: {expected_label} (found {actual_label})')

if (workflow_dir / 'pr-package-cleanup.yml').exists():
    errors.append('release tooling must not retain a pr-package-cleanup workflow expectation when PR images publish to the final repository and promote by digest')

for workflow_path in sorted(workflow_dir.glob('*.yml')):
    for line_number, line in enumerate(workflow_path.read_text().splitlines(), 1):
        match = re.match(r'^\s*runs-on:\s*(["\']?)(docker)\1\s*(?:#.*)?$', line)
        if match:
            errors.append(f'{workflow_path.relative_to(root)}:{line_number} must not use exact runs-on: docker')

if errors:
    print('; '.join(errors))
    sys.exit(1)
PY
)"
  status=$?
  if [[ $status -eq 0 ]]; then
    pass "workflow jobs use workload-specific runner labels"
  else
    fail "workflow jobs use workload-specific runner labels ($output)"
  fi
}

run_tests \
  test_workflow_jobs_use_workload_specific_runner_labels

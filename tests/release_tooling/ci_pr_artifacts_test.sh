#!/usr/bin/env bash
set -u

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=tests/release_tooling/lib.sh
source "$SCRIPT_DIR/lib.sh"
RELEASE_TOOLING_SUITE_NAME="release tooling: ci pr artifacts"

test_ci_workflow_publishes_release_pr_docker_and_binary_packages_updates_pr_body_and_deletes_on_merge() {
	local ci_workflow output status
	ci_workflow="$ROOT/.forgejo/workflows/ci.yml"

	output="$(
		python3 - "$ci_workflow" <<'PY'
import pathlib
import re
import sys

workflow = pathlib.Path(sys.argv[1]).read_text()
errors = []

if 'pull_request' not in workflow:
    errors.append('CI workflow must run for pull_request events so release PRs get artifacts')
pull_request_match = re.search(r'(?ms)^  pull_request:\s*\n(?P<body>.*?)(?=^  [a-zA-Z_]+:|^permissions:|^jobs:|\Z)', workflow)
pull_request_inline = re.search(r'(?m)^  pull_request:\s*\[[^\]]+\]', workflow)
if not (pull_request_match or pull_request_inline):
    errors.append('CI workflow must explicitly declare pull_request trigger types')
else:
    trigger_text = pull_request_inline.group(0) if pull_request_inline else pull_request_match.group('body')
    for action in ['opened', 'synchronize', 'reopened']:
        if action not in trigger_text:
            errors.append(f'CI workflow pull_request trigger must include {action}')
    if 'closed' in trigger_text:
        errors.append('CI workflow must not run on pull_request.closed; release PR images are promoted by digest and do not need PR package cleanup')

if re.search(r'(?m)^  push:\s*(?:\n|\[)', workflow):
    errors.append('CI workflow must not run on push; release-publish owns merged release publication')

required_pr_context = [
    'github.event.pull_request.number',
    'github.event.pull_request.head.sha',
]
for marker in required_pr_context:
    if marker not in workflow:
        errors.append(f'CI release PR artifact publishing must derive package names/tags from PR context: {marker}')

jobs = {
    match.group('name'): match.group('body')
    for match in re.finditer(r'(?ms)^  (?P<name>[a-zA-Z0-9_-]+):\n(?P<body>.*?)(?=^  [a-zA-Z0-9_-]+:|\Z)', workflow)
}

trusted_pr_patterns = [
    r"startsWith\(github\.event\.pull_request\.head\.ref, ['\"]release/v['\"]\)",
    r"github\.event\.pull_request\.head\.repo\.full_name\s*==\s*github\.repository",
    r"startsWith\(github\.event\.pull_request\.title, ['\"]chore: release v['\"]\)",
]

def step_blocks(job_body):
    return re.findall(r'(?ms)^      - (?P<step>.*?)(?=^      - |^  [a-zA-Z0-9_-]+:|\Z)', job_body)

def step_if_expression(step):
    lines = step.splitlines()
    for index, line in enumerate(lines):
        if not re.match(r'^        if\s*:', line):
            continue
        expression = line.split(':', 1)[1].strip()
        base_indent = len(line) - len(line.lstrip())
        for nested in lines[index + 1:]:
            if not nested.strip():
                continue
            indent = len(nested) - len(nested.lstrip())
            if indent <= base_indent:
                break
            expression += ' ' + nested.strip()
        return expression
    return ''

def is_heavy_or_token_step(step):
    return (
        'actions/checkout@' in step
        or 'actions/upload-artifact@' in step
        or 'actions/download-artifact@' in step
        or 'Install or reuse Nix' in step
        or re.search(r'\bnix\s+(?:develop|build|flake|run)\b', step)
        or re.search(r'\bcargo\s+', step)
        or 'skopeo copy' in step
        or re.search(r'\bcurl\b.*(?:-X\s*(?:PUT|PATCH)|--upload-file|/api/packages/jwilger/generic/|/pulls/|pulls/\$PR_NUMBER)', step, re.S)
        or 'RELEASE_PUBLISH_TOKEN: ${{ secrets.RELEASE_PUBLISH_TOKEN }}' in step
        or 'GITEA_SERVER_TOKEN: ${{ secrets.RELEASE_PUBLISH_TOKEN }}' in step
    )

def runs_for_ordinary_pr(step):
    step_if = step_if_expression(step)
    if not step_if:
        return True
    has_negation = '!' in step_if or '!=' in step_if or 'not ' in step_if.lower()
    return has_negation and any(re.search(pattern, step_if) for pattern in trusted_pr_patterns)

def has_explicit_skip_noop_marker(step):
    marker_text = []
    name_match = re.search(r'(?m)^        name:\s*(?P<name>[^\n]+)', step)
    if name_match:
        marker_text.append(name_match.group('name'))
    run_match = re.search(r'(?ms)^        run:\s*\|\s*\n(?P<body>.*?)(?=^        [a-zA-Z_-]+:|\Z)', step)
    if run_match:
        marker_text.append(run_match.group('body'))
    return re.search(r'(?i)\b(?:skip|no-op|noop|non-release)\b', '\n'.join(marker_text)) is not None

def has_informational_command_only(step):
    if re.search(r'(?m)^        uses\s*:', step):
        return False
    run_match = re.search(r'(?ms)^        run:\s*\|\s*\n(?P<body>.*?)(?=^        [a-zA-Z_-]+:|\Z)', step)
    if not run_match:
        return False
    commands = []
    for line in run_match.group('body').splitlines():
        stripped = line.strip()
        if not stripped or stripped.startswith('#'):
            continue
        commands.append(stripped)
        if not re.match(r'^(?::(?:\s|$)|echo(?:\s|$)|printf(?:\s|$))', stripped):
            return False
    return bool(commands)

def is_lightweight_ordinary_pr_noop(step):
    return (
        not is_heavy_or_token_step(step)
        and has_explicit_skip_noop_marker(step)
        and has_informational_command_only(step)
        and runs_for_ordinary_pr(step)
    )

for job_name in ['pr-artifact-build', 'pr-packages']:
    body = jobs.get(job_name)
    if body is None:
        errors.append(f'CI workflow is missing release PR artifact job: {job_name}')
        continue
    header = body.split('    steps:', 1)[0]
    if re.search(r'(?m)^    if\s*:.*startsWith\(github\.event\.pull_request\.head\.ref, [\'\"]release/v[\'\"]\)', header):
        errors.append(f'{job_name} must not be entirely skipped by a job-level release/v gate; required PR artifact status contexts must complete for ordinary PRs')
    if not any(is_lightweight_ordinary_pr_noop(step) for step in step_blocks(body)):
        errors.append(f'{job_name} must include a lightweight non-heavy no-op/skip step that runs for ordinary/non-trusted PRs so the required status context can pass without Docker/package work')

final_image = 'git.johnwilger.com/jwilger/auto_review/ar-gateway'
for forbidden_repo in [
    'git.johnwilger.com/jwilger/auto_review/ar-gateway-rc',
    'git.johnwilger.com/jwilger/auto_review/ar-gateway-pr',
    'git.johnwilger.com/jwilger/auto_review/pr-ar-gateway',
]:
    if forbidden_repo in workflow:
        errors.append(f'CI workflow must not publish release PR Docker images to a PR-only image repository: {forbidden_repo}')
if final_image + ':latest' in workflow:
    errors.append('CI workflow must not update the final latest image tag for release PR builds')

release_pr_publication_steps = [
    step.group('body')
    for step in re.finditer(r'(?ms)^      - (?P<body>.*?)(?=^      - |^  [a-zA-Z0-9_-]+:|\Z)', workflow)
    if final_image in step.group('body') and ('skopeo copy' in step.group('body') or 'Docker image:' in step.group('body') or 'digest' in step.group('body').lower())
]
release_pr_publication_text = '\n'.join(release_pr_publication_steps)
if not re.search(r'ar-gateway:release-candidate\b', release_pr_publication_text):
    errors.append('CI release PR Docker image must publish git.johnwilger.com/jwilger/auto_review/ar-gateway:release-candidate from the reviewed PR artifact')
for mutable_contract in ['Image digest:', 'CURRENT_PR_BODY', 'github.event.pull_request.body']:
    if mutable_contract in release_pr_publication_text:
        errors.append(f'CI release PR Docker publication must not depend on mutable PR body artifact strings: {mutable_contract}')

generic_package_markers = [
    '/api/packages/jwilger/generic/auto-review-release-candidate/release-candidate/',
    'tea api packages/jwilger/generic/auto-review-release-candidate/release-candidate/',
]
binary_archive_markers = ['auto-review-', 'linux-x86_64.tar.gz', 'SHA256SUMS']
if not any(marker in workflow for marker in generic_package_markers):
    errors.append('CI workflow must host reviewed release PR binary downloads at the stable release-candidate Forgejo generic package path')
for marker in binary_archive_markers:
    if marker not in workflow:
        errors.append(f'CI workflow must publish release PR binary package artifact marker: {marker}')

for forbidden_body_contract in ['Image digest:', 'CURRENT_PR_BODY: ${{ github.event.pull_request.body }}']:
    if forbidden_body_contract in workflow:
        errors.append(f'CI workflow must not make release publishing depend on PR description/body artifact strings: {forbidden_body_contract}')

cleanup_markers = ['cleanup-pr-packages:', 'Delete PR Docker and generic binary packages', 'DELETE', '-X DELETE']
for marker in cleanup_markers:
    if marker in workflow:
        errors.append(f'CI workflow must not own PR package cleanup after workflow split: {marker}')

for forbidden in ['tea release create', 'tea releases assets create', '--prerelease', 'git tag -a']:
    if forbidden in workflow:
        errors.append(f'CI release PR artifact publishing must not create Forgejo Releases or tags: {forbidden}')

if errors:
    print('; '.join(errors))
    sys.exit(1)
PY
	)"
	status=$?
	if [[ $status -eq 0 ]]; then
		pass "CI workflow publishes release PR Docker and binary packages without Forgejo Releases"
	else
		fail "CI workflow publishes release PR Docker and binary packages without Forgejo Releases ($output)"
	fi
}

test_ci_release_pr_artifact_jobs_require_trusted_release_pr_source() {
	local ci_workflow output status
	ci_workflow="$ROOT/.forgejo/workflows/ci.yml"

	output="$(
		python3 - "$ci_workflow" <<'PY'
import pathlib
import re
import sys

workflow = pathlib.Path(sys.argv[1]).read_text()
errors = []

jobs = {
    match.group('name'): match.group('body')
    for match in re.finditer(r'(?ms)^  (?P<name>[a-zA-Z0-9_-]+):\n(?P<body>.*?)(?=^  [a-zA-Z0-9_-]+:|\Z)', workflow)
}

trusted_pr_requirements = {
    "release/v branch gate": r"startsWith\(github\.event\.pull_request\.head\.ref, ['\"]release/v['\"]\)",
    "same-repository PR head gate": r"github\.event\.pull_request\.head\.repo\.full_name\s*==\s*github\.repository",
    "release PR title gate": r"startsWith\(github\.event\.pull_request\.title, ['\"]chore: release v['\"]\)",
}

def job_if_expression(job_name, body):
    header = body.split('    steps:', 1)[0]
    lines = header.splitlines()
    for index, line in enumerate(lines):
        if not re.match(r'^    if\s*:', line):
            continue
        expression = line.split(':', 1)[1].strip()
        base_indent = len(line) - len(line.lstrip())
        for nested in lines[index + 1:]:
            if not nested.strip():
                continue
            indent = len(nested) - len(nested.lstrip())
            if indent <= base_indent:
                break
            expression += ' ' + nested.strip()
        return expression
    return ''

def step_blocks(job_body):
    return re.findall(r'(?ms)^      - (?P<step>.*?)(?=^      - |^  [a-zA-Z0-9_-]+:|\Z)', job_body)

def step_if_expression(step):
    lines = step.splitlines()
    for index, line in enumerate(lines):
        if not re.match(r'^        if\s*:', line):
            continue
        expression = line.split(':', 1)[1].strip()
        base_indent = len(line) - len(line.lstrip())
        for nested in lines[index + 1:]:
            if not nested.strip():
                continue
            indent = len(nested) - len(nested.lstrip())
            if indent <= base_indent:
                break
            expression += ' ' + nested.strip()
        return expression
    return ''

def is_heavy_or_token_step(step):
    return (
        'actions/checkout@' in step
        or 'actions/upload-artifact@' in step
        or 'actions/download-artifact@' in step
        or 'Install or reuse Nix' in step
        or 'nix build' in step
        or 'nix develop' in step
        or 'skopeo copy' in step
        or 'RELEASE_PUBLISH_TOKEN: ${{ secrets.RELEASE_PUBLISH_TOKEN }}' in step
        or 'GITEA_SERVER_TOKEN: ${{ secrets.RELEASE_PUBLISH_TOKEN }}' in step
        or '/api/packages/jwilger/generic/' in step
        or 'pulls/$PR_NUMBER' in step
    )

def trusted_pr_predicate_is_conjunctive(step_if):
    if '||' in step_if or '!=' in step_if:
        return False

    def positive_match(pattern):
        match = re.search(pattern, step_if)
        if not match:
            return None
        prefix = step_if[:match.start()].rstrip()
        if prefix.endswith('!'):
            return None
        if prefix.endswith('(') and prefix[:-1].rstrip().endswith('!'):
            return None
        return match

    positions = []
    for _label, pattern in trusted_pr_requirements.items():
        match = positive_match(pattern)
        if not match:
            return False
        positions.append(match.start())
    if step_if.count('&&') < 2:
        return False
    positions.sort()
    return '&&' in step_if[positions[0]:positions[1]] and '&&' in step_if[positions[1]:positions[2]]

for job_name in ['pr-artifact-build', 'pr-packages']:
    body = jobs.get(job_name)
    if body is None:
        errors.append(f'CI workflow is missing release PR artifact job: {job_name}')
        continue

    job_if = job_if_expression(job_name, body)
    for label, pattern in trusted_pr_requirements.items():
        if re.search(pattern, job_if):
            errors.append(f'{job_name} must not enforce trusted release PR source in the job if condition; required artifact status contexts must pass for ordinary PRs: found {label}')

    heavy_steps = [step for step in step_blocks(body) if is_heavy_or_token_step(step)]
    if not heavy_steps:
        errors.append(f'{job_name} must retain release PR artifact build/publish steps')
    for step in heavy_steps:
        step_if = step_if_expression(step)
        step_label = re.search(r'(?m)^        name:\s*(?P<name>[^\n]+)', step)
        step_name = step_label.group('name').strip().strip("'\"") if step_label else '<unnamed>'
        if not trusted_pr_predicate_is_conjunctive(step_if):
            errors.append(f'{job_name} heavy/token-bearing step {step_name} must be step-level gated by a positive conjunctive trusted release PR predicate containing release/v, same-repository, and release-title checks joined with && and no negated trusted checks')

    if job_name == 'pr-packages' and 'RELEASE_PUBLISH_TOKEN: ${{ secrets.RELEASE_PUBLISH_TOKEN }}' not in body:
        errors.append('pr-packages must remain the token-bearing PR package publication job covered by the trusted source gate')

if errors:
    print('; '.join(errors))
    sys.exit(1)
PY
	)"
	status=$?
	if [[ $status -eq 0 ]]; then
		pass "CI release PR artifact jobs require trusted same-repository release PRs"
	else
		fail "CI release PR artifact jobs require trusted same-repository release PRs ($output)"
	fi
}

test_ci_workflow_has_release_pr_contexts_without_pr_package_cleanup_expectation() {
	local ci_workflow cleanup_workflow output status
	ci_workflow="$ROOT/.forgejo/workflows/ci.yml"
	cleanup_workflow="$ROOT/.forgejo/workflows/pr-package-cleanup.yml"

	output="$(
		python3 - "$ci_workflow" "$cleanup_workflow" <<'PY'
import pathlib
import re
import sys

ci = pathlib.Path(sys.argv[1]).read_text()
cleanup_path = pathlib.Path(sys.argv[2])
errors = []

required_ci_job_names = [
    'name: Verify PR with nix flake check',
    'name: Request auto_review semantic review',
    'name: Build PR artifacts (no token)',
    'name: Publish PR artifact packages',
]
for marker in required_ci_job_names:
    if marker not in ci:
        errors.append(f'CI workflow must preserve branch-protection-required status context: CI / {marker.removeprefix("name: ")} (pull_request)')

required_release_step_markers = [
    'name: Build release PR Docker image and binary package',
    'name: Skip non-release PR artifact build',
    'name: Skip non-release PR artifact publication',
]
for marker in required_release_step_markers:
    if marker not in ci:
        errors.append(f'CI workflow must keep release gating semantics at step level: {marker}')

if cleanup_path.exists():
    errors.append('release tooling tests must not expect a pr-package-cleanup workflow; release PR images are published to the final ar-gateway repository and promoted by digest')

if errors:
    print('; '.join(errors))
    sys.exit(1)
PY
	)"
	status=$?
	if [[ $status -eq 0 ]]; then
		pass "CI workflow has release PR contexts without PR package cleanup expectation"
	else
		fail "CI workflow has release PR contexts without PR package cleanup expectation ($output)"
	fi
}

test_ci_workflow_uses_runner_labels_by_job_workload() {
	local ci_workflow output status
	ci_workflow="$ROOT/.forgejo/workflows/ci.yml"

	output="$(
		python3 - "$ci_workflow" <<'PY'
import pathlib
import re
import sys

workflow = pathlib.Path(sys.argv[1]).read_text()
errors = []

jobs = {
    match.group('name'): match.group('body')
    for match in re.finditer(r'(?ms)^  (?P<name>[a-zA-Z0-9_-]+):\n(?P<body>.*?)(?=^  [a-zA-Z0-9_-]+:|\Z)', workflow)
}
expected_runner_labels = {
    'flake-check': 'ubuntu-24.04',
    'semantic-review': 'ubuntu-24.04',
    'pr-artifact-build': 'docker-release',
    'pr-packages': 'ubuntu-24.04',
}

for job_name, expected_label in expected_runner_labels.items():
    body = jobs.get(job_name)
    if body is None:
        errors.append(f'CI workflow is missing the {job_name} job')
        continue
    match = re.search(r'(?m)^    runs-on:\s*(?P<label>[^\n#]+)', body)
    if not match:
        errors.append(f'{job_name} must declare runs-on: {expected_label}')
        continue
    actual_label = match.group('label').strip().strip('"\'')
    if actual_label != expected_label:
        errors.append(f'{job_name} must use runs-on: {expected_label} (found {actual_label})')

if errors:
    print('; '.join(errors))
    sys.exit(1)
PY
	)"
	status=$?
	if [[ $status -eq 0 ]]; then
		pass "CI workflow uses runner labels by job workload"
	else
		fail "CI workflow uses runner labels by job workload ($output)"
	fi
}

test_ci_semantic_review_dispatch_is_best_effort_when_gateway_request_fails() {
	local ci_workflow output status
	ci_workflow="$ROOT/.forgejo/workflows/ci.yml"

	output="$(
		python3 - "$ci_workflow" <<'PY'
import pathlib
import re
import sys

workflow = pathlib.Path(sys.argv[1]).read_text()
errors = []

jobs = {
    match.group('name'): match.group('body')
    for match in re.finditer(r'(?ms)^  (?P<name>[a-zA-Z0-9_-]+):\n(?P<body>.*?)(?=^  [a-zA-Z0-9_-]+:|\Z)', workflow)
}

semantic_review = jobs.get('semantic-review')
if semantic_review is None:
    errors.append('CI workflow is missing the semantic-review job')
else:
    request_steps = [
        step.group('step')
        for step in re.finditer(r'(?ms)^      - (?P<step>.*?)(?=^      - |^  [a-zA-Z0-9_-]+:|\Z)', semantic_review)
        if '/reviews/ci' in step.group('step')
    ]
    if not request_steps:
        errors.append('semantic-review job must request /reviews/ci')
    for step in request_steps:
        curl_lines = [line for line in step.splitlines() if re.search(r'\bcurl\b', line) or '/reviews/ci' in line]
        curl_block = '\n'.join(curl_lines)
        job_is_non_blocking = re.search(r'(?m)^    continue-on-error:\s*true\s*$', semantic_review)
        step_is_non_blocking = re.search(r'(?m)^        continue-on-error:\s*true\s*$', step)
        curl_handles_failure = re.search(r'(?s)curl\b.*(?:\|\|\s*(?:true|echo\b|printf\b|:)\b|if\s+!\s+curl\b|curl\b.*;\s*then)', step)
        if not (job_is_non_blocking or step_is_non_blocking or curl_handles_failure):
            errors.append('semantic-review /reviews/ci curl dispatch must be best-effort/non-blocking so gateway request failures do not fail CI')
            errors.append('observed blocking curl dispatch: ' + ' '.join(part.strip() for part in curl_block.splitlines()))

if errors:
    print('; '.join(errors))
    sys.exit(1)
PY
	)"
	status=$?
	if [[ $status -eq 0 ]]; then
		pass "CI semantic review dispatch is best-effort when the gateway request fails"
	else
		fail "CI semantic review dispatch is best-effort when the gateway request fails ($output)"
	fi
}

test_ci_pr_package_publication_is_token_isolated_from_untrusted_builds() {
	local ci_workflow output status
	ci_workflow="$ROOT/.forgejo/workflows/ci.yml"

	output="$(
		python3 - "$ci_workflow" <<'PY'
import pathlib
import re
import sys

workflow = pathlib.Path(sys.argv[1]).read_text()
errors = []

jobs = {
    match.group('name'): match.group('body')
    for match in re.finditer(r'(?ms)^  (?P<name>[a-zA-Z0-9_-]+):\n(?P<body>.*?)(?=^  [a-zA-Z0-9_-]+:|\Z)', workflow)
}
if not jobs:
    print('CI workflow is missing jobs')
    sys.exit(1)

def step_blocks(job_body):
    return re.findall(r'(?ms)^      - (?P<step>.*?)(?=^      - |^  [a-zA-Z0-9_-]+:|\Z)', job_body)

def step_name(step):
    match = re.search(r'name:\s*(?P<name>[^\n]+)', step)
    return match.group('name').strip().strip('"\'') if match else '<unnamed>'

def job_needs(job_body):
    header = job_body.split('    steps:', 1)[0]
    inline = re.search(r'(?m)^    needs:\s*\[([^\]]+)\]', header)
    if inline:
        return {part.strip().strip('"\'') for part in inline.group(1).split(',') if part.strip()}
    single = re.search(r'(?m)^    needs:\s*([^\s#]+)', header)
    if single:
        return {single.group(1).strip().strip('"\'')}
    block = re.search(r'(?ms)^    needs:\s*\n(?P<body>(?:      -\s*[^\n]+\n)+)', header)
    if block:
        return {line.split('-', 1)[1].strip().strip('"\'') for line in block.group('body').splitlines() if '-' in line}
    return set()

def has_token(body):
    return 'RELEASE_PUBLISH_TOKEN: ${{ secrets.RELEASE_PUBLISH_TOKEN }}' in body or 'GITEA_SERVER_TOKEN: ${{ secrets.RELEASE_PUBLISH_TOKEN }}' in body

def evaluates_untrusted_pr(body):
    return (
        'github.event.pull_request.head.sha' in body
        or re.search(r'\bactions/checkout@v\d+\b', body)
        or re.search(r'\bnix\s+(?:develop|flake|build|run)\b', body)
        or re.search(r'\bcargo\s+(?:build|test|run|nextest|clippy)\b', body)
        or '.#ar-gateway-image' in body
        or '.#packages.x86_64-linux.default' in body
        or '.#packages.x86_64-linux.ar-cli-portable-release-root' in body
    )

def upload_artifact(body):
    return 'actions/upload-artifact' in body or 'forgejo/upload-artifact' in body

def download_artifact(body):
    return 'actions/download-artifact' in body or 'forgejo/download-artifact' in body

def publishes_or_updates_pr_artifacts(body):
    return (
        ('git.johnwilger.com/jwilger/auto_review/ar-gateway' in body or 'auto-review-pr' in body)
        and (
            re.search(r'\b(?:PUT|PATCH)\b', body)
            or 'skopeo copy' in body
            or 'pulls/$PR_NUMBER' in body
            or 'pulls/${{ github.event.pull_request.number }}' in body
        )
        and not re.search(r'\b(?:DELETE|-X DELETE|--method DELETE)\b', body)
    )

build_step_match = re.search(r'- name: Build release PR Docker image and binary package(?P<body>[\s\S]*?)(?:\n      - |\Z)', workflow)
if not build_step_match:
    errors.append('CI workflow must include a PR binary artifact build step')
else:
    build_step = build_step_match.group('body')
    x86_release_root_assignment = build_step.find(
        'x86_release_root="$(nix build .#packages.x86_64-linux.ar-cli-portable-release-root --print-out-paths --no-link)"'
    )
    x86_release_root_archive = build_step.find('tar -C "$x86_release_root"')
    if x86_release_root_assignment == -1:
        errors.append('CI PR binary artifact step must assign x86_release_root from the portable x86 release package build')
    if x86_release_root_archive == -1:
        errors.append('CI PR binary artifact step must archive from the x86_release_root path')
    if (
        x86_release_root_assignment != -1
        and x86_release_root_archive != -1
        and x86_release_root_archive < x86_release_root_assignment
    ):
        errors.append('CI PR binary artifact step must set x86_release_root before archiving from it')

build_jobs = {name: body for name, body in jobs.items() if evaluates_untrusted_pr(body) and not has_token(body)}
token_jobs = {name: body for name, body in jobs.items() if has_token(body) and publishes_or_updates_pr_artifacts(body)}

if not build_jobs:
    errors.append('CI workflow must have a no-token job that checks out/evaluates untrusted PR code and builds PR artifacts')
if not token_jobs:
    errors.append('CI workflow must have a separate token-bearing job for PR package publication/update')

for name, body in jobs.items():
    if has_token(body) and evaluates_untrusted_pr(body):
        errors.append(f'token-bearing job must not checkout PR head or run Nix/flake/dev-shell/build evaluation: {name}')

if build_jobs and token_jobs:
    for token_name, token_body in token_jobs.items():
        needs = job_needs(token_body)
        if not needs.intersection(build_jobs):
            errors.append(f'token-bearing job {token_name} must depend on the no-token untrusted build job via needs')
    if not any(upload_artifact(body) for body in build_jobs.values()):
        errors.append('untrusted PR build job must hand off only inert artifacts with upload-artifact')
    if not any(download_artifact(body) for body in token_jobs.values()):
        errors.append('token-bearing publication/update job must consume inert artifacts with download-artifact')

required_builds = ['.#ar-gateway-image', '.#packages.x86_64-linux.ar-cli-portable-release-root']
build_text = '\n'.join(build_jobs.values())
for marker in required_builds:
    if marker not in build_text:
        errors.append(f'no-token untrusted build job must build {marker} before publication')

for token_name, token_body in token_jobs.items():
    forbidden_patterns = {
        'actions checkout': r'actions/checkout@v\d+',
        'PR head checkout': r'github\.event\.pull_request\.head\.sha',
        'nix develop': r'\bnix\s+develop\b',
        'nix build': r'\bnix\s+(?:develop\s+--command\s+)?(?:nix\s+)?build\b',
        'nix flake': r'\bnix\s+flake\b',
        'nix run': r'\bnix\s+run\b',
        'cargo': r'\bcargo\s+',
        'workspace result path': r'docker-archive:\./result|pr-artifacts/',
        'workspace script path': r'(^|[\s"\'])scripts/|(^|[\s"\'])\./',
    }
    for label, pattern in forbidden_patterns.items():
        if re.search(pattern, token_body):
            errors.append(f'token-bearing job {token_name} must not use {label}; it may only publish/update from downloaded inert artifacts')
    sourced_artifacts = [
        match.group(0).strip()
        for match in re.finditer(r'(?m)^\s*(?:source|\.)\s+[^\n#;&]*pr-publication-artifacts/[^\n#;&]*', token_body)
    ]
    if sourced_artifacts:
        errors.append(f'token-bearing job {token_name} must treat downloaded artifacts as data and must not source or dot-execute them: ' + '; '.join(sourced_artifacts))
    neutralizes_loader_env = (
        re.search(r'\benv\s+-i\b', token_body)
        or re.search(r'\bunset\s+[^\n]*(?:BASH_ENV|ENV|LD_PRELOAD|LD_LIBRARY_PATH|DYLD_INSERT_LIBRARIES|PYTHONPATH|PERL5LIB|RUBYLIB|NODE_OPTIONS)', token_body)
        or re.search(r'(?:BASH_ENV|ENV|LD_PRELOAD|LD_LIBRARY_PATH|DYLD_INSERT_LIBRARIES|PYTHONPATH|PERL5LIB|RUBYLIB|NODE_OPTIONS)=', token_body)
    )
    if not neutralizes_loader_env:
        errors.append(f'token-bearing job {token_name} must neutralize inherited shell/loader environment before using RELEASE_PUBLISH_TOKEN')

if errors:
    print('; '.join(errors))
    sys.exit(1)
PY
	)"
	status=$?
	if [[ $status -eq 0 ]]; then
		pass "CI PR package publication is isolated from untrusted PR build evaluation by inert artifacts"
	else
		fail "CI PR package publication is isolated from untrusted PR build evaluation by inert artifacts ($output)"
	fi
}

test_ci_pr_package_artifact_handoff_avoids_v4_artifact_actions() {
	local ci_workflow output status
	ci_workflow="$ROOT/.forgejo/workflows/ci.yml"

	output="$(
		python3 - "$ci_workflow" <<'PY'
import pathlib
import re
import sys

workflow = pathlib.Path(sys.argv[1]).read_text()
errors = []

for action in ['upload-artifact', 'download-artifact']:
    for match in re.finditer(rf'actions/{action}@v4\b', workflow):
        line = workflow.count('\n', 0, match.start()) + 1
        errors.append(f'CI PR package artifact handoff must not use Forgejo/GHES-incompatible actions/{action}@v4 (line {line})')

if errors:
    print('; '.join(errors))
    sys.exit(1)
PY
	)"
	status=$?
	if [[ $status -eq 0 ]]; then
		pass "CI PR package artifact handoff avoids v4 artifact actions"
	else
		fail "CI PR package artifact handoff avoids v4 artifact actions ($output)"
	fi
}

test_ci_pr_package_tool_resolution_prepares_nix_before_default_profile_checks() {
	local ci_workflow output status
	ci_workflow="$ROOT/.forgejo/workflows/ci.yml"

	output="$(
		python3 - "$ci_workflow" <<'PY'
import pathlib
import re
import sys

workflow = pathlib.Path(sys.argv[1]).read_text()
errors = []

job_match = re.search(r'(?ms)^  pr-packages:\n(?P<body>.*?)(?=^  [a-zA-Z0-9_-]+:|\Z)', workflow)
if not job_match:
    print('CI workflow is missing the pr-packages job')
    sys.exit(1)
job = job_match.group('body')

resolver_match = re.search(r'(?ms)^      - name: Resolve trusted PR package tools\n(?P<body>.*?)(?=^      - |^  [a-zA-Z0-9_-]+:|\Z)', job)
if not resolver_match:
    print('pr-packages job is missing the trusted PR package tool resolver step')
    sys.exit(1)

pre_resolver = job[:resolver_match.start()]
resolver = resolver_match.group('body')
default_profile_tools = re.findall(r'/nix/var/nix/profiles/default/bin/[A-Za-z0-9._+-]+', resolver)
executable_checks = re.findall(r'\[\s+-x\s+"\$[A-Z_]+"\s+\]', resolver)
prepares_nix = re.search(
    r'(?:cachix/install-nix-action|DeterminateSystems/nix-installer-action|nix-installer|nix profile install|nix-env\s+-i|nix develop|nix shell|nix build)',
    pre_resolver,
)

if default_profile_tools and executable_checks and not prepares_nix:
    errors.append('pr-packages must install or reuse Nix before checking /nix/var/nix/profiles/default/bin trusted tools')

if errors:
    print('; '.join(errors))
    sys.exit(1)
PY
	)"
	status=$?
	if [[ $status -eq 0 ]]; then
		pass "CI PR package tool resolution prepares Nix before checking default-profile tools"
	else
		fail "CI PR package tool resolution prepares Nix before checking default-profile tools ($output)"
	fi
}

test_ci_pr_package_tool_resolution_installs_default_profile_tools_before_checks() {
	local ci_workflow output status
	ci_workflow="$ROOT/.forgejo/workflows/ci.yml"

	output="$(
		python3 - "$ci_workflow" <<'PY'
import pathlib
import re
import sys

workflow = pathlib.Path(sys.argv[1]).read_text()
errors = []

job_match = re.search(r'(?ms)^  pr-packages:\n(?P<body>.*?)(?=^  [a-zA-Z0-9_-]+:|\Z)', workflow)
if not job_match:
    print('CI workflow is missing the pr-packages job')
    sys.exit(1)
job = job_match.group('body')

resolver_match = re.search(r'(?ms)^      - name: Resolve trusted PR package tools\n(?P<body>.*?)(?=^      - |^  [a-zA-Z0-9_-]+:|\Z)', job)
if not resolver_match:
    print('pr-packages job is missing the trusted PR package tool resolver step')
    sys.exit(1)

resolver = resolver_match.group('body')
default_profile_tools = set(re.findall(r'/nix/var/nix/profiles/default/bin/([A-Za-z0-9._+-]+)', resolver))
required_tools = {'skopeo', 'jq', 'curl'}
checked_required_tools = required_tools & default_profile_tools

first_check = re.search(r'\[\s+-x\s+"\$[A-Z_]+"\s+\]', resolver)
if checked_required_tools and first_check:
    before_checks = job[:resolver_match.start()] + resolver[:first_check.start()]
    installs_profile_tools = re.search(
        r'(?:nix\s+profile\s+install|nix-env\s+-i)'
        r'(?=[\s\S]*\bskopeo\b)'
        r'(?=[\s\S]*\bjq\b)'
        r'(?=[\s\S]*\bcurl\b)',
        before_checks,
    )
    if not installs_profile_tools:
        errors.append('pr-packages must install trusted package tools (skopeo, jq, curl) into the Nix profile before checking default-profile paths')

if errors:
    print('; '.join(errors))
    sys.exit(1)
PY
	)"
	status=$?
	if [[ $status -eq 0 ]]; then
		pass "CI PR package tool resolution installs default-profile tools before checking them"
	else
		fail "CI PR package tool resolution installs default-profile tools before checking them ($output)"
	fi
}

run_tests \
	test_ci_workflow_publishes_release_pr_docker_and_binary_packages_updates_pr_body_and_deletes_on_merge \
	test_ci_release_pr_artifact_jobs_require_trusted_release_pr_source \
	test_ci_workflow_has_release_pr_contexts_without_pr_package_cleanup_expectation \
	test_ci_workflow_uses_runner_labels_by_job_workload \
	test_ci_semantic_review_dispatch_is_best_effort_when_gateway_request_fails \
	test_ci_pr_package_publication_is_token_isolated_from_untrusted_builds \
	test_ci_pr_package_artifact_handoff_avoids_v4_artifact_actions \
	test_ci_pr_package_tool_resolution_prepares_nix_before_default_profile_checks \
	test_ci_pr_package_tool_resolution_installs_default_profile_tools_before_checks

#!/usr/bin/env bash
set -u

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=tests/release_tooling/lib.sh
source "$SCRIPT_DIR/lib.sh"
RELEASE_TOOLING_SUITE_NAME="release tooling: ci pr artifacts"

test_ci_workflow_publishes_pr_docker_and_binary_packages_updates_pr_body_and_deletes_on_merge() {
  local ci_workflow output status
  ci_workflow="$ROOT/.forgejo/workflows/ci.yml"

  output="$(python3 - "$ci_workflow" <<'PY'
import pathlib
import re
import sys

workflow = pathlib.Path(sys.argv[1]).read_text()
errors = []

if 'pull_request' not in workflow:
    errors.append('CI workflow must run for pull_request events so every PR gets artifacts')
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
        errors.append('CI workflow must not run on pull_request.closed; PR package cleanup belongs in pr-package-cleanup.yml')

if re.search(r'(?m)^  push:\s*(?:\n|\[)', workflow):
    errors.append('CI workflow must not run on push; PR package cleanup belongs in pr-package-cleanup.yml')

required_pr_context = [
    'github.event.pull_request.number',
    'github.event.pull_request.head.sha',
]
for marker in required_pr_context:
    if marker not in workflow:
        errors.append(f'CI PR artifact publishing must derive package names/tags from PR context: {marker}')

final_image = 'git.johnwilger.com/jwilger/auto_review/ar-gateway'
rc_image_candidates = [
    'git.johnwilger.com/jwilger/auto_review/ar-gateway-rc',
    'git.johnwilger.com/jwilger/auto_review/ar-gateway-pr',
    'git.johnwilger.com/jwilger/auto_review/pr-ar-gateway',
]
if not any(candidate in workflow for candidate in rc_image_candidates):
    errors.append('CI workflow must publish PR Docker images under a package name distinct from final releases, such as ar-gateway-rc or ar-gateway-pr')
if re.search(r'docker://git\.johnwilger\.com/jwilger/auto_review/ar-gateway:(?:pr-|rc-|\$\{?PR_)', workflow):
    errors.append('CI workflow must not publish PR Docker images as tags under the final ar-gateway package name')
if final_image + ':latest' in workflow:
    errors.append('CI workflow must not update the final latest image tag for PR builds')

generic_package_markers = [
    '/api/packages/jwilger/generic/',
    'tea api packages/jwilger/generic/',
    'type=generic',
]
binary_archive_markers = ['auto-review-', 'linux-x86_64.tar.gz', 'SHA256SUMS']
if not any(marker in workflow for marker in generic_package_markers):
    errors.append('CI workflow must host PR binary downloads as Forgejo generic packages')
for marker in binary_archive_markers:
    if marker not in workflow:
        errors.append(f'CI workflow must publish binary package artifact marker: {marker}')

pr_body_update_markers = ['tea pr edit', 'PATCH', 'pulls/${{ github.event.pull_request.number }}', 'pulls/$PR_NUMBER']
if not any(marker in workflow for marker in pr_body_update_markers):
    errors.append('CI workflow must update the PR description with artifact links')
for marker in ['Docker image', 'binary download']:
    if marker not in workflow:
        errors.append(f'PR description update must include {marker} links')

cleanup_markers = ['cleanup-pr-packages:', 'Delete PR Docker and generic binary packages', 'DELETE', '-X DELETE']
for marker in cleanup_markers:
    if marker in workflow:
        errors.append(f'CI workflow must not own PR package cleanup after workflow split: {marker}')

for forbidden in ['tea release create', 'tea releases assets create', '--prerelease', 'git tag -a']:
    if forbidden in workflow:
        errors.append(f'CI PR artifact publishing must not create Forgejo Releases or tags: {forbidden}')

if errors:
    print('; '.join(errors))
    sys.exit(1)
PY
)"
  status=$?
  if [[ $status -eq 0 ]]; then
    pass "CI workflow publishes and cleans PR Docker and binary packages without Forgejo Releases"
  else
    fail "CI workflow publishes and cleans PR Docker and binary packages without Forgejo Releases ($output)"
  fi
}

test_ci_and_cleanup_workflows_have_clear_triggers_and_job_names() {
  local ci_workflow cleanup_workflow output status
  ci_workflow="$ROOT/.forgejo/workflows/ci.yml"
  cleanup_workflow="$ROOT/.forgejo/workflows/pr-package-cleanup.yml"

  output="$(python3 - "$ci_workflow" "$cleanup_workflow" <<'PY'
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
        errors.append(f'CI workflow must use clearer job name: {marker}')

if not cleanup_path.exists():
    errors.append('PR package cleanup workflow must exist at .forgejo/workflows/pr-package-cleanup.yml')
else:
    cleanup = cleanup_path.read_text()
    if not re.search(r'(?m)^name:\s*Clean PR packages\s*$', cleanup):
        errors.append('PR package cleanup workflow must be clearly named Clean PR packages')
    pull_request_match = re.search(r'(?ms)^  pull_request:\s*\n(?P<body>.*?)(?=^  [a-zA-Z_]+:|^permissions:|^jobs:|\Z)', cleanup)
    if not pull_request_match or 'closed' not in pull_request_match.group('body'):
        errors.append('PR package cleanup workflow must own pull_request.closed cleanup')
    if re.search(r'(?m)^  push:\s*(?:\n|\[)', cleanup):
        errors.append('PR package cleanup workflow must not run stale cleanup on push.main')
    schedule_match = re.search(r'(?ms)^  schedule:\s*\n(?P<body>.*?)(?=^  [a-zA-Z_]+:|^permissions:|^jobs:|\Z)', cleanup)
    if not schedule_match:
        errors.append('PR package cleanup workflow must run broad stale cleanup on a nightly schedule')
    elif not re.search(r'cron:\s*["\']?[^"\'\n]*\*[^"\'\n]*["\']?', schedule_match.group('body')):
        errors.append('PR package cleanup scheduled stale cleanup must declare a cron entry')
    if 'name: Delete packages for merged PRs' not in cleanup:
        errors.append('PR package cleanup job must be named Delete packages for merged PRs')

if errors:
    print('; '.join(errors))
    sys.exit(1)
PY
)"
  status=$?
  if [[ $status -eq 0 ]]; then
    pass "CI and PR cleanup workflows have clear triggers and job names"
  else
    fail "CI and PR cleanup workflows have clear triggers and job names ($output)"
  fi
}

test_ci_pr_package_publication_is_token_isolated_from_untrusted_builds() {
  local ci_workflow output status
  ci_workflow="$ROOT/.forgejo/workflows/ci.yml"

  output="$(python3 - "$ci_workflow" <<'PY'
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
    )

def upload_artifact(body):
    return 'actions/upload-artifact' in body or 'forgejo/upload-artifact' in body

def download_artifact(body):
    return 'actions/download-artifact' in body or 'forgejo/download-artifact' in body

def publishes_or_updates_pr_artifacts(body):
    return (
        ('ar-gateway-pr' in body or 'auto-review-pr' in body)
        and (
            re.search(r'\b(?:PUT|PATCH)\b', body)
            or 'skopeo copy' in body
            or 'pulls/$PR_NUMBER' in body
            or 'pulls/${{ github.event.pull_request.number }}' in body
        )
        and not re.search(r'\b(?:DELETE|-X DELETE|--method DELETE)\b', body)
    )

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

required_builds = ['.#ar-gateway-image', '.#packages.x86_64-linux.default']
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

  output="$(python3 - "$ci_workflow" <<'PY'
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

  output="$(python3 - "$ci_workflow" <<'PY'
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

  output="$(python3 - "$ci_workflow" <<'PY'
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

test_ci_pr_package_cleanup_installs_default_profile_tools_before_checks() {
  local cleanup_workflow output status
  cleanup_workflow="$ROOT/.forgejo/workflows/pr-package-cleanup.yml"

  output="$(python3 - "$cleanup_workflow" <<'PY'
import pathlib
import re
import sys

workflow_path = pathlib.Path(sys.argv[1])
if not workflow_path.exists():
    print('PR package cleanup workflow is missing at .forgejo/workflows/pr-package-cleanup.yml')
    sys.exit(1)
workflow = workflow_path.read_text()
errors = []

job_match = re.search(r'(?ms)^  cleanup-pr-packages:\n(?P<body>.*?)(?=^  [a-zA-Z0-9_-]+:|\Z)', workflow)
if not job_match:
    print('CI workflow is missing the cleanup-pr-packages job')
    sys.exit(1)
job = job_match.group('body')

resolver_match = re.search(r'(?ms)^      - name: Resolve trusted cleanup tools\n(?P<body>.*?)(?=^      - |^  [a-zA-Z0-9_-]+:|\Z)', job)
if not resolver_match:
    print('cleanup-pr-packages job is missing the trusted cleanup tool resolver step')
    sys.exit(1)

resolver = resolver_match.group('body')
default_profile_tools = set(re.findall(r'/nix/var/nix/profiles/default/bin/([A-Za-z0-9._+-]+)', resolver))
required_tools = {'jq', 'curl', 'cat'}
checked_required_tools = required_tools & default_profile_tools

first_check = re.search(r'\[\s+-x\s+"\$[A-Z_]+"\s+\]', resolver)
if checked_required_tools and first_check:
    before_checks = job[:resolver_match.start()] + resolver[:first_check.start()]
    installs_profile_tools = re.search(
        r'(?:nix\s+profile\s+install|nix-env\s+-i)'
        r'(?=[\s\S]*\bjq\b)'
        r'(?=[\s\S]*\bcurl\b)'
        r'(?=[\s\S]*\b(?:coreutils|cat)\b)',
        before_checks,
    )
    if not installs_profile_tools:
        errors.append('cleanup-pr-packages must install trusted cleanup tools (jq, curl, coreutils/cat) into the Nix profile before checking default-profile paths')

if errors:
    print('; '.join(errors))
    sys.exit(1)
PY
)"
  status=$?
  if [[ $status -eq 0 ]]; then
    pass "CI PR package cleanup installs default-profile tools before checking them"
  else
    fail "CI PR package cleanup installs default-profile tools before checking them ($output)"
  fi
}

test_ci_pr_description_update_preserves_author_body_with_managed_artifact_block() {
  local ci_workflow output status
  ci_workflow="$ROOT/.forgejo/workflows/ci.yml"

  output="$(python3 - "$ci_workflow" <<'PY'
import pathlib
import re
import sys

workflow = pathlib.Path(sys.argv[1]).read_text()
errors = []

step_match = re.search(r'(?ms)^      - name: Update PR description with package links\n(?P<body>.*?)(?=^      - |^  [a-zA-Z0-9_-]+:|\Z)', workflow)
if not step_match:
    print('CI workflow is missing the PR description artifact-link update step')
    sys.exit(1)

step = step_match.group('body')
required_markers = [
    '<!-- auto_review:artifact-links:start -->',
    '<!-- auto_review:artifact-links:end -->',
    'github.event.pull_request.body',
]
for marker in required_markers:
    if marker not in step:
        errors.append(f'PR description update must use managed artifact-links block while preserving author body: missing {marker}')

preserve_patterns = [
    r'existing_(?:body|description)',
    r'current_(?:body|description)',
    r'pull_request\.body',
    r'body_file',
]
if not any(re.search(pattern, step, re.I) for pattern in preserve_patterns):
    errors.append('PR description update must read and carry forward the existing author body')

replacement_patterns = [
    r'auto_review:artifact-links:start[\s\S]{0,1200}auto_review:artifact-links:end',
    r're\.sub\([\s\S]{0,500}auto_review:artifact-links',
    r'awk[\s\S]{0,500}auto_review:artifact-links',
]
if not any(re.search(pattern, step) for pattern in replacement_patterns):
    errors.append('PR description update must replace only the managed artifact-links block')

whole_body_replace = re.search(r'tea\s+pr\s+edit[\s\S]{0,300}--description\s+"PR artifacts for', step)
if whole_body_replace:
    errors.append('PR description update must not replace the whole author-authored body with only artifact links')

if errors:
    print('; '.join(errors))
    sys.exit(1)
PY
)"
  status=$?
  if [[ $status -eq 0 ]]; then
    pass "CI PR description update preserves author body with a managed artifact-links block"
  else
    fail "CI PR description update preserves author body with a managed artifact-links block ($output)"
  fi
}

test_ci_pr_package_cleanup_runs_nightly_and_discovers_stale_pr_versions() {
  local cleanup_workflow output status
  cleanup_workflow="$ROOT/.forgejo/workflows/pr-package-cleanup.yml"

  output="$(python3 - "$cleanup_workflow" <<'PY'
import pathlib
import re
import sys

workflow_path = pathlib.Path(sys.argv[1])
if not workflow_path.exists():
    print('PR package cleanup workflow is missing at .forgejo/workflows/pr-package-cleanup.yml')
    sys.exit(1)
workflow = workflow_path.read_text()
errors = []

if re.search(r'(?m)^  push:\s*(?:\n|\[)', workflow):
    errors.append('PR package cleanup workflow must not run stale all-merged-PR cleanup on push to main')

schedule_match = re.search(
    r'(?ms)^on:\s*\n[\s\S]*?^  schedule:\s*\n(?P<body>[\s\S]*?)(?=^  [a-zA-Z_]+:|^permissions:|^jobs:|\Z)',
    workflow,
)
if not schedule_match:
    errors.append('PR package cleanup workflow must run broad stale all-merged-PR cleanup on a nightly schedule')
elif not re.search(r'cron:\s*["\']?[^"\'\n]*\*[^"\'\n]*["\']?', schedule_match.group('body')):
    errors.append('PR package cleanup nightly stale cleanup must declare a cron entry')

jobs = {
    match.group('name'): match.group('body')
    for match in re.finditer(r'(?ms)^  (?P<name>[a-zA-Z0-9_-]+):\n(?P<body>.*?)(?=^  [a-zA-Z0-9_-]+:|\Z)', workflow)
}
if 'cleanup-pr-packages' not in jobs:
    print('; '.join(errors + ['PR package cleanup workflow is missing the cleanup-pr-packages job']))
    sys.exit(1)

broad_cleanup_jobs = {}
for name, body in jobs.items():
    header = body.split('    steps:', 1)[0]
    cleanup_related = re.search(r'cleanup|delete[\s\S]{0,80}packages|stale[\s\S]{0,80}packages', name + '\n' + body, re.I)
    if cleanup_related:
        if re.search(r'github\.event_name\s*==\s*[\'\"]push[\'\"]|github\.ref\s*==\s*[\'\"]refs/heads/main[\'\"]|GITHUB_REF|refs/heads/main', header):
            errors.append(f'cleanup job {name} must not permit stale cleanup on push to main')
        if re.search(r'github\.event_name\s*==\s*[\'\"]schedule[\'\"]|github\.event_name\s*!=\s*[\'\"]pull_request[\'\"]|github\.event\.schedule|GITHUB_EVENT_NAME', header + '\n' + body):
            broad_cleanup_jobs[name] = body

if not broad_cleanup_jobs:
    errors.append('cleanup must have a scheduled nightly stale PR package cleanup path separate from the pull_request-number cleanup path')

broad_cleanup_text = '\n'.join(broad_cleanup_jobs.values())
docker_lists_versions = any(re.search(pattern, broad_cleanup_text) for pattern in [
    r'/api/v1/packages/jwilger\?[^\s"\']*type=container[^\s"\']*[&?]q=auto_review/ar-gateway-pr',
    r'/api/v1/packages/jwilger\?[^\s"\']*q=auto_review/ar-gateway-pr[^\s"\']*[&?]type=container',
    r'list_package_versions\s+["\']container["\']\s+["\']auto_review/ar-gateway-pr["\']\s+["\']?\$container_versions_file',
])
generic_lists_versions = any(re.search(pattern, broad_cleanup_text) for pattern in [
    r'/api/v1/packages/jwilger\?[^\s"\']*type=generic[^\s"\']*[&?]q=auto-review-pr',
    r'/api/v1/packages/jwilger\?[^\s"\']*q=auto-review-pr[^\s"\']*[&?]type=generic',
    r'list_package_versions\s+["\']generic["\']\s+["\']auto-review-pr["\']\s+["\']?\$generic_versions_file',
])
reads_discovered_version_files = all(re.search(pattern, broad_cleanup_text) for pattern in [
    r'done\s*<\s*"\$container_versions_file"',
    r'done\s*<\s*"\$generic_versions_file"',
])
derives_pr_numbers_from_versions = all(re.search(pattern, broad_cleanup_text) for pattern in [
    r'pr_number="\$\{version#pr-\}"[\s\S]{0,120}pr_number="\$\{pr_number%%-\*\}"',
    r'pr_number="\$\{version%%-\*\}"',
])
discovers_pr_prefixed_versions = any(re.search(pattern, broad_cleanup_text) for pattern in [
    r'pr-\[0-9\]\+-',
    r'pr-\([0-9]\+\)-',
    r'pr-\(\[0-9\]\+\)-',
    r'pr-[\^]?\[0-9\]\+?-',
    r'pr-\d\+-',
    r'container_prefix="pr-\$\{pr_number\}-"',
    r'\$\{version#"\$container_prefix"\}',
    r'capture\([\'\"]pr-(?:\\d\+|\[0-9\]\+?|\(\?<pr_number>)',
    r'match\([\'\"]pr-(?:\\d\+|\[0-9\]\+?)',
    r'grep\s+-E[^\n]*pr-\[0-9\]\+-',
    r'sed\s+-E[^\n]*pr-\([0-9]+\)-',
])

if not (docker_lists_versions and generic_lists_versions and reads_discovered_version_files and derives_pr_numbers_from_versions and discovers_pr_prefixed_versions):
    errors.append('scheduled nightly cleanup must enumerate stale PR package versions from Forgejo REST package metadata rather than relying on github.event.pull_request.number')

if errors:
    print('; '.join(errors))
    sys.exit(1)
PY
)"
  status=$?
  if [[ $status -eq 0 ]]; then
    pass "CI PR package cleanup runs nightly and discovers stale PR package versions"
  else
    fail "CI PR package cleanup runs nightly and discovers stale PR package versions ($output)"
  fi
}

test_ci_pr_context_jobs_do_not_run_on_push_events() {
  local ci_workflow output status
  ci_workflow="$ROOT/.forgejo/workflows/ci.yml"

  output="$(python3 - "$ci_workflow" <<'PY'
import pathlib
import re
import sys

workflow = pathlib.Path(sys.argv[1]).read_text()
errors = []

jobs = {
    match.group('name'): match.group('body')
    for match in re.finditer(r'(?ms)^  (?P<name>[a-zA-Z0-9_-]+):\n(?P<body>.*?)(?=^  [a-zA-Z0-9_-]+:|\Z)', workflow)
}
for job_name in ['pr-artifact-build', 'pr-packages']:
    body = jobs.get(job_name)
    if body is None:
        errors.append(f'CI workflow is missing the {job_name} job')
    elif 'github.event.pull_request.' not in body:
        errors.append(f'{job_name} regression test expected the job to reference github.event.pull_request context')

pull_request_match = re.search(r'(?ms)^  pull_request:\s*\n(?P<body>.*?)(?=^  [a-zA-Z_]+:|^permissions:|^jobs:|\Z)', workflow)
pull_request_inline = re.search(r'(?m)^  pull_request:\s*\[[^\]]+\]', workflow)
trigger_text = pull_request_inline.group(0) if pull_request_inline else (pull_request_match.group('body') if pull_request_match else '')
for action in ['opened', 'synchronize', 'reopened']:
    if action not in trigger_text:
        errors.append(f'CI workflow trigger must include pull_request.{action} for PR-context jobs')
if 'closed' in trigger_text:
    errors.append('CI workflow trigger must exclude pull_request.closed so PR-context jobs never run on closed events')
if re.search(r'(?m)^  push:\s*(?:\n|\[)', workflow):
    errors.append('CI workflow trigger must exclude push so PR-context jobs never run on push events')

if errors:
    print('; '.join(errors))
    sys.exit(1)
PY
)"
  status=$?
  if [[ $status -eq 0 ]]; then
    pass "CI PR-context package jobs do not run on push events"
  else
    fail "CI PR-context package jobs do not run on push events ($output)"
  fi
}

test_ci_flake_check_does_not_run_on_push_events() {
  local ci_workflow output status
  ci_workflow="$ROOT/.forgejo/workflows/ci.yml"

  output="$(python3 - "$ci_workflow" <<'PY'
import pathlib
import re
import sys

workflow = pathlib.Path(sys.argv[1]).read_text()
errors = []

if not re.search(r'(?ms)^  flake-check:\n', workflow):
    print('CI workflow is missing the flake-check job')
    sys.exit(1)

pull_request_match = re.search(r'(?ms)^  pull_request:\s*\n(?P<body>.*?)(?=^  [a-zA-Z_]+:|^permissions:|^jobs:|\Z)', workflow)
pull_request_inline = re.search(r'(?m)^  pull_request:\s*\[[^\]]+\]', workflow)
trigger_text = pull_request_inline.group(0) if pull_request_inline else (pull_request_match.group('body') if pull_request_match else '')
for action in ['opened', 'synchronize', 'reopened']:
    if action not in trigger_text:
        errors.append(f'CI workflow trigger must include pull_request.{action} for flake-check')
if 'closed' in trigger_text:
    errors.append('CI workflow trigger must exclude pull_request.closed so flake-check does not rerun for cleanup-only closures')
if re.search(r'(?m)^  push:\s*(?:\n|\[)', workflow):
    errors.append('CI workflow trigger must exclude push so flake-check does not rerun for PR package cleanup')

if errors:
    print('; '.join(errors))
    sys.exit(1)
PY
)"
  status=$?
  if [[ $status -eq 0 ]]; then
    pass "CI flake-check does not run on push events"
  else
    fail "CI flake-check does not run on push events ($output)"
  fi
}

test_ci_nightly_stale_cleanup_confirms_pr_is_merged_before_delete() {
  local cleanup_workflow output status
  cleanup_workflow="$ROOT/.forgejo/workflows/pr-package-cleanup.yml"

  output="$(python3 - "$cleanup_workflow" <<'PY'
import pathlib
import re
import sys

workflow_path = pathlib.Path(sys.argv[1])
if not workflow_path.exists():
    print('PR package cleanup workflow is missing at .forgejo/workflows/pr-package-cleanup.yml')
    sys.exit(1)
workflow = workflow_path.read_text()
errors = []

jobs = {
    match.group('name'): match.group('body')
    for match in re.finditer(r'(?ms)^  (?P<name>[a-zA-Z0-9_-]+):\n(?P<body>.*?)(?=^  [a-zA-Z0-9_-]+:|\Z)', workflow)
}

broad_cleanup_jobs = {}
for name, body in jobs.items():
    header = body.split('    steps:', 1)[0]
    cleanup_related = re.search(r'cleanup|delete[\s\S]{0,80}packages|stale[\s\S]{0,80}packages', name + '\n' + body, re.I)
    if cleanup_related:
        if re.search(r'github\.event_name\s*==\s*[\'\"]push[\'\"]|github\.ref\s*==\s*[\'\"]refs/heads/main[\'\"]|GITHUB_REF|refs/heads/main', header):
            errors.append(f'cleanup job {name} must not permit stale cleanup on push to main')
        if re.search(r'github\.event_name\s*==\s*[\'\"]schedule[\'\"]|github\.event_name\s*!=\s*[\'\"]pull_request[\'\"]|github\.event\.schedule|GITHUB_EVENT_NAME', body if cleanup_related else header):
            broad_cleanup_jobs[name] = body

broad_cleanup_text = '\n'.join(broad_cleanup_jobs.values())
if not broad_cleanup_text:
    errors.append('scheduled nightly stale cleanup path must exist before open-PR deletion guard can be verified')
else:
    derives_pr_number_from_version = any(re.search(pattern, broad_cleanup_text) for pattern in [
        r'capture\([\'\"]\^?pr-(?:\\d\+|\[0-9\]\+?|\(\?<pr_number>)',
        r'match\([\'\"]\^?pr-(?:\\d\+|\[0-9\]\+?)',
        r'sed\s+-E[^\n]*pr-\([0-9]+\)-',
        r'grep\s+-Eo?[^\n]*pr-\[0-9\]\+-',
        r'pr_number=.*\$\{?version\}?',
        r'PR_NUMBER=.*\$\{?version\}?',
    ])
    queries_pr_by_number = any(re.search(pattern, broad_cleanup_text) for pattern in [
        r'/api/v1/repos/jwilger/auto_review/pulls/\$\{?[A-Za-z_][A-Za-z0-9_]*\}?',
        r'/api/v1/repos/[^\s"\']+/[^\s"\']+/pulls/\$\{?[A-Za-z_][A-Za-z0-9_]*\}?',
        r'tea\s+api[\s\S]{0,240}/pulls/\$\{?[A-Za-z_][A-Za-z0-9_]*\}?',
    ])
    confirms_merged = any(re.search(pattern, broad_cleanup_text) for pattern in [
        r'\.merged\s*==\s*true',
        r'\.merged[\s\S]{0,120}true',
        r'"merged"[\s\S]{0,120}true',
    ])
    if not (derives_pr_number_from_version and queries_pr_by_number and confirms_merged):
        errors.append('scheduled nightly stale cleanup must derive the PR number from each PR-prefixed package version and confirm that PR is merged before deleting its versions')

    pr_is_merged_match = re.search(
        r'(?ms)^\s*pr_is_merged\(\) \{(?P<body>.*?)^\s*\}\s*^\s*deletion_failures=',
        broad_cleanup_text,
    )
    if not pr_is_merged_match:
        errors.append('scheduled nightly cleanup must keep the PR merge lookup isolated in a pr_is_merged helper')
    else:
        pr_helper = pr_is_merged_match.group('body')
        if not re.search(r'pr_number="\$1"|local\s+pr_number="\$1"', pr_helper):
            errors.append('pr_is_merged must query the PR number passed by each discovered package version')
        if not re.search(r'/pulls/\$pr_number|/pulls/\$\{pr_number\}', pr_helper):
            errors.append('pr_is_merged must query Forgejo for the specific discovered PR number')
        if not re.search(r'\bjq\b|\$\{?JQ\}?', pr_helper):
            errors.append('pr_is_merged must parse the Forgejo PR response with the trusted JSON parser')
        if not re.search(r'\.merged\s*==\s*true', pr_helper):
            errors.append('pr_is_merged must require the Forgejo PR response merged field to be true')
        if not re.search(r'404\)[\s\S]{0,80}return\s+1', pr_helper):
            errors.append('pr_is_merged must reject missing PRs instead of allowing deletion')
        if not re.search(r'\*\)[\s\S]{0,180}deletion_failures=\$\(\(\s*deletion_failures\s*\+\s*1\s*\)\)[\s\S]{0,80}return\s+1', pr_helper):
            errors.append('pr_is_merged must fail closed and record lookup failures for non-2xx PR API responses')

    scheduled_branch_match = re.search(r'(?ms)if \[ "\$\{GITHUB_EVENT_NAME:-\}" = "schedule" \]; then(?P<body>.*?)^\s*else$', broad_cleanup_text)
    if not scheduled_branch_match:
        errors.append('scheduled nightly cleanup branch must be structurally distinct from pull_request cleanup')
    else:
        scheduled_branch = scheduled_branch_match.group('body')
        guarded_delete_calls = re.findall(r'pr_is_merged\s+"\$pr_number"\s+&&\s+delete_package', scheduled_branch)
        if len(guarded_delete_calls) < 2:
            errors.append('each scheduled cleanup delete path must be guarded by pr_is_merged "$pr_number" immediately before delete_package')
        for delete_line in re.findall(r'(?m)^.*delete_package .*$', scheduled_branch):
            if 'pr_is_merged "$pr_number" && delete_package' not in delete_line:
                errors.append('scheduled cleanup delete is not immediately guarded by pr_is_merged: ' + delete_line.strip())

if errors:
    print('; '.join(errors))
    sys.exit(1)
PY
)"
  status=$?
  if [[ $status -eq 0 ]]; then
    pass "CI nightly stale cleanup confirms PR is merged before deleting versions"
  else
    fail "CI nightly stale cleanup confirms PR is merged before deleting versions ($output)"
  fi
}

test_ci_nightly_stale_cleanup_matches_container_and_generic_version_schemes() {
  local cleanup_workflow output status
  cleanup_workflow="$ROOT/.forgejo/workflows/pr-package-cleanup.yml"

  output="$(python3 - "$cleanup_workflow" <<'PY'
import pathlib
import re
import sys

workflow_path = pathlib.Path(sys.argv[1])
if not workflow_path.exists():
    print('PR package cleanup workflow is missing at .forgejo/workflows/pr-package-cleanup.yml')
    sys.exit(1)
workflow = workflow_path.read_text()
errors = []

jobs = {
    match.group('name'): match.group('body')
    for match in re.finditer(r'(?ms)^  (?P<name>[a-zA-Z0-9_-]+):\n(?P<body>.*?)(?=^  [a-zA-Z0-9_-]+:|\Z)', workflow)
}

broad_cleanup_jobs = {}
for name, body in jobs.items():
    header = body.split('    steps:', 1)[0]
    cleanup_related = re.search(r'cleanup|delete[\s\S]{0,80}packages|stale[\s\S]{0,80}packages', name + '\n' + body, re.I)
    if cleanup_related:
        if re.search(r'github\.event_name\s*==\s*[\'\"]push[\'\"]|github\.ref\s*==\s*[\'\"]refs/heads/main[\'\"]|GITHUB_REF|refs/heads/main', header):
            errors.append(f'cleanup job {name} must not permit stale cleanup on push to main')
        if re.search(r'github\.event_name\s*==\s*[\'\"]schedule[\'\"]|github\.event_name\s*!=\s*[\'\"]pull_request[\'\"]|github\.event\.schedule|GITHUB_EVENT_NAME', body if cleanup_related else header):
            broad_cleanup_jobs[name] = body

broad_cleanup_text = '\n'.join(broad_cleanup_jobs.values())
if not broad_cleanup_text:
    errors.append('scheduled nightly stale cleanup path must exist before version-scheme matching can be verified')
else:
    container_fragments = [
        match.group(0)
        for match in re.finditer(r'.{0,800}ar-gateway-pr.{0,1400}', broad_cleanup_text, re.I | re.S)
    ]
    generic_fragments = [
        match.group(0)
        for match in re.finditer(r'.{0,800}auto-review-pr.{0,1400}', broad_cleanup_text, re.I | re.S)
    ]
    container_text = '\n'.join(container_fragments)
    generic_text = '\n'.join(generic_fragments)

    container_matches_published_scheme = any(re.search(pattern, container_text) for pattern in [
        r'\^pr-\[0-9\]\+-',
        r'\^pr-[^\n"\']*\[0-9\][^\n"\']*-',
        r'pr-\$\{?[A-Za-z_][A-Za-z0-9_]*\}?-',
        r'container_prefix="pr-\$\{pr_number\}-"',
        r'\$\{version#"\$container_prefix"\}',
    ])
    generic_matches_published_scheme = any(re.search(pattern, generic_text) for pattern in [
        r'\^\[0-9\]\+-',
        r'\^[^\n"\']*\[0-9\][^\n"\']*-',
        r'\$\{version#"\$pr_number-"\}',
        r'\$\{version#"\$\{pr_number\}-"\}',
        r'\$\{version#"\$PR_NUMBER-"\}',
        r'capture\([\'\"]\^?\(\?<pr_number>\[0-9\]',
        r'match\([\'\"]\^?\[0-9\]',
    ])
    generic_filters_pr_prefixed_only = bool(re.search(r'\^pr-\[0-9\]\+-|\^pr-[^\n"\']*\[0-9\][^\n"\']*-', generic_text))
    generic_derives_pr_number_without_pr_prefix = any(re.search(pattern, generic_text) for pattern in [
        r'capture\([\'\"]\^?\(\?<pr_number>\[0-9\]',
        r'match\([\'\"]\^?\[0-9\]',
        r'sed\s+-E[^\n]*\^\(\[0-9\]\+\)-',
        r'sed\s+-E[^\n]*\^\(\[0-9\]\{1,\}',
        r'cut\s+-d[\'\"]-[\'\"]\s+-f\s*1',
        r'\$\{version%%-\*\}',
    ])
    if not container_matches_published_scheme:
        errors.append('scheduled cleanup must discover container versions published as pr-<PR>-<sha>')
    if not generic_matches_published_scheme or generic_filters_pr_prefixed_only:
        errors.append('scheduled cleanup must discover generic binary versions published as <PR>-<sha>, not only pr-<PR>-<sha>')
    if not generic_derives_pr_number_without_pr_prefix:
        errors.append('scheduled cleanup must derive PR numbers from generic <PR>-<sha> versions')

if errors:
    print('; '.join(errors))
    sys.exit(1)
PY
)"
  status=$?
  if [[ $status -eq 0 ]]; then
    pass "CI nightly stale cleanup matches container and generic version schemes"
  else
    fail "CI nightly stale cleanup matches container and generic version schemes ($output)"
  fi
}

test_ci_pr_package_cleanup_uses_forgejo_rest_listing_and_parent_scope_failures() {
  local cleanup_workflow output status
  cleanup_workflow="$ROOT/.forgejo/workflows/pr-package-cleanup.yml"

  output="$(python3 - "$cleanup_workflow" <<'PY'
import pathlib
import re
import sys

workflow_path = pathlib.Path(sys.argv[1])
if not workflow_path.exists():
    print('PR package cleanup workflow is missing at .forgejo/workflows/pr-package-cleanup.yml')
    sys.exit(1)
workflow = workflow_path.read_text()
errors = []

job_match = re.search(r'(?ms)^  cleanup-pr-packages:\n(?P<body>.*?)(?=^  [a-zA-Z0-9_-]+:|\Z)', workflow)
if not job_match:
    print('CI workflow is missing the cleanup-pr-packages job')
    sys.exit(1)
job = job_match.group('body')

step_match = re.search(r'(?ms)^      - name: Delete PR Docker and generic binary packages\n(?P<body>.*?)(?=^      - |\Z)', job)
if not step_match:
    print('cleanup-pr-packages job is missing the package deletion step')
    sys.exit(1)
step = step_match.group('body')

def has(pattern, text=step):
    return re.search(pattern, text) is not None

list_helper_match = re.search(r'(?ms)^\s*list_package_versions\(\) \{(?P<body>.*?)^\s*\}', step)
list_helper = list_helper_match.group('body') if list_helper_match else ''
if not list_helper_match:
    errors.append('cleanup must keep package listing in a list_package_versions helper')

legacy_list_urls = re.findall(r'/api/packages/jwilger/(?:container|docker|generic)/[^\s"\']+/versions(?:\?[^\s"\']*)?', step)
if legacy_list_urls:
    errors.append('cleanup must not list packages with legacy /api/packages/.../versions endpoints: ' + '; '.join(sorted(set(legacy_list_urls))))

rest_list_requirements = {
    'container auto_review/ar-gateway-pr': [r'/api/v1/packages/jwilger\?[^\s"\']*type=container', r'[?&]q=auto_review/ar-gateway-pr(?:[&"\']|$)', r'[?&]limit=100(?:[&"\']|$)', r'[?&]page=\$?\{?[A-Za-z_][A-Za-z0-9_]*\}?'],
    'generic auto-review-pr': [r'/api/v1/packages/jwilger\?[^\s"\']*type=generic', r'[?&]q=auto-review-pr(?:[&"\']|$)', r'[?&]limit=100(?:[&"\']|$)', r'[?&]page=\$?\{?[A-Za-z_][A-Za-z0-9_]*\}?'],
}
for label, patterns in rest_list_requirements.items():
    if not all(has(pattern) for pattern in patterns):
        errors.append(f'cleanup must list {label} packages through /api/v1/packages/jwilger?type=<type>&q=<name>&limit=100&page=<page>')

delete_requirements = {
    'container auto_review/ar-gateway-pr': r'/api/v1/packages/jwilger/container/auto_review%2Far-gateway-pr/\$\{?version\}?',
    'generic auto-review-pr': r'/api/v1/packages/jwilger/generic/auto-review-pr/\$\{?version\}?',
}
for label, pattern in delete_requirements.items():
    if not has(pattern):
        errors.append(f'cleanup must delete {label} package versions through /api/v1/packages/jwilger/<type>/<name>/$version')

if 'deletion_failures=$((deletion_failures + 1))' in list_helper or 'deletion_failures=$(( deletion_failures + 1 ))' in list_helper:
    errors.append('list_package_versions must return failure for the parent shell to count instead of incrementing deletion_failures inside the helper')
if re.search(r'done\s*<\s*<\(\s*list_package_versions[\s\S]{0,240}\|', step):
    errors.append('cleanup must not rely on list_package_versions inside process-substitution pipelines because list failures are hidden from the parent shell')

pull_request_branch = re.search(r'(?ms)^\s*else\n(?P<body>.*?)^\s*fi\n\s*if \[ "\$deletion_failures"', step)
if pull_request_branch and not all(marker in pull_request_branch.group('body') for marker in ['pr-$PR_NUMBER-', '$PR_NUMBER-']):
    errors.append('pull_request cleanup must keep filtering listed container and generic versions by PR number before deletion')

scheduled_branch = re.search(r'(?ms)if \[ "\$\{GITHUB_EVENT_NAME:-\}" = "schedule" \]; then(?P<body>.*?)^\s*else$', step)
if scheduled_branch:
    scheduled_text = scheduled_branch.group('body')
    if len(re.findall(r'pr_is_merged\s+"\$pr_number"\s+&&\s+delete_package', scheduled_text)) < 2:
        errors.append('scheduled nightly cleanup must keep confirming each discovered PR is merged before deleting container and generic versions')
else:
    errors.append('scheduled nightly cleanup branch must remain distinct from pull_request cleanup')

if errors:
    print('; '.join(errors))
    sys.exit(1)
PY
)"
  status=$?
  if [[ $status -eq 0 ]]; then
    pass "CI PR package cleanup uses Forgejo REST listing and parent-scope list failures"
  else
    fail "CI PR package cleanup uses Forgejo REST listing and parent-scope list failures ($output)"
  fi
}

test_ci_pr_package_cleanup_targets_forgejo_container_package_name() {
  local cleanup_workflow output status
  cleanup_workflow="$ROOT/.forgejo/workflows/pr-package-cleanup.yml"

  output="$(python3 - "$cleanup_workflow" <<'PY'
import pathlib
import re
import sys

workflow_path = pathlib.Path(sys.argv[1])
if not workflow_path.exists():
    print('PR package cleanup workflow is missing at .forgejo/workflows/pr-package-cleanup.yml')
    sys.exit(1)
workflow = workflow_path.read_text()
errors = []

job_match = re.search(r'(?ms)^  cleanup-pr-packages:\n(?P<body>.*?)(?=^  [a-zA-Z0-9_-]+:|\Z)', workflow)
if not job_match:
    print('CI workflow is missing the cleanup-pr-packages job')
    sys.exit(1)
job = job_match.group('body')

step_match = re.search(r'(?ms)^      - name: Delete PR Docker and generic binary packages\n(?P<body>.*?)(?=^      - |\Z)', job)
if not step_match:
    print('cleanup-pr-packages job is missing the package deletion step')
    sys.exit(1)
step = step_match.group('body')

lists_actual_name = re.search(r'/api/v1/packages/jwilger\?[^\s"\']*type=container[^\s"\']*[&?]q=auto_review/ar-gateway-pr', step) or re.search(r'/api/v1/packages/jwilger\?[^\s"\']*q=auto_review/ar-gateway-pr[^\s"\']*[&?]type=container', step)
filters_actual_name = 'auto_review/ar-gateway-pr' in step
deletes_encoded_name = '/api/v1/packages/jwilger/container/auto_review%2Far-gateway-pr/' in step

if not (lists_actual_name and filters_actual_name and deletes_encoded_name):
    errors.append('container cleanup must list/filter Forgejo package name auto_review/ar-gateway-pr and DELETE /api/v1/packages/jwilger/container/auto_review%2Far-gateway-pr/$version, not ar-gateway-pr')

if errors:
    print('; '.join(errors))
    sys.exit(1)
PY
)"
  status=$?
  if [[ $status -eq 0 ]]; then
    pass "CI PR package cleanup targets Forgejo container package name"
  else
    fail "CI PR package cleanup targets Forgejo container package name ($output)"
  fi
}

test_ci_pr_package_cleanup_tolerates_missing_docker_and_generic_packages() {
  local cleanup_workflow output status
  cleanup_workflow="$ROOT/.forgejo/workflows/pr-package-cleanup.yml"

  output="$(python3 - "$cleanup_workflow" <<'PY'
import pathlib
import re
import sys

workflow_path = pathlib.Path(sys.argv[1])
if not workflow_path.exists():
    print('PR package cleanup workflow is missing at .forgejo/workflows/pr-package-cleanup.yml')
    sys.exit(1)
workflow = workflow_path.read_text()
errors = []

job_match = re.search(r'(?ms)^  cleanup-pr-packages:\n(?P<body>.*?)(?=^  [a-zA-Z0-9_-]+:|\Z)', workflow)
if not job_match:
    print('CI workflow is missing the cleanup-pr-packages job')
    sys.exit(1)
job = job_match.group('body')

step_match = re.search(r'(?ms)^      - name: Delete PR Docker and generic binary packages\n(?P<body>.*?)(?=^      - |\Z)', job)
if not step_match:
    print('cleanup-pr-packages job is missing the package deletion step')
    sys.exit(1)
step = step_match.group('body')
pre_cleanup_steps = job[:step_match.start()]

curl_command = r'(?:curl|"?\$\{?CURL\}?"?)'
delete_calls = re.findall(rf'(?ms)(?:{curl_command}|tea api)[^\n]*(?:DELETE|-X DELETE|--method DELETE)[\s\S]*?(?=\n\s*(?:{curl_command}|tea api)\b|\Z)', step)
if not delete_calls:
    errors.append('cleanup step must issue DELETE calls for stale PR packages')

docker_delete = any('/container/' in call or '/docker/' in call for call in delete_calls)
generic_delete = any('/generic/' in call for call in delete_calls)
if not docker_delete:
    errors.append('cleanup must attempt deletion of the PR Docker/container package')
if not generic_delete:
    errors.append('cleanup must attempt deletion of the PR generic binary package')

tolerates_missing_patterns = [
    r'404',
    r'--fail-with-body[\s\S]{0,500}(?:status|http_code)',
    r'-w\s+["\']%\{http_code\}',
    r'http_code',
    r'not[ -]?found',
]
if not any(re.search(pattern, step, re.I) for pattern in tolerates_missing_patterns):
    errors.append('cleanup must tolerate already-missing PR packages instead of failing the merge cleanup job')

if re.search(r'curl\s+-fsS\s+-X DELETE', step) and not re.search(r'404|http_code|not[ -]?found', step, re.I):
    errors.append('cleanup uses curl -f for DELETE without explicitly accepting 404 missing-package responses')

if 'PR_HEAD_SHA' in step:
    errors.append('cleanup must not limit deletion to the final PR_HEAD_SHA package version')

def has_pre_resolved_trusted_tool(tool, variable):
    trusted_assignment = any(
        assignment in pre_cleanup_steps
        for assignment in [
            f'{variable}=/nix/var/nix/profiles/default/bin/{tool}',
            f'{variable}="$TRUSTED_RELEASE_TOOLS/bin/{tool}"',
            f'{variable}="${{TRUSTED_RELEASE_TOOLS}}/bin/{tool}"',
        ]
    )
    persisted_variable = (
        re.search(rf'printf\s+["\'][^"\']*{variable}=', pre_cleanup_steps)
        and '>> "$GITHUB_ENV"' in pre_cleanup_steps
    )
    return trusted_assignment and persisted_variable

for tool, variable in [('curl', 'CURL'), ('cat', 'CAT')]:
    command_pattern = rf'(?m)(^|[|;&]\s*|\$\()\s*{tool}\b[^\n]*'
    ambient_uses = [match.group(0).strip() for match in re.finditer(command_pattern, step)]
    trusted_uses = [
        match.group(0).strip()
        for match in re.finditer(rf'(?m)(^|[|;&]\s*|\$\()\s*(?:"\$\{{?{variable}\}}"|\$\{{?{variable}\}})(?=\s|$)[^\n]*', step)
    ]
    if ambient_uses:
        errors.append(f'cleanup token-bearing step must not invoke ambient {tool}: ' + '; '.join(ambient_uses))
    if not has_pre_resolved_trusted_tool(tool, variable):
        errors.append(f'cleanup token-bearing step must pre-resolve trusted {tool} before token exposure')
    if ambient_uses or not trusted_uses:
        errors.append(f'cleanup token-bearing step must use pre-resolved trusted {tool} consistently')

version_requirements = {
    'PR Docker/container package': {
        'list': [
            r'/api/v1/packages/jwilger\?[^\s"\']*type=container[^\s"\']*[&?]q=auto_review/ar-gateway-pr',
            r'/api/v1/packages/jwilger\?[^\s"\']*q=auto_review/ar-gateway-pr[^\s"\']*[&?]type=container',
        ],
        'scope': [r'auto_review/ar-gateway-pr', r'container', r'docker'],
    },
    'PR generic binary package': {
        'list': [
            r'/api/v1/packages/jwilger\?[^\s"\']*type=generic[^\s"\']*[&?]q=auto-review-pr',
            r'/api/v1/packages/jwilger\?[^\s"\']*q=auto-review-pr[^\s"\']*[&?]type=generic',
        ],
        'scope': [r'auto-review-pr', r'generic'],
    },
}
loop_patterns = [
    r'for\s+[^\n]+\bin\s+\$\(',
    r'while\s+IFS=.*read\s+-r',
    r'jq\s+-r[\s\S]{0,240}\|\s*while\s+(?:IFS=.*)?read\s+-r',
]
pagination_patterns = [
    r'\bpage=\$?\{?[A-Za-z_][A-Za-z0-9_]*\}?',
    r'\bpage\+\+',
    r'\bpage=\$\(\(\s*page\s*\+\s*1\s*\)\)',
    r'\bwhile\b[\s\S]{0,1200}\bpage\b[\s\S]{0,1200}\blist_package_versions\b',
]
high_limit_patterns = [
    r'[?&]limit=(?:100|[2-9][0-9]{2,}|[1-9][0-9]{3,})\b',
    r'\blimit=(?:100|[2-9][0-9]{2,}|[1-9][0-9]{3,})\b',
    r'[?&]per_page=(?:100|[2-9][0-9]{2,}|[1-9][0-9]{3,})\b',
]
if not any(re.search(pattern, step) for pattern in pagination_patterns + high_limit_patterns):
    errors.append('cleanup must paginate package version listing or request an explicit high limit so deletion is not capped at the first page')

ambient_jq = [
    match.group(0).strip()
    for match in re.finditer(r'(?m)(^|[|;&]\s*)jq\s+-r[^\n]*', step)
    if not re.search(r'(/nix/var/nix/profiles/default/bin/jq|TRUSTED_RELEASE_TOOLS|trusted-release-tools/bin/jq)', match.group(0))
]
trusted_parser_patterns = [
    r'/nix/var/nix/profiles/default/bin/jq\b',
    r'\$\{?TRUSTED_RELEASE_TOOLS\}?/bin/jq\b',
    r'trusted-release-tools/bin/jq\b',
    r'python3?\s+-\s+<<[\'\"]?PY[\'\"]?[\s\S]{0,1200}\bjson\b',
]
if ambient_jq:
    errors.append('cleanup must not parse package lists with ambient unpinned jq: ' + '; '.join(ambient_jq))
if not any(re.search(pattern, step) for pattern in trusted_parser_patterns):
    errors.append('cleanup must use a trusted JSON parser/tool strategy rather than ambient runner jq')

pr_version_filter_patterns = [
    r'pr-\$PR_NUMBER(?:-|\b)',
    r'\$PR_NUMBER-[^\n]*(?:version|name)',
    r'(?:version|name)[^\n]*\$PR_NUMBER-',
    r'jq\s+-r[\s\S]{0,240}\$PR_NUMBER',
]
for package_label, requirement in version_requirements.items():
    scoped_fragments = [fragment for pattern in requirement['scope'] for fragment in re.findall(r'.{0,800}' + pattern + r'.{0,1200}', step, re.I)]
    scoped_text = '\n'.join(scoped_fragments)
    if not any(re.search(pattern, step) for pattern in requirement['list']):
        errors.append(f'cleanup must list {package_label} versions so every version for the PR can be deleted')
    if not any(re.search(pattern, scoped_text) for pattern in pr_version_filter_patterns):
        errors.append(f'cleanup must filter listed {package_label} versions by PR number before deletion')
    if not any(re.search(pattern, scoped_text) for pattern in loop_patterns):
        errors.append(f'cleanup must iterate over all matching {package_label} versions rather than issuing one fixed DELETE')

if errors:
    print('; '.join(errors))
    sys.exit(1)
PY
)"
  status=$?
  if [[ $status -eq 0 ]]; then
    pass "CI PR package cleanup tolerates missing Docker and generic packages"
  else
    fail "CI PR package cleanup tolerates missing Docker and generic packages ($output)"
  fi
}

run_tests \
  test_ci_workflow_publishes_pr_docker_and_binary_packages_updates_pr_body_and_deletes_on_merge \
  test_ci_and_cleanup_workflows_have_clear_triggers_and_job_names \
  test_ci_pr_package_publication_is_token_isolated_from_untrusted_builds \
  test_ci_pr_package_artifact_handoff_avoids_v4_artifact_actions \
  test_ci_pr_package_tool_resolution_prepares_nix_before_default_profile_checks \
  test_ci_pr_package_tool_resolution_installs_default_profile_tools_before_checks \
  test_ci_pr_package_cleanup_installs_default_profile_tools_before_checks \
  test_ci_pr_description_update_preserves_author_body_with_managed_artifact_block \
  test_ci_pr_package_cleanup_runs_nightly_and_discovers_stale_pr_versions \
  test_ci_pr_context_jobs_do_not_run_on_push_events \
  test_ci_flake_check_does_not_run_on_push_events \
  test_ci_nightly_stale_cleanup_confirms_pr_is_merged_before_delete \
  test_ci_nightly_stale_cleanup_matches_container_and_generic_version_schemes \
  test_ci_pr_package_cleanup_uses_forgejo_rest_listing_and_parent_scope_failures \
  test_ci_pr_package_cleanup_targets_forgejo_container_package_name \
  test_ci_pr_package_cleanup_tolerates_missing_docker_and_generic_packages

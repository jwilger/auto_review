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
if not re.search(r'(?ms)pull_request:\s*(?:\n\s+types:\s*\[[^\]]*(?:opened|synchronize|reopened)[^\]]*\]|\n\s+types:\s*\n(?:\s+-\s*(?:opened|synchronize|reopened)\s*\n){3})', workflow):
    errors.append('CI workflow must explicitly publish artifacts for opened, synchronize, and reopened PR events')

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

cleanup_markers = ['DELETE', '/api/packages/', 'tea api --method DELETE', 'curl -X DELETE']
if 'closed' not in workflow:
    errors.append('CI workflow must run on closed PR events to remove package-hosted PR artifacts after merge')
if "github.event.pull_request.merged == true" not in workflow:
    errors.append('CI cleanup must be gated to merged PRs')
if not any(marker in workflow for marker in cleanup_markers):
    errors.append('CI workflow must delete PR Docker and generic binary packages after the PR merges')

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

test_ci_pr_package_cleanup_tolerates_missing_docker_and_generic_packages() {
  local ci_workflow output status
  ci_workflow="$ROOT/.forgejo/workflows/ci.yml"

  output="$(python3 - "$ci_workflow" <<'PY'
import pathlib
import re
import sys

workflow = pathlib.Path(sys.argv[1]).read_text()
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
            r'/api/packages/jwilger/(?:container|docker)/[^\s"\']+/versions',
            r'tea\s+api[\s\S]{0,240}/packages/jwilger/(?:container|docker)/[^\s"\']+/versions',
        ],
        'scope': [r'ar-gateway-pr', r'container', r'docker'],
    },
    'PR generic binary package': {
        'list': [
            r'/api/packages/jwilger/generic/[^\s"\']+/versions',
            r'tea\s+api[\s\S]{0,240}/packages/jwilger/generic/[^\s"\']+/versions',
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
  test_ci_pr_package_publication_is_token_isolated_from_untrusted_builds \
  test_ci_pr_package_artifact_handoff_avoids_v4_artifact_actions \
  test_ci_pr_package_tool_resolution_prepares_nix_before_default_profile_checks \
  test_ci_pr_package_tool_resolution_installs_default_profile_tools_before_checks \
  test_ci_pr_description_update_preserves_author_body_with_managed_artifact_block \
  test_ci_pr_package_cleanup_tolerates_missing_docker_and_generic_packages

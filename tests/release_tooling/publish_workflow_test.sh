#!/usr/bin/env bash
set -u

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=tests/release_tooling/lib.sh
source "$SCRIPT_DIR/lib.sh"
RELEASE_TOOLING_SUITE_NAME="release tooling: publish workflow"

test_publish_workflow_triggers_on_main_push_or_manual_dispatch_only() {
  local publish_workflow output status
  publish_workflow="$ROOT/.forgejo/workflows/release-publish.yml"

  output="$(python3 - "$publish_workflow" <<'PY'
import pathlib
import re
import sys

workflow = pathlib.Path(sys.argv[1]).read_text()
errors = []

if re.search(r'(?m)^  pull_request:\s*(?:\n|\[)', workflow):
    errors.append('publish workflow must not trigger on pull_request.closed because it creates skipped runs for ordinary non-release PRs')

push_match = re.search(r'(?ms)^  push:\s*\n(?P<body>.*?)(?=^  [a-zA-Z_]+:|^jobs:|\Z)', workflow)
push_inline = re.search(r'(?m)^  push:\s*\[[^\]]*main[^\]]*\]', workflow)
if not (push_match or push_inline):
    errors.append('publish workflow must trigger on push to main so release publication follows the merged release commit')
elif push_match and not re.search(r'(?m)^    branches:\s*(?:\[[^\]]*\bmain\b[^\]]*\]|\n(?:      -\s*main\s*\n?)+)', push_match.group('body')):
    errors.append('publish workflow push trigger must be limited to main')

if not re.search(r'(?ms)^  workflow_dispatch:\s*\n[\s\S]*release_merge_sha:', workflow):
    errors.append('publish workflow must preserve workflow_dispatch with release_merge_sha input for manual recovery')

push_main_guard_patterns = [
    r'github\.event_name\s*==\s*[\'\"]push[\'\"][\s\S]{0,240}github\.ref\s*==\s*[\'\"]refs/heads/main[\'\"]',
    r'github\.ref\s*==\s*[\'\"]refs/heads/main[\'\"][\s\S]{0,240}github\.event_name\s*==\s*[\'\"]push[\'\"]',
]
if not any(re.search(pattern, workflow) for pattern in push_main_guard_patterns):
    errors.append('publish workflow must guard push-triggered publication to main push events')

token_index = workflow.find('RELEASE_PUBLISH_TOKEN: ${{ secrets.RELEASE_PUBLISH_TOKEN }}')
pre_token = workflow[:token_index] if token_index != -1 else workflow
release_decision_patterns = [
    r'(RELEASE_PUBLISH_APPROVED|SHOULD_PUBLISH_RELEASE|release_publish_approved|should_publish_release)[\s\S]{0,800}(GITHUB_OUTPUT|GITHUB_ENV)',
    r'(GITHUB_OUTPUT|GITHUB_ENV)[\s\S]{0,800}(RELEASE_PUBLISH_APPROVED|SHOULD_PUBLISH_RELEASE|release_publish_approved|should_publish_release)',
    r'(release[-_ ]?(?:commit|publish|guard|decision|eligible)|should[-_ ]?publish|publish[-_ ]?release)[\s\S]{0,800}(exit\s+0|false|skip|not a release)',
]
has_push_release_decision = any(re.search(pattern, pre_token, re.I) for pattern in release_decision_patterns)
has_push_release_inputs = 'github.event.before' in pre_token and 'github.sha' in pre_token
if not (has_push_release_decision and has_push_release_inputs):
    errors.append('publish workflow must make an explicit pre-token release-commit decision for push events so ordinary main pushes do not reach publish-token steps')

release_message_sources = [
    'github.event.head_commit.message',
    'RELEASE_COMMIT_MESSAGE: ${{ github.event.head_commit.message }}',
    'PUSH_COMMIT_MESSAGE: ${{ github.event.head_commit.message }}',
]
has_release_message_source = any(source in pre_token for source in release_message_sources)
pre_token_without_comments = '\n'.join(
    line for line in pre_token.splitlines()
    if not line.lstrip().startswith('#')
)
release_prefix_patterns = [
    r'startsWith\(\s*github\.event\.head_commit\.message\s*,\s*[\'\"]chore: release v[\'\"]\s*\)',
    r'\[\[\s*[\'\"]?\$\{?(?:RELEASE_COMMIT_MESSAGE|PUSH_COMMIT_MESSAGE)\}?[\'\"]?\s*==\s*[\'\"]chore: release v[\'\"]\*\s*\]\]',
    r'case\s+[\'\"]?\$\{?(?:RELEASE_COMMIT_MESSAGE|PUSH_COMMIT_MESSAGE)\}?[\'\"]?\s+in[\s\S]{0,240}[\'\"]chore: release v[\'\"]\*\)',
]
has_contains_only_check = re.search(r'contains\(\s*github\.event\.head_commit\.message\s*,\s*[\'\"]chore: release v[\'\"]\s*\)', pre_token_without_comments)
has_release_prefix_check = any(re.search(pattern, pre_token_without_comments, re.I) for pattern in release_prefix_patterns)
if not (has_release_message_source and has_release_prefix_check):
    errors.append('publish workflow push release guard must use starts-with/prefix semantics on github.event.head_commit.message (or exported env equivalent), e.g. chore: release v*, before token-bearing publication')
if has_contains_only_check:
    errors.append('publish workflow push release guard must not use contains-style matching for chore: release v; it must be a prefix check')

if errors:
    print('; '.join(errors))
    sys.exit(1)
PY
)"
  status=$?
  if [[ $status -eq 0 ]]; then
    pass "publish workflow triggers on main push or manual dispatch only"
  else
    fail "publish workflow triggers on main push or manual dispatch only ($output)"
  fi
}

test_publish_workflow_does_not_persist_push_commit_message_to_github_env() {
  local publish_workflow output status
  publish_workflow="$ROOT/.forgejo/workflows/release-publish.yml"

  output="$(python3 - "$publish_workflow" <<'PY'
import pathlib
import re
import sys

workflow = pathlib.Path(sys.argv[1]).read_text()
errors = []

token_index = workflow.find('RELEASE_PUBLISH_TOKEN: ${{ secrets.RELEASE_PUBLISH_TOKEN }}')
pre_token = workflow[:token_index] if token_index != -1 else workflow
pre_token_without_comments = '\n'.join(
    line for line in pre_token.splitlines()
    if not line.lstrip().startswith('#')
)

uses_push_head_message = 'github.event.head_commit.message' in pre_token_without_comments
if not uses_push_head_message:
    errors.append('publish workflow push path must still reference github.event.head_commit.message before token exposure')

for line_number, line in enumerate(workflow.splitlines(), start=1):
    if 'GITHUB_ENV' not in line:
        continue
    persists_push_message_value = (
        re.search(r'(?<![A-Z0-9_])(?:RELEASE_COMMIT_MESSAGE|PUSH_COMMIT_MESSAGE)=', line)
        or 'github.event.head_commit.message' in line
    )
    if persists_push_message_value:
        errors.append(f'publish workflow must not persist the potentially multiline push commit message to GITHUB_ENV: line {line_number}: {line.strip()}')

if errors:
    print('; '.join(errors))
    sys.exit(1)
PY
)"
  status=$?
  if [[ $status -eq 0 ]]; then
    pass "publish workflow does not persist push commit message to GITHUB_ENV"
  else
    fail "publish workflow does not persist push commit message to GITHUB_ENV ($output)"
  fi
}

test_publish_workflow_validates_provenance_and_changed_files_before_publish_token() {
  local publish_workflow output status
  publish_workflow="$ROOT/.forgejo/workflows/release-publish.yml"

  assert_file_contains "$publish_workflow" 'Validate release provenance and changed files' "publish workflow has a no-token provenance validation step"
  assert_file_contains "$publish_workflow" 'RELEASE_BASE_SHA: ${{ github.event.before }}' "publish workflow records the push before SHA for provenance checks"
  assert_file_contains "$publish_workflow" 'RELEASE_MERGE_SHA: ${{ github.sha }}' "publish workflow derives the release merge SHA from push event context"
  assert_file_contains "$publish_workflow" 'git diff --name-only "$RELEASE_BASE_SHA" "$RELEASE_MERGE_SHA"' "publish workflow derives changed files from the merged release PR"
  assert_file_contains "$publish_workflow" 'case "$changed_file" in' "publish workflow evaluates each changed file before publishing"
  assert_file_contains "$publish_workflow" 'Cargo.toml|Cargo.lock|CHANGELOG.md)' "publish workflow allows release metadata files before publishing"
  output="$(python3 - "$publish_workflow" <<'PY'
import fnmatch
import pathlib
import re
import sys

workflow = pathlib.Path(sys.argv[1]).read_text()
publish_marker = 'Publish Docker image to Forgejo package registry'
if publish_marker not in workflow:
    print('missing Docker image registry publish marker')
    sys.exit(1)
validation_section = workflow.split(publish_marker, 1)[0]
case_match = re.search(r'case "\$changed_file" in(?P<body>.*?)\n\s*esac', validation_section, re.S)
if not case_match:
    print('missing changed-file case allowlist before publish')
    sys.exit(1)

patterns = []
case_lines = case_match.group('body').splitlines()
index = 0
while index < len(case_lines):
    stripped = case_lines[index].strip()
    index += 1
    if not stripped or stripped.startswith('#') or ')' not in stripped:
        continue

    head = stripped.split(')', 1)[0]
    arm_body = []
    while index < len(case_lines):
        body_line = case_lines[index].strip()
        arm_body.append(body_line)
        index += 1
        if body_line == ';;':
            break

    meaningful_body = [line for line in arm_body if line and not line.startswith('#')]
    is_pass_through = meaningful_body == [';;']
    is_reject = any('exit' in line or 'refusing token-bearing publish' in line for line in meaningful_body)
    is_default = head.strip() == '*'
    if not is_pass_through or is_reject or is_default:
        continue

    if any(token in head for token in ['*', '/', '.toml', '.md', '.lock']):
        patterns.extend(part for part in head.split('|') if part)

required = {
    'Cargo.toml': False,
    'Cargo.lock': False,
    'CHANGELOG.md': False,
}

for sample in required:
    required[sample] = any(fnmatch.fnmatchcase(sample, pattern) for pattern in patterns)

missing = [sample for sample, allowed in required.items() if not allowed]
if missing:
    print('publish allowlist does not permit root release metadata needed for image publication: ' + ', '.join(missing))
    print('observed allowlist patterns: ' + ', '.join(patterns))
    sys.exit(1)
forbidden = ['crates/ar-cli/Cargo.toml', 'crates/ar-cli/CHANGELOG.md']
permitted_forbidden = [sample for sample in forbidden if any(fnmatch.fnmatchcase(sample, pattern) for pattern in patterns)]
if permitted_forbidden:
    print('publish allowlist permits crate package metadata even though publishing is image-based: ' + ', '.join(permitted_forbidden))
    print('observed allowlist patterns: ' + ', '.join(patterns))
    sys.exit(1)
PY
  )"
  status=$?
  if [[ $status -eq 0 ]]; then
    pass "publish workflow allows only root release metadata before image publishing"
  else
    fail "publish workflow allows only root release metadata before image publishing ($output)"
  fi
  assert_file_contains "$publish_workflow" '.forgejo/workflows/*|scripts/*)' "publish workflow explicitly rejects script and workflow changes before publishing"
  assert_file_contains "$publish_workflow" 'refusing token-bearing publish for release PR file:' "publish workflow fails closed for unexpected release PR files"
  assert_file_contains_before "$publish_workflow" 'git diff --name-only "$RELEASE_BASE_SHA" "$RELEASE_MERGE_SHA"' 'Publish Docker image to Forgejo package registry' "publish workflow validates changed files before publishing the image with the publish token"
  assert_file_contains_before "$publish_workflow" '.forgejo/workflows/*|scripts/*)' 'Publish Docker image to Forgejo package registry' "publish workflow rejects script and workflow changes before publishing the image with the publish token"
}

test_publish_workflow_uses_push_sha_without_dispatch_inputs_on_push_path() {
  local publish_workflow output status
  publish_workflow="$ROOT/.forgejo/workflows/release-publish.yml"

  output="$(python3 - "$publish_workflow" <<'PY'
import pathlib
import re
import sys

workflow = pathlib.Path(sys.argv[1]).read_text()
errors = []

push_context_candidates = []
for step_match in re.finditer(r'(?ms)^      - (?P<body>.*?)(?=^      - |\Z)', workflow):
    step = step_match.group('body')
    if 'github.sha' in step or 'github.event.before' in step or "github.event_name == 'push'" in step or 'github.event_name == "push"' in step:
        push_context_candidates.append(step)

push_context = '\n'.join(push_context_candidates)
if not push_context:
    errors.append('publish workflow must derive release context from push event data for push-to-main releases')
else:
    if "github.event_name == 'push'" not in push_context and 'github.event_name == "push"' not in push_context:
        errors.append('pushed release context must be gated to push events')
    if 'RELEASE_MERGE_SHA: ${{ github.sha }}' not in push_context:
        errors.append('pushed release context must derive RELEASE_MERGE_SHA from github.sha')
    if 'RELEASE_BASE_SHA: ${{ github.event.before }}' not in push_context:
        errors.append('pushed release context must derive RELEASE_BASE_SHA from github.event.before')
    if 'inputs.release_merge_sha' in push_context:
        errors.append('pushed release context must not read inputs.release_merge_sha')

for step_match in re.finditer(r'(?ms)^      - (?P<body>.*?)(?=^      - |\Z)', workflow):
    step = step_match.group('body')
    if 'inputs.release_merge_sha' not in step:
        continue
    gated_to_dispatch = "github.event_name == 'workflow_dispatch'" in step or 'github.event_name == "workflow_dispatch"' in step
    if not gated_to_dispatch:
        first_line = step.strip().splitlines()[0] if step.strip() else '<unnamed step>'
        errors.append(f'non-dispatch publish path must not use inputs.release_merge_sha: {first_line}')

validation_marker = 'Validate release provenance and changed files'
if validation_marker not in workflow:
    errors.append('publish workflow must validate provenance before token-bearing publish')
else:
    validation_section = workflow.split(validation_marker, 1)[1].split('Publish Docker image to Forgejo package registry', 1)[0]
    if 'RELEASE_MERGE_SHA' not in validation_section or 'git rev-parse HEAD' not in validation_section:
        errors.append('publish workflow must validate the checked-out push/dispatch commit against RELEASE_MERGE_SHA before token-bearing publish')

if errors:
    print('; '.join(errors))
    sys.exit(1)
PY
)"
  status=$?
  if [[ $status -eq 0 ]]; then
    pass "publish workflow uses push SHA without dispatch inputs on push path"
  else
    fail "publish workflow uses push SHA without dispatch inputs on push path ($output)"
  fi
}

test_publish_workflow_uses_release_pr_merge_sha_not_a_recomputed_version() {
  local publish_workflow output status
  publish_workflow="$ROOT/.forgejo/workflows/release-publish.yml"

  assert_file_not_contains "$publish_workflow" 'RELEASE_VERSION="${FORGEJO_PULL_REQUEST_HEAD_BRANCH#release/v}"' "publish workflow does not derive a release version from a hand-managed branch"
  assert_file_not_contains "$publish_workflow" '[[ "$RELEASE_VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]' "publish workflow does not recompute release versions"
  assert_file_not_contains "$publish_workflow" 'release-plz' "publish workflow does not use release-plz"
  assert_file_contains "$publish_workflow" 'git.johnwilger.com/jwilger/auto_review/ar-gateway' "publish workflow publishes the application Docker image rather than workspace crates"
  output="$(python3 - "$publish_workflow" <<'PY'
import pathlib
import re
import sys

workflow = pathlib.Path(sys.argv[1]).read_text()
errors = []
for forbidden in [
    'FORGEJO_PULL_REQUEST_HEAD_BRANCH#release/v',
    'scripts/release plan',
    'cargo semver-checks',
]:
    if forbidden in workflow:
        errors.append(f'publish workflow must not derive RELEASE_VERSION from branch names or planning: {forbidden}')

trusted_metadata_patterns = [
    r'RELEASE_VERSION=.*Cargo\.toml',
    r'Cargo\.toml.*RELEASE_VERSION=',
    r'workspace_version\s+Cargo\.toml',
    r'pathlib\.Path\(["\']Cargo\.toml["\']\).*version',
]
if not any(re.search(pattern, workflow) for pattern in trusted_metadata_patterns):
    errors.append('publish workflow must derive RELEASE_VERSION from trusted Cargo.toml at the checked-out merge commit')

publish_marker = 'Publish Docker image to Forgejo package registry'
if publish_marker in workflow:
    before_publish = workflow.split(publish_marker, 1)[0]
    if 'Cargo.toml' not in before_publish or 'RELEASE_VERSION' not in before_publish:
        errors.append('publish workflow must derive RELEASE_VERSION before token-bearing publication begins')
else:
    errors.append('publish workflow is missing the token-bearing publication step marker')

if errors:
    print('; '.join(errors))
    sys.exit(1)
sys.exit(0)
PY
)"
  status=$?
  if [[ $status -eq 0 ]]; then
    pass "publish workflow derives release version from checked-out Cargo.toml metadata"
  else
    fail "publish workflow derives release version from checked-out Cargo.toml metadata ($output)"
  fi
}

test_publish_workflow_executes_from_merge_commit_sha_before_publish_token() {
  local publish_workflow output status
  publish_workflow="$ROOT/.forgejo/workflows/release-publish.yml"

  output="$(python3 - "$publish_workflow" <<'PY'
import pathlib
import sys

workflow = pathlib.Path(sys.argv[1]).read_text().splitlines()
for index, line in enumerate(workflow):
    if line.strip() == "- uses: actions/checkout@v4":
        with_index = None
        for nested in workflow[index + 1:]:
            if nested.startswith("      - "):
                break
            if nested.strip() == "with:":
                with_index = workflow.index(nested, index + 1)
                break
        if with_index is None:
            print("actions/checkout@v4 step is missing a with mapping")
            sys.exit(1)

        with_indent = len(workflow[with_index]) - len(workflow[with_index].lstrip())
        required_checkout_settings = {
            "fetch-depth: 0": False,
            "persist-credentials: false": False,
        }
        for nested in workflow[with_index + 1:]:
            stripped = nested.strip()
            if not stripped:
                continue
            nested_indent = len(nested) - len(nested.lstrip())
            if nested_indent <= with_indent:
                break
            if stripped in required_checkout_settings:
                required_checkout_settings[stripped] = True
        missing = [setting for setting, present in required_checkout_settings.items() if not present]
        if not missing:
                sys.exit(0)
        print(f"actions/checkout@v4 with mapping is missing: {', '.join(missing)}")
sys.exit(1)
PY
  )"
  status=$?
  if [[ $status -eq 0 ]]; then
    pass "publish workflow checkout fetches full history without persisting credentials"
  else
    fail "publish workflow checkout fetches full history without persisting credentials ($output)"
  fi
  assert_file_contains "$publish_workflow" '[[ "$(git rev-parse HEAD)" == "$RELEASE_MERGE_SHA" ]]' "publish workflow asserts HEAD is the merged release PR commit"
  assert_file_contains_before "$publish_workflow" '[[ "$(git rev-parse HEAD)" == "$RELEASE_MERGE_SHA" ]]' 'Publish Docker image to Forgejo package registry' "publish workflow verifies checked-out merge commit before publishing the image with the publish token"
}

test_publish_workflow_attaches_merge_commit_to_main_with_upstream_before_image_publish() {
  local publish_workflow output status
  publish_workflow="$ROOT/.forgejo/workflows/release-publish.yml"

  output="$(python3 - "$publish_workflow" <<'PY'
import pathlib
import re
import sys

workflow = pathlib.Path(sys.argv[1]).read_text()
publish_marker = 'Publish Docker image to Forgejo package registry'
if publish_marker not in workflow:
    print('missing Docker image registry publication step')
    sys.exit(1)

before_publish = workflow.split(publish_marker, 1)[0]
lines = before_publish.splitlines()
attach_head_patterns = [
    r'\bgit\s+switch\b.*(?:-C|--force-create|-c|--create)\s+main\b.*\$RELEASE_MERGE_SHA',
    r'\bgit\s+checkout\b.*(?:-B|-b)\s+main\b.*\$RELEASE_MERGE_SHA',
]
move_main_patterns = [
    r'\bgit\s+branch\b.*(?:-f|--force)\s+main\b.*\$RELEASE_MERGE_SHA',
    r'\bgit\s+update-ref\s+refs/heads/main\s+\$RELEASE_MERGE_SHA\b',
]
checkout_main_patterns = [
    r'\bgit\s+switch\b(?!.*\$RELEASE_MERGE_SHA).*\bmain\b',
    r'\bgit\s+checkout\b(?!.*\$RELEASE_MERGE_SHA).*\bmain\b',
]
set_upstream_patterns = [
    r'\bgit\s+branch\b.*(?:--set-upstream-to|-u)(?:=|\s+)origin/main(?:\s+main)?\b',
]
upstream_remote_patterns = [r'\bgit\s+config\s+branch\.main\.remote\s+origin\b']
upstream_merge_patterns = [r'\bgit\s+config\s+branch\.main\.merge\s+refs/heads/main\b']

def first_matching_line(patterns, start=0):
    for index, line in enumerate(lines[start:], start):
        if any(re.search(pattern, line) for pattern in patterns):
            return index
    return None

attach_line = first_matching_line(attach_head_patterns)
if attach_line is None:
    move_main_line = first_matching_line(move_main_patterns)
    if move_main_line is not None:
        checkout_main_line = first_matching_line(checkout_main_patterns, move_main_line + 1)
        if checkout_main_line is not None:
            attach_line = checkout_main_line

if attach_line is None:
    print('missing attached HEAD on local main at $RELEASE_MERGE_SHA before registry image publication')
    sys.exit(1)

set_upstream_line = first_matching_line(set_upstream_patterns, attach_line + 1)
remote_line = first_matching_line(upstream_remote_patterns, attach_line + 1)
merge_line = first_matching_line(upstream_merge_patterns, attach_line + 1)
if set_upstream_line is None and (remote_line is None or merge_line is None):
    print('missing complete local main upstream tracking of origin/main after HEAD attaches to main and before registry image publication')
    sys.exit(1)
sys.exit(0)
PY
)"
  status=$?
  if [[ $status -eq 0 ]]; then
    pass "publish workflow attaches the merge commit to main with upstream before registry image publication"
  else
    fail "publish workflow attaches the merge commit to main with upstream before registry image publication ($output)"
  fi
}

test_release_tooling_uses_local_prepare_and_image_registry_for_publish() {
  local prepare_workflow publish_workflow
  prepare_workflow="$ROOT/.forgejo/workflows/release-prepare.yml"
  publish_workflow="$ROOT/.forgejo/workflows/release-publish.yml"

  assert_file_contains "$prepare_workflow" 'scripts/release plan --workspace .' "release PR preparation workflow invokes local release planning"
  assert_file_contains "$prepare_workflow" 'scripts/release prepare --workspace . --version "$RELEASE_VERSION"' "release PR preparation workflow invokes local release metadata preparation"
  assert_file_not_contains "$prepare_workflow" 'release-plz' "release PR preparation workflow does not invoke release-plz"
  assert_file_not_contains "$publish_workflow" 'release-plz' "publish workflow does not invoke release-plz"
  assert_file_contains "$publish_workflow" 'git.johnwilger.com/jwilger/auto_review/ar-gateway' "publish workflow targets the application image package registry"
  assert_file_contains "$prepare_workflow" 'tea login add' "release PR preparation workflow configures tea login"
  assert_file_contains "$prepare_workflow" 'tea pr create' "release PR preparation workflow uses tea for PR management"
  assert_file_contains "$publish_workflow" 'release create' "publish workflow creates a Forgejo Release entry"
}

test_publish_workflow_builds_release_image_and_generates_release_notes_after_merge() {
  local publish_workflow output status
  publish_workflow="$ROOT/.forgejo/workflows/release-publish.yml"

  output="$(python3 - "$publish_workflow" <<'PY'
import pathlib
import re
import sys

workflow = pathlib.Path(sys.argv[1]).read_text()
errors = []

image = 'git.johnwilger.com/jwilger/auto_review/ar-gateway'
required_image_tags = {
    'raw release version image tag': [f'{image}:$RELEASE_VERSION', f'{image}:${{RELEASE_VERSION}}'],
    'latest image tag': [f'{image}:latest'],
}
for description, candidates in required_image_tags.items():
    if not any(candidate in workflow for candidate in candidates):
        errors.append(f'missing {description}')

release_markers = [
    'login add',
    'release create',
    '--repo jwilger/auto_review',
    '--tag v$RELEASE_VERSION',
    '--target "$RELEASE_MERGE_SHA"',
    '--note-file',
    'GITEA_SERVER_TOKEN: ${{ secrets.RELEASE_PUBLISH_TOKEN }}',
    'GITEA_SERVER_URL: https://git.johnwilger.com',
]
missing_release_markers = [marker for marker in release_markers if marker not in workflow]
if missing_release_markers:
    errors.append('missing authenticated Forgejo Release creation markers: ' + ', '.join(missing_release_markers))

note_file_candidates = ['release_notes_file', 'RELEASE_NOTES_FILE', 'notes_file']
if not any(candidate in workflow for candidate in note_file_candidates):
    errors.append('publish workflow must write a dedicated release notes file before tea release create')

finalized_changelog_patterns = [
    r'CHANGELOG\.md[\s\S]{0,500}## \[\$RELEASE_VERSION\]',
    r'## \[\$RELEASE_VERSION\][\s\S]{0,500}CHANGELOG\.md',
    r'CHANGELOG\.md[\s\S]{0,500}## \[v?\$\{?RELEASE_VERSION\}?\]',
]
if not any(re.search(pattern, workflow) for pattern in finalized_changelog_patterns):
    errors.append('release notes file must be generated from the finalized CHANGELOG.md section for RELEASE_VERSION')

docker_link_candidates = [
    'git.johnwilger.com/jwilger/auto_review/ar-gateway:$RELEASE_VERSION',
    'git.johnwilger.com/jwilger/auto_review/ar-gateway:${RELEASE_VERSION}',
]
if not any(candidate in workflow for candidate in docker_link_candidates):
    errors.append('release notes file must include the raw-version release Docker image link git.johnwilger.com/jwilger/auto_review/ar-gateway:$RELEASE_VERSION')

release_create_match = re.search(r'(?:\btea\b|\$\{?[A-Za-z_][A-Za-z0-9_]*\}?|/[^\s]+/tea)\s+release\s+create', workflow)
if '--note-file' in workflow and release_create_match:
    note_file_index = min((workflow.find(candidate) for candidate in note_file_candidates if candidate in workflow), default=-1)
    release_create_index = release_create_match.start()
    if note_file_index == -1 or note_file_index > release_create_index:
        errors.append('release notes file must be generated before Forgejo release create consumes --note-file')

release_step_match = re.search(r'- name: Create Forgejo Release(?P<body>[\s\S]*?)(?:\n      - |\Z)', workflow)
if not release_step_match:
    errors.append('publish workflow must have a dedicated Create Forgejo Release step')
else:
    release_step = release_step_match.group('body')
    if 'GITEA_SERVER_TOKEN: ${{ secrets.RELEASE_PUBLISH_TOKEN }}' not in release_step:
        errors.append('Create Forgejo Release step must receive the publish token as GITEA_SERVER_TOKEN')
    if 'login add' not in release_step:
        errors.append('Create Forgejo Release step must authenticate tea explicitly with login add')
    if 'release create' not in release_step:
        errors.append('Create Forgejo Release step must create the Forgejo Release with release create')
    if '--target "$RELEASE_MERGE_SHA"' not in release_step:
        errors.append('tea release create must pin the release tag target to $RELEASE_MERGE_SHA')

if errors:
    print('; '.join(errors))
    sys.exit(1)
sys.exit(0)
PY
)"
  status=$?
  if [[ $status -eq 0 ]]; then
    pass "publish workflow builds release image and generates Forgejo release notes after merge"
  else
    fail "publish workflow builds release image and generates Forgejo release notes after merge ($output)"
  fi
}

test_publish_workflow_publishes_nix_docker_image_to_forgejo_registry() {
  local publish_workflow output status
  publish_workflow="$ROOT/.forgejo/workflows/release-publish.yml"

  output="$(python3 - "$publish_workflow" <<'PY'
import pathlib
import re
import sys

workflow = pathlib.Path(sys.argv[1]).read_text()
errors = []
if 'release-plz release --forge gitea --git-token "$RELEASE_PUBLISH_TOKEN"' in workflow:
    errors.append('publish workflow still delegates publication to release-plz cargo release')
if 'RELEASE_REGISTRY_USER' in workflow:
    errors.append('publish workflow should reuse RELEASE_BOT_NAME instead of a separate RELEASE_REGISTRY_USER')

required_markers = [
    'git.johnwilger.com/jwilger/auto_review/ar-gateway',
    'RELEASE_PUBLISH_TOKEN: ${{ secrets.RELEASE_PUBLISH_TOKEN }}',
    'RELEASE_BOT_NAME: ${{ vars.RELEASE_BOT_NAME }}',
    'nix build .#ar-gateway-image',
    'docker-archive:./result',
]
missing = [marker for marker in required_markers if marker not in workflow]
if missing:
    errors.append('publish workflow does not build and publish the release Docker image after merge: ' + ', '.join(missing))
if 'RELEASE_CANDIDATE_SHA' in workflow:
    errors.append('publish workflow must not depend on pre-merge candidate image tags')

lines = workflow.splitlines()
def workflow_steps():
    steps = []
    for index, line in enumerate(lines):
        if line.startswith('      - '):
            step_lines = [line]
            for nested in lines[index + 1:]:
                if nested.startswith('      - '):
                    break
                step_lines.append(nested)
            steps.append('\n'.join(step_lines))
    return steps

for index, line in enumerate(lines):
    if line.startswith('      - '):
        step_lines = [line]
        for nested in lines[index + 1:]:
            if nested.startswith('      - '):
                break
            step_lines.append(nested)
        step = '\n'.join(step_lines)
        if 'RELEASE_PUBLISH_TOKEN' in step and 'nix build .#ar-gateway-image' in step:
            errors.append('token-bearing publish step must publish only after a separate no-token Nix image build')
            break

publication_steps = []
for step in workflow_steps():
    if 'Publish Docker image to Forgejo package registry' in step or ('skopeo copy' in step and 'docker-archive:./result' in step):
        publication_steps.append(step)
if not publication_steps:
    errors.append('publish workflow is missing concrete skopeo publication from docker-archive:./result')
    before_publish = workflow
    publish_text = ''
else:
    first_publish = min(workflow.find(step) for step in publication_steps)
    before_publish = workflow[:first_publish]
    publish_text = '\n'.join(publication_steps)
    if 'docker-archive:./result' not in publish_text:
        errors.append('publish workflow must publish the Nix-built image archive after merge')
    version_destinations = [
        'docker://git.johnwilger.com/jwilger/auto_review/ar-gateway:$RELEASE_VERSION',
        'docker://git.johnwilger.com/jwilger/auto_review/ar-gateway:${RELEASE_VERSION}',
    ]
    if not any(destination in publish_text for destination in version_destinations):
        errors.append('publish workflow must publish the release image to RELEASE_VERSION')
    if 'docker://git.johnwilger.com/jwilger/auto_review/ar-gateway:latest' not in publish_text:
        errors.append('publish workflow must publish the release image to latest')
auth_patterns = [
    r'\b(?:docker|podman)\s+login\b[^\n]*git\.johnwilger\.com[\s\S]{0,400}\$RELEASE_PUBLISH_TOKEN',
    r'\$RELEASE_PUBLISH_TOKEN[\s\S]{0,400}\b(?:docker|podman)\s+login\b[^\n]*git\.johnwilger\.com',
]
has_login_before_publish = any(re.search(pattern, before_publish) for pattern in auth_patterns)
has_skopeo_creds_on_copy = re.search(r'(?:\bskopeo|"\$SKOPEO"|\$\{SKOPEO\}|/nix/var/nix/profiles/default/bin/skopeo)\s+copy\b[\s\S]{0,1000}(?:--src-creds|--dest-creds|--src-authfile|--dest-authfile)\b[\s\S]{0,300}\$RELEASE_PUBLISH_TOKEN', publish_text)
if not has_login_before_publish and not has_skopeo_creds_on_copy:
    errors.append('publish workflow must authenticate to git.johnwilger.com with RELEASE_PUBLISH_TOKEN before pushing or copying the image')

if re.search(r'git\.johnwilger\.com/jwilger/auto_review/ar-gateway:dev\b', workflow):
    errors.append('publish workflow must not publish the flake image default :dev tag as the release artifact')

if 'git.johnwilger.com/jwilger/auto_review/ar-gateway:latest' not in workflow:
    errors.append('publish workflow must publish the release image to latest')
if 'git.johnwilger.com/jwilger/auto_review/ar-gateway:$RELEASE_VERSION' not in workflow and 'git.johnwilger.com/jwilger/auto_review/ar-gateway:${RELEASE_VERSION}' not in workflow:
    errors.append('publish workflow must publish the release image to RELEASE_VERSION')

if errors:
    print('; '.join(errors))
    sys.exit(1)

sys.exit(0)
PY
)"
  status=$?
  if [[ $status -eq 0 ]]; then
    pass "publish workflow publishes the Nix-built Docker image to the Forgejo package registry"
  else
    fail "publish workflow publishes the Nix-built Docker image to the Forgejo package registry ($output)"
  fi
}

test_publish_workflow_requires_trusted_release_environment() {
  local publish_workflow output status
  publish_workflow="$ROOT/.forgejo/workflows/release-publish.yml"

  output="$(python3 - "$publish_workflow" <<'PY'
import pathlib
import sys

workflow = pathlib.Path(sys.argv[1]).read_text().splitlines()
job_line = None
for index, line in enumerate(workflow):
    if line == "  release-publish:":
        job_line = index
        break
if job_line is None:
    print("release-publish job is missing")
    sys.exit(1)

job_indent = len(workflow[job_line]) - len(workflow[job_line].lstrip())
environment_line = None
steps_line = None
for index in range(job_line + 1, len(workflow)):
    line = workflow[index]
    if not line.strip():
        continue
    indent = len(line) - len(line.lstrip())
    if indent <= job_indent:
        break
    if indent == job_indent + 2 and line.strip() == "environment: release-publish":
        environment_line = index
    if indent == job_indent + 2 and line.strip() == "steps:":
        steps_line = index
        break

if steps_line is None:
    print("release-publish job is missing steps")
    sys.exit(1)
if environment_line is None:
    print("release-publish job is missing job-level environment: release-publish before steps")
    sys.exit(1)
if environment_line > steps_line:
    print("release-publish job environment: release-publish appears after steps")
    sys.exit(1)
sys.exit(0)
PY
  )"
  status=$?
  if [[ $status -eq 0 ]]; then
    pass "publish workflow requires the protected release-publish environment at job level before steps"
  else
    fail "publish workflow requires the protected release-publish environment at job level before steps ($output)"
  fi
}

test_publish_workflow_supports_manual_dispatch_from_release_merge_sha() {
  local publish_workflow output status
  publish_workflow="$ROOT/.forgejo/workflows/release-publish.yml"

  output="$(python3 - "$publish_workflow" <<'PY'
import pathlib
import sys

workflow = pathlib.Path(sys.argv[1]).read_text()
if "workflow_dispatch:" not in workflow:
    print("publish workflow is missing workflow_dispatch trigger")
    sys.exit(1)
if "release_merge_sha:" not in workflow:
    print("publish workflow is missing release_merge_sha dispatch input")
    sys.exit(1)

dispatch_section = workflow.split("workflow_dispatch:", 1)[1].split("jobs:", 1)[0]
for required in ["inputs:", "release_merge_sha:", "required: true"]:
    if required not in dispatch_section:
        print(f"publish workflow dispatch input contract is missing: {required}")
        sys.exit(1)
dispatch_lines = dispatch_section.splitlines()
inputs_line = None
for index, line in enumerate(dispatch_lines):
    if line.strip() == "inputs:":
        inputs_line = index
        break
if inputs_line is None:
    print("publish workflow dispatch input contract is missing: inputs:")
    sys.exit(1)
input_names = []
for line in dispatch_lines[inputs_line + 1:]:
    if not line.strip():
        continue
    indent = len(line) - len(line.lstrip())
    if indent <= 4:
        break
    stripped = line.strip()
    if indent == 6 and stripped.endswith(":"):
        input_names.append(stripped[:-1])
if input_names != ["release_merge_sha"]:
    print(f"publish workflow dispatch should expose only release_merge_sha input, got: {input_names}")
    sys.exit(1)
release_input_lines = dispatch_section.split("release_merge_sha:", 1)[1].splitlines()
release_input = "\n".join(
    line
    for line in release_input_lines
    if not line.strip() or len(line) - len(line.lstrip()) > 6
)
if "required: true" not in release_input:
    print("publish workflow release_merge_sha dispatch input must be required")
    sys.exit(1)
if "type: string" not in release_input:
    print("publish workflow release_merge_sha dispatch input must render as a Forgejo UI text field")
    sys.exit(1)

job_header = "  release-publish:"
if job_header not in workflow:
    print("release-publish job is missing")
    sys.exit(1)
job_section = workflow.split(job_header, 1)[1]
job_if = None
for line in job_section.splitlines():
    if line.startswith("    steps:"):
        break
    if line.startswith("    if:"):
        job_if = line
        break
if job_if is None:
    print("release-publish job is missing an if condition")
    sys.exit(1)
condition = job_if.split("if:", 1)[1].strip()
if condition.startswith("${{") and condition.endswith("}}"):
    condition = condition[3:-2].strip()
paths = [path.strip(" ()") for path in condition.split("||")]
if len(paths) < 2:
    print("release-publish job condition must admit manual dispatch through a separate || path")
    sys.exit(1)
trusted_push_paths = [
    path for path in paths
    if "github.event_name == 'push'" in path
    and "github.ref == 'refs/heads/main'" in path
    and "inputs.release_merge_sha" not in path
]
manual_paths = [
    path for path in paths
    if "github.event_name == 'workflow_dispatch'" in path
    and "inputs.release_merge_sha" in path
    and "pull_request" not in path
]
if not trusted_push_paths:
    print("release-publish job condition is missing trusted push-to-main release path")
    sys.exit(1)
if not manual_paths:
    print("release-publish job condition is missing separate workflow_dispatch path gated by non-empty inputs.release_merge_sha")
    sys.exit(1)
expected_manual_paths = {
    "github.event_name == 'workflow_dispatch' && inputs.release_merge_sha != ''",
    'github.event_name == \'workflow_dispatch\' && inputs.release_merge_sha != ""',
}
if not any(path in expected_manual_paths for path in manual_paths):
    print("release-publish job workflow_dispatch path must be exactly github.event_name == 'workflow_dispatch' && inputs.release_merge_sha != ''")
    sys.exit(1)

required_markers = [
    "environment: release-publish",
    'git switch -C main "$RELEASE_MERGE_SHA"',
    'Publish Docker image to Forgejo package registry',
    'release create',
    'nix build .#ar-gateway-image',
    'git.johnwilger.com/jwilger/auto_review/ar-gateway',
]
missing = [marker for marker in required_markers if marker not in workflow]
if missing:
    print("publish workflow manual dispatch does not reuse release merge SHA for checkout/provenance/branch attachment/publication: " + ", ".join(missing))
    sys.exit(1)

publish_marker = 'Publish Docker image to Forgejo package registry'
before_publish = workflow.split(publish_marker, 1)[0]
for required in [
    "Validate release provenance and changed files",
    '[[ "$(git rev-parse HEAD)" == "$RELEASE_MERGE_SHA" ]]',
    'git merge-base --is-ancestor "$RELEASE_MERGE_SHA" origin/main',
    'git diff --name-only "$RELEASE_BASE_SHA" "$RELEASE_MERGE_SHA"',
]:
    if required not in before_publish:
        print(f"publish workflow manual dispatch is missing no-token provenance validation before publish token exposure: {required}")
        sys.exit(1)
token_index = workflow.find("RELEASE_PUBLISH_TOKEN: ${{ secrets.RELEASE_PUBLISH_TOKEN }}")
validation_index = workflow.find("Validate release provenance and changed files")
if token_index == -1:
    print("publish workflow is missing protected release publish token exposure")
    sys.exit(1)
if validation_index == -1 or token_index < validation_index:
    print("publish workflow exposes publish token before no-token provenance validation")
    sys.exit(1)
validation_step = None
lines = workflow.splitlines()
for index, line in enumerate(lines):
    if line.strip() == "- name: Validate release provenance and changed files":
        step_lines = [line]
        for nested in lines[index + 1:]:
            if nested.startswith("      - "):
                break
            step_lines.append(nested)
        validation_step = "\n".join(step_lines)
        break
if validation_step is None:
    print("publish workflow is missing validation step")
    sys.exit(1)
if "RELEASE_PUBLISH_TOKEN" in validation_step:
    print("publish workflow validation step must not expose RELEASE_PUBLISH_TOKEN")
    sys.exit(1)
publish_step_index = workflow.find("Publish Docker image to Forgejo package registry")
release_step_index = workflow.find("release create")
if publish_step_index == -1:
    print("publish workflow is missing publish step")
    sys.exit(1)
if publish_step_index < validation_index or token_index < publish_step_index:
    print("publish workflow must expose RELEASE_PUBLISH_TOKEN only in token-bearing publish/release steps after validation completes")
    sys.exit(1)
allowed_token_lines = set()
for index, line in enumerate(lines):
    if line.strip() in {
        "- name: Publish Docker image to Forgejo package registry",
        "- name: Create Forgejo Release",
    }:
        step_lines = [line]
        for nested in lines[index + 1:]:
            if nested.startswith("      - "):
                break
            step_lines.append(nested)
        allowed_token_lines.update(range(index, index + len(step_lines)))
if not allowed_token_lines:
    print("publish workflow is missing publish step")
    sys.exit(1)
for index, line in enumerate(lines):
    if "RELEASE_PUBLISH_TOKEN" in line and index not in allowed_token_lines:
        print("publish workflow must confine every RELEASE_PUBLISH_TOKEN reference to token-bearing image publish or Forgejo Release steps")
        sys.exit(1)
for forbidden in ["git clone", "tea pr checkout", "gh pr checkout"]:
    if forbidden in workflow:
        print(f"publish workflow should not require local manual checkout recovery commands, found: {forbidden}")
        sys.exit(1)
PY
)"
  status=$?
  if [[ $status -eq 0 ]]; then
    pass "publish workflow supports protected manual dispatch from a release merge SHA"
  else
    fail "publish workflow supports protected manual dispatch from a release merge SHA ($output)"
  fi
}

test_publish_workflow_keeps_dispatch_input_out_of_non_dispatch_paths() {
  local publish_workflow output status
  publish_workflow="$ROOT/.forgejo/workflows/release-publish.yml"

  output="$(python3 - "$publish_workflow" <<'PY'
import pathlib
import re
import sys

workflow = pathlib.Path(sys.argv[1]).read_text()
errors = []

if "workflow_dispatch:" not in workflow or "release_merge_sha:" not in workflow:
    errors.append("publish workflow must still expose release_merge_sha for workflow_dispatch reruns")

job_match = re.search(r'(?ms)^  release-publish:\n(?P<body>.*?)(?=^  [a-zA-Z0-9_-]+:|\Z)', workflow)
if not job_match:
    print("publish workflow is missing the release-publish job")
    sys.exit(1)
job = job_match.group('body')

steps = re.findall(r'(?ms)^      - (?P<step>.*?)(?=^      - |^  [a-zA-Z0-9_-]+:|\Z)', job)
if not steps:
    errors.append("publish workflow release-publish job is missing steps")

push_merge_steps = [step for step in steps if "github.sha" in step]
if not push_merge_steps:
    errors.append("push release path must use github.sha directly as the release merge SHA")
for step in push_merge_steps:
    first_line = step.splitlines()[0].strip()
    if "inputs.release_merge_sha" in step:
        errors.append(f"push merge commit step must not reference inputs.release_merge_sha: {first_line}")

dispatch_steps = [step for step in steps if "inputs.release_merge_sha" in step]
if not dispatch_steps:
    errors.append("workflow_dispatch release path must still consume inputs.release_merge_sha")
for step in dispatch_steps:
    first_line = step.splitlines()[0].strip()
    has_dispatch_gate = "github.event_name == 'workflow_dispatch'" in step or 'github.event_name == "workflow_dispatch"' in step
    if not has_dispatch_gate:
        errors.append(f"workflow_dispatch input step must be gated to workflow_dispatch only: {first_line}")
    if "github.event.pull_request" in step or "github.sha" in step or "github.event.before" in step:
        errors.append(f"workflow_dispatch input step must not also depend on push or pull_request context: {first_line}")

if errors:
    print("; ".join(errors))
    sys.exit(1)
PY
)"
  status=$?
  if [[ $status -eq 0 ]]; then
    pass "publish workflow keeps workflow_dispatch input out of non-dispatch release paths"
  else
    fail "publish workflow keeps workflow_dispatch input out of non-dispatch release paths ($output)"
  fi
}

test_publish_workflow_uses_trusted_tools_after_publish_token_exposure() {
  local publish_workflow output status
  publish_workflow="$ROOT/.forgejo/workflows/release-publish.yml"

  output="$(python3 - "$publish_workflow" <<'PY'
import pathlib
import re
import sys

workflow = pathlib.Path(sys.argv[1]).read_text()
errors = []

job_match = re.search(r'(?ms)^  release-publish:\n(?P<body>.*?)(?=^  [a-zA-Z0-9_-]+:|\Z)', workflow)
if not job_match:
    print('publish workflow is missing the release-publish job')
    sys.exit(1)
job = job_match.group('body')
steps = re.findall(r'(?ms)^      - (?P<step>.*?)(?=^      - |^  [a-zA-Z0-9_-]+:|\Z)', job)
if not steps:
    print('publish workflow release-publish job is missing steps')
    sys.exit(1)

token_markers = [
    'RELEASE_PUBLISH_TOKEN: ${{ secrets.RELEASE_PUBLISH_TOKEN }}',
    'GITEA_SERVER_TOKEN: ${{ secrets.RELEASE_PUBLISH_TOKEN }}',
]
first_token_index = min((job.find(marker) for marker in token_markers if marker in job), default=-1)
if first_token_index == -1:
    errors.append('publish workflow must expose the publish token only in final publication steps')

resolver_markers = [
    'TRUSTED_RELEASE_TOOLS',
    'SKOPEO=',
    'TEA=',
    'trusted-release-tools',
    '/nix/var/nix/profiles/default/bin/skopeo',
    '/nix/var/nix/profiles/default/bin/tea',
]
resolver_positions = [job.find(marker) for marker in resolver_markers if marker in job]
if not resolver_positions or (first_token_index != -1 and min(resolver_positions) > first_token_index):
    errors.append('publish workflow must resolve trusted skopeo/tea tool paths before exposing RELEASE_PUBLISH_TOKEN or GITEA_SERVER_TOKEN')

trusted_tool_markers = {
    'skopeo': [
        '/nix/var/nix/profiles/default/bin/skopeo',
        '$TRUSTED_RELEASE_TOOLS/bin/skopeo',
        '${TRUSTED_RELEASE_TOOLS}/bin/skopeo',
        '$SKOPEO',
        '${SKOPEO}',
    ],
    'tea': [
        '/nix/var/nix/profiles/default/bin/tea',
        '$TRUSTED_RELEASE_TOOLS/bin/tea',
        '${TRUSTED_RELEASE_TOOLS}/bin/tea',
        '$TEA',
        '${TEA}',
    ],
    'jq': [
        '/nix/var/nix/profiles/default/bin/jq',
        '$TRUSTED_RELEASE_TOOLS/bin/jq',
        '${TRUSTED_RELEASE_TOOLS}/bin/jq',
        '$JQ',
        '${JQ}',
    ],
    'curl': [
        '/nix/var/nix/profiles/default/bin/curl',
        '$TRUSTED_RELEASE_TOOLS/bin/curl',
        '${TRUSTED_RELEASE_TOOLS}/bin/curl',
        '$CURL',
        '${CURL}',
    ],
    'awk': [
        '/nix/var/nix/profiles/default/bin/awk',
        '$TRUSTED_RELEASE_TOOLS/bin/awk',
        '${TRUSTED_RELEASE_TOOLS}/bin/awk',
        '$AWK',
        '${AWK}',
    ],
    'sed': [
        '/nix/var/nix/profiles/default/bin/sed',
        '$TRUSTED_RELEASE_TOOLS/bin/sed',
        '${TRUSTED_RELEASE_TOOLS}/bin/sed',
        '$SED',
        '${SED}',
    ],
    'mktemp': [
        '/nix/var/nix/profiles/default/bin/mktemp',
        '$TRUSTED_RELEASE_TOOLS/bin/mktemp',
        '${TRUSTED_RELEASE_TOOLS}/bin/mktemp',
        '$MKTEMP',
        '${MKTEMP}',
    ],
    'cat': [
        '/nix/var/nix/profiles/default/bin/cat',
        '$TRUSTED_RELEASE_TOOLS/bin/cat',
        '${TRUSTED_RELEASE_TOOLS}/bin/cat',
        '$CAT',
        '${CAT}',
    ],
    'mkdir': [
        '/nix/var/nix/profiles/default/bin/mkdir',
        '$TRUSTED_RELEASE_TOOLS/bin/mkdir',
        '${TRUSTED_RELEASE_TOOLS}/bin/mkdir',
        '$MKDIR',
        '${MKDIR}',
    ],
}
pre_token_job = job[:first_token_index] if first_token_index != -1 else ''
trusted_tool_variables = {
    'skopeo': ['SKOPEO'],
    'tea': ['TEA'],
    'jq': ['JQ'],
    'curl': ['CURL'],
    'awk': ['AWK'],
    'sed': ['SED'],
    'mktemp': ['MKTEMP'],
    'cat': ['CAT'],
    'mkdir': ['MKDIR'],
}
def uses_pre_resolved_trusted_tool(step, tool, markers):
    trusted_tools_proven_outside_checkout = any(
        assignment in pre_token_job
        for assignment in [
            'TRUSTED_RELEASE_TOOLS=/nix/var/nix/profiles/default',
            'TRUSTED_RELEASE_TOOLS=/nix/var/nix/profiles/default/bin',
            'TRUSTED_RELEASE_TOOLS="/nix/var/nix/profiles/default"',
            'TRUSTED_RELEASE_TOOLS="/nix/var/nix/profiles/default/bin"',
        ]
    ) and re.search(r'printf\s+["\'][^"\']*TRUSTED_RELEASE_TOOLS=', pre_token_job) and '>> "$GITHUB_ENV"' in pre_token_job
    direct_markers = [
        marker
        for marker in markers
        if marker.startswith('/nix/var/nix/profiles/default/bin/')
        or (trusted_tools_proven_outside_checkout and marker.startswith('$TRUSTED_RELEASE_TOOLS/'))
        or (trusted_tools_proven_outside_checkout and marker.startswith('${TRUSTED_RELEASE_TOOLS}/'))
    ]
    if any(marker in step for marker in direct_markers):
        return True
    for variable in trusted_tool_variables[tool]:
        if f'${variable}' not in step and f'${{{variable}}}' not in step:
            continue
        trusted_assignments = [
            f'{variable}=/nix/var/nix/profiles/default/bin/{tool}',
            f'{variable}="$TRUSTED_RELEASE_TOOLS/bin/{tool}"',
            f'{variable}="${{TRUSTED_RELEASE_TOOLS}}/bin/{tool}"',
        ]
        persisted_variable = (
            re.search(rf'printf\s+["\'][^"\']*{variable}=', pre_token_job) and '>> "$GITHUB_ENV"' in pre_token_job
        )
        trusted_assignment = any(assignment in pre_token_job for assignment in trusted_assignments)
        trusted_tools_assignment = any('TRUSTED_RELEASE_TOOLS' in assignment for assignment in trusted_assignments if assignment in pre_token_job)
        if persisted_variable and trusted_assignment and (not trusted_tools_assignment or trusted_tools_proven_outside_checkout):
            return True
    return False
token_steps = []
for step in steps:
    name_match = re.search(r'name:\s*(?P<name>[^\n]+)', step)
    name = name_match.group('name').strip().strip('"\'') if name_match else '<unnamed>'
    has_publish_token = any(marker in step for marker in token_markers)
    if not has_publish_token:
        continue
    token_steps.append(name)
    if name not in {'Publish Docker image to Forgejo package registry', 'Create Forgejo Release'}:
        errors.append(f'RELEASE_PUBLISH_TOKEN must be confined to final publish/release steps, not {name}')
    step_header = step.split('        run:', 1)[0]
    guarded_by_release_decision = (
        re.search(r'(?m)^\s*if:\s*.*(?:release|publish|eligible|approved|should)', step_header, re.I)
        and (
            re.search(r'(?m)^\s*if:\s*.*workflow_dispatch', step_header)
            or re.search(r'(?m)^\s*if:\s*.*(?:true|success|passed|approved|eligible)', step_header, re.I)
        )
    )
    if not guarded_by_release_decision:
        errors.append(f'token-bearing step must be unreachable unless workflow_dispatch or the push release-commit guard approved publication: {name}')
    for forbidden_label, pattern in {
        'nix develop': r'\bnix\s+develop\b',
        'nix run .#': r'\bnix\s+run\s+\.#',
        'cargo run': r'\bcargo\s+run\b',
        'workspace scripts': r'(^|[\s"\'])scripts/',
        'workspace executable': r'(^|\n)\s*(?:bash|sh|python3?|perl|ruby)\s+(?:\./|scripts/|\$GITHUB_WORKSPACE/|\$\{GITHUB_WORKSPACE\}/)',
    }.items():
        if re.search(pattern, step):
            errors.append(f'token-bearing publish step must not use checkout-derived dev-shell tooling ({forbidden_label}): {name}')
    for tool, markers in trusted_tool_markers.items():
        if re.search(rf'(?<![A-Za-z0-9_/-]){tool}\b', step) and not uses_pre_resolved_trusted_tool(step, tool, markers):
            errors.append(f'token-bearing publish step must invoke pre-resolved trusted {tool} before token exposure: {name}')

if not token_steps:
    errors.append('publish workflow has no token-bearing final publication steps to guard')

if errors:
    print('; '.join(errors))
    sys.exit(1)
PY
)"
  status=$?
  if [[ $status -eq 0 ]]; then
    pass "publish workflow uses only trusted pre-resolved tools after publish token exposure"
  else
    fail "publish workflow uses only trusted pre-resolved tools after publish token exposure ($output)"
  fi
}

test_publish_workflow_builds_and_publishes_release_image_after_merge() {
  local publish_workflow output status
  publish_workflow="$ROOT/.forgejo/workflows/release-publish.yml"

  assert_file_contains "$publish_workflow" 'nix build .#ar-gateway-image' "publish workflow builds the ar-gateway image after merge to main"
  assert_file_contains "$publish_workflow" 'docker-archive:./result' "publish workflow publishes final release tags from the merged release image archive"
  assert_file_contains "$publish_workflow" 'RELEASE_PUBLISH_TOKEN: ${{ secrets.RELEASE_PUBLISH_TOKEN }}' "publish workflow uses the publish token only after merge validation"
  assert_file_not_contains "$publish_workflow" 'RELEASE_CANDIDATE_SHA' "publish workflow does not depend on pre-merge candidate image tags"

  output="$(python3 - "$publish_workflow" <<'PY'
import pathlib
import re
import sys

workflow = pathlib.Path(sys.argv[1]).read_text()
errors = []

publish_marker = 'Publish Docker image to Forgejo package registry'
if publish_marker not in workflow:
    errors.append('publish workflow is missing the token-bearing image publication step')
    before_publish = workflow
else:
    before_publish = workflow.split(publish_marker, 1)[0]

if 'RELEASE_CANDIDATE_SHA' in workflow or 'github.event.pull_request.head.sha' in workflow:
    errors.append('publish workflow must not derive or promote pre-merge candidate image tags')
if 'nix build .#ar-gateway-image' not in before_publish:
    errors.append('publish workflow must build the Docker image from the merged release commit before token-bearing publication')

publication_step_match = re.search(r'- name: Publish Docker image to Forgejo package registry(?P<body>[\s\S]*?)(?:\n      - |\Z)', workflow)
if not publication_step_match:
    errors.append('publish workflow must have a dedicated image publication step')
else:
    step = publication_step_match.group('body')
    if 'RELEASE_PUBLISH_TOKEN: ${{ secrets.RELEASE_PUBLISH_TOKEN }}' not in step:
        errors.append('image publication step must receive RELEASE_PUBLISH_TOKEN')
    if 'docker-archive:./result' not in step:
        errors.append('image publication step must copy from the merged release image archive')
    for destination in [
        ('docker://git.johnwilger.com/jwilger/auto_review/ar-gateway:$RELEASE_VERSION', 'docker://git.johnwilger.com/jwilger/auto_review/ar-gateway:${RELEASE_VERSION}'),
        'docker://git.johnwilger.com/jwilger/auto_review/ar-gateway:latest',
    ]:
        if isinstance(destination, tuple):
            if not any(candidate in step for candidate in destination):
                errors.append('image publication step must copy the merged release image to RELEASE_VERSION')
        elif destination not in step:
            errors.append(f'image publication step must copy the merged release image to {destination}')
    if 'nix build .#ar-gateway-image' in step:
        errors.append('image publication step must publish only after a separate no-token Nix image build')

if errors:
    print('; '.join(errors))
    sys.exit(1)
PY
)"
  status=$?
  if [[ $status -eq 0 ]]; then
    pass "publish workflow builds and publishes the release image after merge"
  else
    fail "publish workflow builds and publishes the release image after merge ($output)"
  fi
}

test_publish_workflow_attaches_binary_archives_checksums_signatures_and_provenance() {
  local publish_workflow output status
  publish_workflow="$ROOT/.forgejo/workflows/release-publish.yml"

  output="$(python3 - "$publish_workflow" <<'PY'
import pathlib
import re
import sys

workflow = pathlib.Path(sys.argv[1]).read_text()
errors = []

release_step_match = re.search(r'- name: Create Forgejo Release(?P<body>[\s\S]*?)(?:\n      - |\Z)', workflow)
if not release_step_match:
    errors.append('publish workflow must have a dedicated Create Forgejo Release step')
    release_step = ''
    asset_upload_section = workflow
else:
    release_step = release_step_match.group('body')
    asset_upload_section = workflow[release_step_match.start():]

required_release_assets = {
    'Linux x86_64 auto-review binary archive': [
        'auto-review-$RELEASE_VERSION-linux-x86_64.tar.gz',
        'auto-review-${RELEASE_VERSION}-linux-x86_64.tar.gz',
        'x86_64-unknown-linux',
    ],
    'SHA-256 checksum manifest': ['SHA256SUMS', 'sha256sum'],
    'signature files': ['.sig', 'sign-blob', 'minisign', 'cosign sign-blob'],
    'SBOM metadata': ['sbom', 'SBOM', 'cyclonedx', 'spdx', 'syft'],
    'provenance metadata': ['provenance', 'attestation', 'slsa'],
}
for description, candidates in required_release_assets.items():
    if not any(candidate in workflow for candidate in candidates):
        errors.append(f'missing {description}')

release_create_lines = []
collecting_release_create = False
for line in release_step.splitlines():
    if 'release create' in line and not collecting_release_create:
        collecting_release_create = True
    if collecting_release_create:
        release_create_lines.append(line)
        if not line.rstrip().endswith('\\'):
            break
release_create_command = '\n'.join(release_create_lines)

if not release_create_command:
    errors.append('Create Forgejo Release step must include a tea release create command')
elif not any(marker in release_create_command for marker in ['--asset', '--attachment']):
    errors.append('tea release create must attach release artifacts as part of release creation instead of using a later asset-upload command')

if re.search(r'(?m)\breleases?\s+assets\s+create\b', asset_upload_section):
    errors.append('publish workflow must not rely on a separate tea releases assets create command after release creation')

for required in [
    'auto-review-$RELEASE_VERSION-linux-x86_64.tar.gz',
    'SHA256SUMS',
    'SHA256SUMS.sig',
    'release-signing-key.pub',
    'allowed-signers',
    'sbom.spdx.json',
    'provenance.json',
]:
    if required not in release_create_command:
        errors.append(f'tea release create is missing atomic asset attachment marker: {required}')

if errors:
    print('; '.join(errors))
    sys.exit(1)
PY
)"
  status=$?
  if [[ $status -eq 0 ]]; then
    pass "publish workflow attaches Linux binary archives, checksums, signatures, SBOM, and provenance metadata"
  else
    fail "publish workflow attaches Linux binary archives, checksums, signatures, SBOM, and provenance metadata ($output)"
  fi
}

test_publish_workflow_verifies_generated_binary_artifacts_before_release_upload() {
  local publish_workflow output status
  publish_workflow="$ROOT/.forgejo/workflows/release-publish.yml"

  output="$(python3 - "$publish_workflow" <<'PY'
import pathlib
import re
import sys

workflow = pathlib.Path(sys.argv[1]).read_text()
errors = []

release_marker = 'Create Forgejo Release'
before_release = workflow.split(release_marker, 1)[0] if release_marker in workflow else workflow
required_before_release = [
    'sha256sum -c SHA256SUMS',
    'linux-x86_64',
    '.sig',
    'sbom',
    'provenance',
]
for marker in required_before_release:
    if marker not in before_release:
        errors.append(f'generated release artifacts are not verified before Forgejo upload: {marker}')

signature_verify_patterns = [
    r'cosign\s+verify-blob[\s\S]{0,300}\.sig',
    r'minisign\s+-V[\s\S]{0,300}\.sig',
    r'gpg\s+--verify[\s\S]{0,300}\.sig',
    r'ssh-keygen\s+-Y\s+verify[\s\S]{0,300}\.sig',
]
if not any(re.search(pattern, before_release) for pattern in signature_verify_patterns):
    errors.append('generated binary signatures must be verified before Forgejo upload')

artifact_build_index = min(
    (index for index in [before_release.find('linux-x86_64')] if index != -1),
    default=-1,
)
checksum_verify_index = before_release.find('sha256sum -c SHA256SUMS')
if artifact_build_index == -1 or checksum_verify_index == -1 or checksum_verify_index < artifact_build_index:
    errors.append('checksum verification must run after binary artifact generation and before release creation')

if errors:
    print('; '.join(errors))
    sys.exit(1)
PY
)"
  status=$?
  if [[ $status -eq 0 ]]; then
    pass "publish workflow verifies generated binary artifacts before Forgejo upload"
  else
    fail "publish workflow verifies generated binary artifacts before Forgejo upload ($output)"
  fi
}

test_publish_workflow_handles_release_signing_key_in_private_tempdir() {
  local publish_workflow output status
  publish_workflow="$ROOT/.forgejo/workflows/release-publish.yml"

  output="$(python3 - "$publish_workflow" <<'PY'
import pathlib
import re
import sys

workflow = pathlib.Path(sys.argv[1]).read_text()
errors = []

step_match = re.search(r'- name: Build and verify Linux binary release artifacts(?P<body>[\s\S]*?)(?:\n      - |\Z)', workflow)
if not step_match:
    print('publish workflow is missing the binary artifact build/signing step')
    sys.exit(1)

step = step_match.group('body')
if 'umask 077' not in step:
    errors.append('binary signing step must set umask 077 before writing signing material')

tempdir_match = re.search(r'(?P<var>[A-Za-z_][A-Za-z0-9_]*)="?\$\(mktemp -d\)"?', step)
tempdir_var = tempdir_match.group('var') if tempdir_match else None
if tempdir_var is None:
    errors.append('binary signing step must create a private temporary directory with mktemp -d')
else:
    trap_patterns = [
        rf"trap\s+'rm -rf \"\${tempdir_var}\"'\s+(?P<signals>[A-Z ]+)",
        rf'trap\s+"rm -rf \\\"\${tempdir_var}\\\""\s+(?P<signals>[A-Z ]+)',
        rf"trap\s+'rm -rf \"\${{{tempdir_var}}}\"'\s+(?P<signals>[A-Z ]+)",
        rf'trap\s+"rm -rf \\\"\${{{tempdir_var}}}\\\""\s+(?P<signals>[A-Z ]+)',
    ]
    trap_matches = [match for pattern in trap_patterns for match in re.finditer(pattern, step)]
    sign_indices = [index for marker in ['ssh-keygen -Y sign', 'ssh-keygen -y -f "$signing_key"'] if (index := step.find(marker)) != -1]
    first_sign_index = min(sign_indices, default=-1)
    if not trap_matches:
        errors.append('binary signing step must install a trap that runs rm -rf "$signing_dir" on EXIT TERM INT')
    elif first_sign_index == -1:
        errors.append('binary signing step must sign with the private signing key')
    else:
        pre_sign_trap_signals = {
            signal
            for match in trap_matches
            if match.start() < first_sign_index
            for signal in match.group('signals').split()
        }
        missing_signals = sorted({'EXIT', 'TERM', 'INT'} - pre_sign_trap_signals)
        if missing_signals:
            errors.append('binary signing step must install signing directory cleanup traps before signing commands for: ' + ', '.join(missing_signals))
    key_path_patterns = [
        rf'signing_key="\${tempdir_var}/[^"\n]+"',
        rf'signing_key="\${{{tempdir_var}}}/[^"\n]+"',
    ]
    if not any(re.search(pattern, step) for pattern in key_path_patterns):
        errors.append('binary signing step must store the private key inside the private temporary directory')

if 'chmod 600 "$signing_key"' not in step:
    errors.append('binary signing step must chmod 600 the private signing key file')
if 'printf \'%s\\n\' "$RELEASE_SIGNING_KEY" > "$signing_key"' not in step and 'printf "%s\\n" "$RELEASE_SIGNING_KEY" > "$signing_key"' not in step:
    errors.append('binary signing step must write RELEASE_SIGNING_KEY only to the private signing key file')

artifact_leak_patterns = [
    r'RELEASE_SIGNING_KEY[^\n]*release-artifacts',
    r'release-artifacts[^\n]*RELEASE_SIGNING_KEY',
    r'private[-_]key',
    r'id_(?:ed25519|rsa)',
]
for pattern in artifact_leak_patterns:
    if re.search(pattern, step, re.I):
        errors.append('binary signing step must not place RELEASE_SIGNING_KEY/private key material under release-artifacts')
        break

if errors:
    print('; '.join(errors))
    sys.exit(1)
PY
)"
  status=$?
  if [[ $status -eq 0 ]]; then
    pass "publish workflow handles release signing key in a private temporary directory"
  else
    fail "publish workflow handles release signing key in a private temporary directory ($output)"
  fi
}

test_publish_workflow_builds_x86_64_linux_artifacts_in_docker() {
  local publish_workflow output status
  publish_workflow="$ROOT/.forgejo/workflows/release-publish.yml"

  output="$(python3 - "$publish_workflow" <<'PY'
import pathlib
import re
import sys

workflow = pathlib.Path(sys.argv[1]).read_text()
errors = []

job_match = re.search(r'(?ms)^  release-publish:\n(?P<body>.*?)(?=^  [a-zA-Z0-9_-]+:|\Z)', workflow)
if not job_match:
    print('publish workflow is missing the release-publish job')
    sys.exit(1)

job = job_match.group('body')
if not re.search(r'(?m)^    runs-on:\s*docker\s*$', job):
    errors.append('release-publish job must run in a Docker container instead of directly on a native host runner')

step_match = re.search(r'- name: Build and verify Linux binary release artifacts(?P<body>[\s\S]*?)(?:\n      - |\Z)', workflow)
if not step_match:
    print('publish workflow is missing the binary artifact build/signing step')
    sys.exit(1)

step = step_match.group('body')
first_x86_build = step.find('.#packages.x86_64-linux.default')
first_aarch64_build = step.find('.#packages.aarch64-linux.default')
platform_config = step.find('extra-platforms = x86_64-linux aarch64-linux')

if first_x86_build == -1:
    errors.append('binary artifact step must build the x86_64-linux package')
if first_aarch64_build != -1:
    errors.append('binary artifact step must not attempt native aarch64-linux builds from the Docker release runner')
if platform_config != -1:
    errors.append('binary artifact step must not enable extra-platforms for Docker release builds')

if errors:
    print('; '.join(errors))
    sys.exit(1)
PY
)"
  status=$?
  if [[ $status -eq 0 ]]; then
    pass "publish workflow builds x86_64 Linux binary artifacts in Docker"
  else
    fail "publish workflow builds x86_64 Linux binary artifacts in Docker ($output)"
  fi
}

test_publish_workflow_allows_intentional_release_tooling_changes_before_token_publish() {
  local publish_workflow output status
  publish_workflow="$ROOT/.forgejo/workflows/release-publish.yml"

  output="$(python3 - "$publish_workflow" <<'PY'
import fnmatch
import pathlib
import re
import sys

workflow = pathlib.Path(sys.argv[1]).read_text()
publish_marker = 'Publish Docker image to Forgejo package registry'
if publish_marker not in workflow:
    print('missing token-bearing image publication step')
    sys.exit(1)

validation_section = workflow.split(publish_marker, 1)[0]
case_match = re.search(r'case "\$changed_file" in(?P<body>.*?)\n\s*esac', validation_section, re.S)
if not case_match:
    print('missing changed-file case allowlist before publish')
    sys.exit(1)

patterns = []
reject_patterns = []
case_lines = case_match.group('body').splitlines()
index = 0
while index < len(case_lines):
    stripped = case_lines[index].strip()
    index += 1
    if not stripped or stripped.startswith('#') or ')' not in stripped:
        continue
    head = stripped.split(')', 1)[0]
    arm_body = []
    while index < len(case_lines):
        body_line = case_lines[index].strip()
        arm_body.append(body_line)
        index += 1
        if body_line == ';;':
            break
    meaningful_body = [line for line in arm_body if line and not line.startswith('#')]
    is_pass_through = meaningful_body == [';;']
    is_reject = any('exit' in line or 'refusing token-bearing publish' in line for line in meaningful_body)
    parts = [part for part in head.split('|') if part]
    if is_pass_through:
        patterns.extend(parts)
    if is_reject:
        reject_patterns.extend(parts)

required_allowed = [
    '.forgejo/workflows/release-prepare.yml',
    '.forgejo/workflows/release-publish.yml',
    'scripts/release',
    'tests/release_tooling_test.sh',
    'tests/release_tooling/publish_workflow_test.sh',
    'tests/release_tooling/ci_pr_artifacts_test.sh',
    'tests/release_tooling/docs_secrets_test.sh',
    'tests/release_tooling/release_script_flake_test.sh',
]
missing_allowed = [sample for sample in required_allowed if not any(fnmatch.fnmatchcase(sample, pattern) for pattern in patterns)]
if missing_allowed:
    print('publish allowlist does not permit intentional release workflow/script/test changes: ' + ', '.join(missing_allowed))
    print('observed allowlist patterns: ' + ', '.join(patterns))
    errors = 1
else:
    errors = 0

unexpected_samples = [
    '.forgejo/workflows/ci.yml',
    'scripts/unrelated-token-helper',
    '.forgejo/workflows/untrusted.yml',
]
missing_rejections = [sample for sample in unexpected_samples if not any(fnmatch.fnmatchcase(sample, pattern) for pattern in reject_patterns)]
if missing_rejections:
    print('publish allowlist does not explicitly refuse unexpected token-bearing workflow/script changes: ' + ', '.join(missing_rejections))
    print('observed reject patterns: ' + ', '.join(reject_patterns))
    errors = 1

if 'refusing token-bearing publish for release PR file:' not in validation_section:
    print('publish workflow must fail closed with a clear refusal for unexpected token-bearing changes')
    errors = 1
if 'tests/release_tooling/*.sh' not in validation_section:
    print('publish workflow must categorize release tooling shell tests with an explicit tests/release_tooling/*.sh allowlist entry')
    errors = 1
if errors:
    sys.exit(1)
PY
)"
  status=$?
  if [[ $status -eq 0 ]]; then
    pass "publish workflow allows intentional release tooling changes while rejecting unexpected token-bearing files"
  else
    fail "publish workflow allows intentional release tooling changes while rejecting unexpected token-bearing files ($output)"
  fi
}

run_tests \
  test_publish_workflow_triggers_on_main_push_or_manual_dispatch_only \
  test_publish_workflow_does_not_persist_push_commit_message_to_github_env \
  test_publish_workflow_validates_provenance_and_changed_files_before_publish_token \
  test_publish_workflow_uses_push_sha_without_dispatch_inputs_on_push_path \
  test_publish_workflow_uses_release_pr_merge_sha_not_a_recomputed_version \
  test_publish_workflow_executes_from_merge_commit_sha_before_publish_token \
  test_publish_workflow_attaches_merge_commit_to_main_with_upstream_before_image_publish \
  test_release_tooling_uses_local_prepare_and_image_registry_for_publish \
  test_publish_workflow_builds_release_image_and_generates_release_notes_after_merge \
  test_publish_workflow_publishes_nix_docker_image_to_forgejo_registry \
  test_publish_workflow_requires_trusted_release_environment \
  test_publish_workflow_supports_manual_dispatch_from_release_merge_sha \
  test_publish_workflow_keeps_dispatch_input_out_of_non_dispatch_paths \
  test_publish_workflow_uses_trusted_tools_after_publish_token_exposure \
  test_publish_workflow_builds_and_publishes_release_image_after_merge \
  test_publish_workflow_attaches_binary_archives_checksums_signatures_and_provenance \
  test_publish_workflow_verifies_generated_binary_artifacts_before_release_upload \
  test_publish_workflow_handles_release_signing_key_in_private_tempdir \
  test_publish_workflow_builds_x86_64_linux_artifacts_in_docker \
  test_publish_workflow_allows_intentional_release_tooling_changes_before_token_publish

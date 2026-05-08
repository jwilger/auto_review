#!/usr/bin/env bash
set -u

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=tests/release_tooling/lib.sh
source "$SCRIPT_DIR/lib.sh"
RELEASE_TOOLING_SUITE_NAME="release tooling: docs secrets"

test_changelog_uses_release_marker_without_unreleased_section() {
  local output status

  output="$(python3 - "$ROOT/CHANGELOG.md" <<'PY'
import pathlib
import sys

text = pathlib.Path(sys.argv[1]).read_text()
marker = "<!-- release-prepare inserts generated release sections below this line -->"
if "## [Unreleased]" in text:
    print("CHANGELOG should not contain an Unreleased section")
    sys.exit(1)
if marker not in text:
    print("missing release-prepare insertion marker")
    sys.exit(1)

sys.exit(0)
PY
)"
  status=$?

  if [[ $status -eq 0 ]]; then
    pass "CHANGELOG uses release marker without Unreleased section"
  else
    fail "CHANGELOG uses release marker without Unreleased section ($output)"
  fi
}

test_release_workflows_use_prepare_secret_and_protected_publish_token() {
  local prepare_workflow publish_workflow
  prepare_workflow="$ROOT/.forgejo/workflows/release-prepare.yml"
  publish_workflow="$ROOT/.forgejo/workflows/release-publish.yml"

  assert_file_not_contains "$prepare_workflow" 'GITEA_SERVER_TOKEN: ${{ forgejo.token }}' "release PR preparation workflow does not use unsupported forgejo.token expression for tea"
  assert_file_not_contains "$prepare_workflow" 'FORGEJO_ACTIONS_TOKEN: ${{ forgejo.token }}' "release PR preparation workflow does not use unsupported forgejo.token expression for git push"
  assert_file_contains "$prepare_workflow" 'RELEASE_PREPARE_TOKEN: ${{ secrets.RELEASE_PREPARE_TOKEN }}' "release PR preparation workflow exposes the prepare-scoped Actions secret to release tooling"
  assert_file_contains "$prepare_workflow" 'GITEA_SERVER_TOKEN="$RELEASE_PREPARE_TOKEN"' "release PR preparation workflow passes the prepare-scoped token to PR management tea calls"
  assert_file_not_contains "$prepare_workflow" 'GITEA_SERVER_TOKEN="$RELEASE_PUBLISH_TOKEN"' "release PR preparation workflow does not use the publish-scoped token before merge"
  assert_file_not_contains "$prepare_workflow" 'repo_token=' "release PR preparation workflow does not derive shared helper tokens"
  assert_file_not_contains "$prepare_workflow" 'GITEA_SERVER_TOKEN: ${{ secrets.RELEASE_PREPARE_TOKEN }}' "release PR preparation workflow does not expose tea token at step scope"
  assert_file_not_contains "$prepare_workflow" 'FORGEJO_ACTIONS_TOKEN' "release PR preparation workflow does not expose manual git push token environment"
  assert_file_not_contains "$prepare_workflow" 'TEA_TOKEN:' "release PR preparation workflow does not use tea's legacy token env var"
  assert_file_not_contains "$prepare_workflow" 'secrets.FORGEJO_TOKEN' "release PR preparation workflow does not use the legacy shared Actions secret"
  assert_file_not_contains "$prepare_workflow" 'secrets.FORGEJO_RELEASE_PREPARE_TOKEN' "release PR preparation workflow does not reference the old disallowed prepare secret name"
  assert_file_not_contains "$prepare_workflow" 'secrets.FORGEJO_RELEASE_PUBLISH_TOKEN' "release PR preparation workflow does not reference the old disallowed publish secret name"
  assert_file_not_contains "$prepare_workflow" 'FORGEJO_RELEASE_PREPARE_TOKEN' "release PR preparation workflow does not reference the old disallowed prepare token name anywhere"
  assert_file_not_contains "$prepare_workflow" 'FORGEJO_RELEASE_PUBLISH_TOKEN' "release PR preparation workflow does not expose the old publish-scoped Actions secret"

  assert_file_contains "$publish_workflow" 'RELEASE_PUBLISH_TOKEN: ${{ secrets.RELEASE_PUBLISH_TOKEN }}' "publish workflow uses the publish-scoped Actions secret"
  assert_file_not_contains "$publish_workflow" 'release-plz' "publish workflow does not use release-plz"
  assert_file_contains "$publish_workflow" 'git.johnwilger.com/jwilger/auto_review/ar-gateway' "publish workflow uses the publish-scoped token for registry image publication"
  assert_file_contains "$publish_workflow" 'GITEA_SERVER_TOKEN: ${{ secrets.RELEASE_PUBLISH_TOKEN }}' "publish workflow intentionally broadens the publish-scoped token to Forgejo Release creation"
  assert_file_contains "$publish_workflow" 'GITEA_SERVER_URL: https://git.johnwilger.com' "publish workflow points Forgejo Release API calls at the Forgejo server"
  assert_file_not_contains "$publish_workflow" 'FORGEJO_TOKEN: ${{ secrets.RELEASE_PUBLISH_TOKEN }}' "publish workflow does not expose legacy FORGEJO_TOKEN to publish tooling"
  assert_file_not_contains "$publish_workflow" 'secrets.FORGEJO_TOKEN' "publish workflow does not use the legacy shared Actions secret"
  assert_file_not_contains "$publish_workflow" 'secrets.FORGEJO_RELEASE_PREPARE_TOKEN' "publish workflow does not reference the old disallowed prepare secret name"
  assert_file_not_contains "$publish_workflow" 'secrets.FORGEJO_RELEASE_PUBLISH_TOKEN' "publish workflow does not reference the old disallowed publish secret name"
  assert_file_not_contains "$publish_workflow" 'FORGEJO_RELEASE_PREPARE_TOKEN' "publish workflow does not expose the prepare-scoped Actions secret"
}

test_release_docs_account_for_final_binary_assets_and_publish_token_scope() {
  local output status

  output="$(python3 - "$ROOT/docs/THREAT-MODEL.md" "$ROOT/docs/OPERATIONS.md" <<'PY'
import pathlib
import re
import sys

threat = pathlib.Path(sys.argv[1]).read_text()
operations = pathlib.Path(sys.argv[2]).read_text()
combined = threat + '\n' + operations
errors = []

concept_patterns = {
    'Linux x86_64 binary archive': r'(?is)(?:Linux[^\n]{0,80}x86_64|x86_64[^\n]{0,80}Linux)[^\n]{0,120}(?:archive|tarball|download|asset)',
    'auto-review binary release asset': r'(?is)auto-review[^\n]{0,120}(?:binary|archive|tarball|download|asset)',
    'SHA-256 checksum concept': r'(?is)(?:SHA-256|sha256|SHA256SUMS)',
    'signature concept': r'(?is)(?:signature|\.sig|sign-blob|minisign|gpg --verify)',
    'SBOM concept': r'(?is)(?:SBOM|software bill of materials|cyclonedx|spdx|syft)',
    'provenance concept': r'(?is)(?:provenance|attestation|slsa)',
    'release publish token scope': r'(?is)(?:RELEASE_PUBLISH_TOKEN|Release publishing PAT)[^\n]{0,240}(?:binary|archive|asset|checksum|signature|SBOM|provenance)',
}
for description, pattern in concept_patterns.items():
    if not re.search(pattern, combined):
        errors.append(f'docs missing binary release concept: {description}')

for forbidden in ['future binary', 'future Linux binary', 'after issue #121 lands', 'Issue #121 must publish']:
    if forbidden in combined:
        errors.append(f'docs still describe binary assets as future work: {forbidden}')

scope_patterns = [
    r'Release publishing PAT[^\n]*(?:Linux binary archives|binary release assets)',
    r'RELEASE_PUBLISH_TOKEN[^\n]*(?:Linux binary archives|binary release assets)',
]
if not any(re.search(pattern, combined, re.I) for pattern in scope_patterns):
    errors.append('docs do not tie release publishing token scope to binary archives and metadata')

verification_patterns = [
    r'sha256sum\s+-c\s+SHA256SUMS',
    r'(?:cosign\s+verify-blob|minisign\s+-V|gpg\s+--verify|ssh-keygen\s+-Y\s+verify)',
]
for pattern in verification_patterns:
    if not re.search(pattern, combined):
        errors.append(f'docs missing operator verification command matching: {pattern}')

if errors:
    print('; '.join(errors))
    sys.exit(1)
PY
)"
  status=$?
  if [[ $status -eq 0 ]]; then
    pass "docs and threat model account for binary assets and publish token scope"
  else
    fail "docs and threat model account for binary assets and publish token scope ($output)"
  fi
}

test_release_secrets_are_documented_for_operators() {
  assert_file_contains "$ROOT/docs/OPERATIONS.md" 'Forgejo Actions secret `RELEASE_PREPARE_TOKEN`' "operations docs require an operator-created release preparation Actions secret"
  assert_file_contains "$ROOT/docs/THREAT-MODEL.md" 'Forgejo Actions secret `RELEASE_PREPARE_TOKEN`' "threat model documents the operator-created release preparation Actions secret"
  assert_file_not_contains "$ROOT/docs/OPERATIONS.md" 'Forgejo Actions secret `RELEASE_CANDIDATE_TOKEN`' "operations docs do not require a separate candidate image publishing Actions secret"
  assert_file_not_contains "$ROOT/docs/THREAT-MODEL.md" 'Forgejo Actions secret `RELEASE_CANDIDATE_TOKEN`' "threat model does not document a separate candidate image publishing Actions secret"
  assert_file_contains "$ROOT/docs/OPERATIONS.md" 'PR Docker images under a package name distinct from final releases' "operations docs describe separate-package PR Docker images"
  assert_file_contains "$ROOT/docs/OPERATIONS.md" 'PR binary downloads as Forgejo generic packages' "operations docs describe package-hosted PR binaries"
  assert_file_contains "$ROOT/docs/OPERATIONS.md" 'delete PR Docker and binary packages after the PR merges' "operations docs describe PR package cleanup after merge"
  assert_file_not_contains "$ROOT/docs/OPERATIONS.md" 'Build pre-release versions from source' "operations docs no longer tell PR users to build from source"
  assert_file_contains "$ROOT/docs/OPERATIONS.md" 'release publishing credential' "operations docs identify the release publishing credential purpose"
  assert_file_contains "$ROOT/docs/OPERATIONS.md" 'Forgejo Actions secret `RELEASE_PUBLISH_TOKEN`' "operations docs document release publishing credential as an Actions secret"
  assert_file_contains "$ROOT/docs/OPERATIONS.md" 'release bot Forgejo user' "operations docs require a dedicated release bot user"
  assert_file_contains "$ROOT/docs/OPERATIONS.md" 'Forgejo Actions secret `RELEASE_SIGNING_KEY`' "operations docs document the release signing key secret"
  assert_file_contains "$ROOT/docs/OPERATIONS.md" 'repository variables `RELEASE_BOT_NAME` and `RELEASE_BOT_EMAIL`' "operations docs document release bot identity variables"
  assert_file_contains "$ROOT/docs/OPERATIONS.md" 'owned by the same release bot named in `RELEASE_BOT_NAME`' "operations docs reuse release bot identity for registry publishing"
  assert_file_not_contains "$ROOT/docs/OPERATIONS.md" 'RELEASE_REGISTRY_USER' "operations docs do not require a separate registry user variable"
  assert_file_contains "$ROOT/docs/THREAT-MODEL.md" 'release bot identity in repository variable `RELEASE_BOT_NAME`' "threat model reuses release bot identity for registry publishing"
  assert_file_not_contains "$ROOT/docs/THREAT-MODEL.md" 'RELEASE_REGISTRY_USER' "threat model does not require a separate registry user variable"
  assert_file_contains "$ROOT/docs/THREAT-MODEL.md" 'Release signing key' "threat model names the release signing key asset"
  assert_file_not_contains "$ROOT/docs/OPERATIONS.md" 'manual approval gate' "operations docs do not require a manual approval gate for release publishing credentials"
  assert_file_contains "$ROOT/docs/OPERATIONS.md" 'Configure the release publishing credential as Forgejo Actions secret `RELEASE_PUBLISH_TOKEN`' "operations docs describe the publish token as an ordinary Actions secret"
  assert_file_not_contains "$ROOT/docs/OPERATIONS.md" 'FORGEJO_RELEASE_PREPARE_TOKEN' "operations docs do not expose the old operator-facing prepare secret name"
  assert_file_not_contains "$ROOT/docs/OPERATIONS.md" 'FORGEJO_RELEASE_PUBLISH_TOKEN' "operations docs do not expose the old operator-facing publish secret name"
  assert_file_not_contains "$ROOT/docs/THREAT-MODEL.md" 'FORGEJO_RELEASE_PREPARE_TOKEN' "threat model does not expose the old operator-facing prepare secret name"
  assert_file_not_contains "$ROOT/docs/THREAT-MODEL.md" 'FORGEJO_RELEASE_PUBLISH_TOKEN' "threat model does not expose the old operator-facing publish secret name"
  assert_file_not_contains "$ROOT/docs/OPERATIONS.md" 'Forgejo Actions secret `FORGEJO_TOKEN`' "operations docs do not document the legacy shared release Actions secret"
  assert_file_lacks_line "$ROOT/deploy/systemd/auto_review.env.example" 'FORGEJO_TOKEN=' "systemd env example does not declare the release publishing Actions secret"
  assert_file_not_contains "$ROOT/deploy/systemd/auto_review.env.example" 'Release publishing credential' "systemd env example does not describe the Actions-only release publishing credential"
}


test_release_token_blast_radius_is_documented() {
  local t5a
  t5a="$(python3 - "$ROOT/docs/THREAT-MODEL.md" <<'PY'
import pathlib
import re
import sys

text = pathlib.Path(sys.argv[1]).read_text()
match = re.search(r'(?ms)^### T5a\. Release preparation and publishing PAT compromise\n(?P<section>.*?)(?=^### |\Z)', text)
if not match:
    raise SystemExit("T5a release PAT threat section not found")
print(match.group("section"))
PY
)"

  assert_file_contains "$ROOT/docs/THREAT-MODEL.md" 'Release preparation PAT' "threat model names the operator-created release preparation PAT asset"
  assert_file_contains "$ROOT/docs/THREAT-MODEL.md" 'Release publishing PAT' "threat model names the release publishing PAT asset"
  assert_file_contains "$ROOT/docs/THREAT-MODEL.md" 'Release preparation PAT blast radius' "threat model documents the release preparation PAT blast radius"
  assert_file_contains "$ROOT/docs/THREAT-MODEL.md" 'Release publishing PAT blast radius' "threat model documents the release publishing PAT blast radius"
  assert_file_contains "$ROOT/docs/THREAT-MODEL.md" 'prepare release PR branches and release PRs only in `jwilger/auto_review`' "threat model documents the release preparation PAT scope"
  assert_file_contains "$ROOT/docs/THREAT-MODEL.md" 'publish container images to `git.johnwilger.com/jwilger/auto_review/ar-gateway` and create Forgejo Releases only in `jwilger/auto_review`' "threat model documents the release publishing PAT package registry and release API scope"
  assert_file_not_contains "$ROOT/docs/THREAT-MODEL.md" 'Release candidate publishing PAT' "threat model does not document a separate candidate publishing PAT"
  assert_contains "$t5a" 'publishes PR Docker images under a package name distinct from final releases' "T5a mitigation documents separate-package PR Docker publication"
  assert_contains "$t5a" 'publishes PR binary downloads as Forgejo generic packages' "T5a mitigation documents package-hosted PR binaries"
  assert_contains "$t5a" 'deletes PR Docker and binary packages after the PR merges' "T5a mitigation documents PR package cleanup after merge"
  assert_contains "$t5a" 'updates PR descriptions with package links' "T5a mitigation documents PR body artifact-link edits"
  assert_contains "$t5a" 'PR body edit' "T5a mitigation explicitly names PR body edit capability"
  assert_contains "$t5a" 'generic package deletion' "T5a mitigation explicitly names generic package deletion capability"
  assert_contains "$t5a" 'PR package publishing credential' "T5a mitigation documents the distinct credential model for PR package publication"
  assert_not_contains "$t5a" 'promotes the candidate image to the release version and `latest` tags' "T5a mitigation does not document stale candidate-image promotion"
  assert_not_contains "$t5a" 'Build pre-release versions from source' "T5a mitigation no longer tells pre-release users to build from source"
  assert_contains "$t5a" 'builds the release Docker image after the release PR merges to main' "T5a mitigation documents Docker image publication only after merge to main"
  assert_contains "$t5a" 'publishes only `git.johnwilger.com/jwilger/auto_review/ar-gateway` to the Forgejo package registry and creates the matching Forgejo Release entry' "T5a mitigation documents package registry and Forgejo Release publication instead of cargo publishing"
  assert_not_contains "$t5a" 'Forgejo release selection to `release-plz`' "T5a mitigation does not describe stale release-plz cargo publication"
  assert_contains "$t5a" '`Cargo.toml`, `Cargo.lock`, and `CHANGELOG.md`' "T5a mitigation publish allowlist includes Cargo.toml, Cargo.lock, and CHANGELOG.md"
  assert_contains "$t5a" 'root release metadata' "T5a mitigation documents root release metadata is permitted"
  assert_contains "$t5a" '`tests/release_tooling/*.sh`' "T5a mitigation publish allowlist includes categorized release tooling shell tests"
  assert_not_contains "$t5a" 'root and package release metadata' "T5a mitigation does not permit package-crate release metadata for Docker image publication"
  assert_not_contains "$t5a" 'Prepare validates dispatch inputs' "T5a mitigation does not describe stale manual dispatch input validation"
  assert_not_contains "$t5a" 'validates the derived semantic version' "T5a mitigation does not describe stale derived semantic-version validation"
  assert_file_contains "$ROOT/docs/OPERATIONS.md" 'release preparation PAT blast radius' "operations docs summarize the release preparation PAT blast radius"
  assert_file_contains "$ROOT/docs/OPERATIONS.md" 'release publishing PAT blast radius' "operations docs summarize the release publishing PAT blast radius"
  assert_file_contains "$ROOT/docs/OPERATIONS.md" 'prepare release PR branches and release PRs only in `jwilger/auto_review`' "operations docs constrain the release preparation PAT scope"
  assert_file_contains "$ROOT/docs/OPERATIONS.md" 'publish container images to `git.johnwilger.com/jwilger/auto_review/ar-gateway` and create Forgejo Releases only in `jwilger/auto_review`' "operations docs constrain the release publishing PAT package registry and release API scope"
  assert_file_contains "$ROOT/docs/THREAT-MODEL.md" 'PR body edit' "threat model documents the broadened PR body edit blast radius"
  assert_file_contains "$ROOT/docs/THREAT-MODEL.md" 'generic package deletion' "threat model documents the broadened generic package deletion blast radius"
  assert_file_contains "$ROOT/docs/OPERATIONS.md" 'PR package publishing credential' "operations docs document the distinct credential model for PR package publishing"
}

test_threat_model_release_publish_token_boundary_and_asset_inventory_cover_pr_package_blast_radius() {
  local output status

  output="$(python3 - "$ROOT/docs/THREAT-MODEL.md" <<'PY'
import pathlib
import re
import sys

text = pathlib.Path(sys.argv[1]).read_text()
errors = []

boundary_match = re.search(r'(?ms)^## Trust Boundaries\n(?P<section>.*?)(?=^## )', text)
asset_match = re.search(r'(?ms)^## Asset Inventory\n(?P<section>.*?)(?=^## )', text)
if not boundary_match:
    errors.append('threat model is missing Trust Boundaries section')
    trust_boundary = ''
else:
    trust_boundary = boundary_match.group('section')
if not asset_match:
    errors.append('threat model is missing Asset Inventory section')
    asset_inventory = ''
else:
    asset_inventory = asset_match.group('section')

required_scopes = {
    'PR Docker publish': r'(?is)PR[^|\n]*(?:Docker|container)[^|\n]*publish|publish[^|\n]*PR[^|\n]*(?:Docker|container)',
    'PR Docker delete': r'(?is)PR[^|\n]*(?:Docker|container)[^|\n]*delet|delet[^|\n]*PR[^|\n]*(?:Docker|container)',
    'generic package publish': r'(?is)generic package[^|\n]*publish|publish[^|\n]*generic package',
    'generic package delete': r'(?is)generic package[^|\n]*delet|delet[^|\n]*generic package',
    'PR body edit': r'(?is)PR body edit|PR description[^|\n]*(?:edit|update)|(?:edit|update)[^|\n]*PR description',
}

def release_publish_entries(section):
    entries = []
    for line in section.splitlines():
        if 'RELEASE_PUBLISH_TOKEN' in line or 'Release publishing PAT' in line:
            entries.append(line.strip())
    return entries

for section_name, section in [
    ('Trust Boundaries', trust_boundary),
    ('Asset Inventory', asset_inventory),
]:
    entries = release_publish_entries(section)
    if not entries:
        errors.append(f'{section_name} has no RELEASE_PUBLISH_TOKEN / Release publishing PAT row or entry')
        continue
    joined_entries = '\n'.join(entries)
    if not re.search(r'RELEASE_PUBLISH_TOKEN|Release publishing PAT', joined_entries):
        errors.append(f'{section_name} release publishing row is not tied directly to RELEASE_PUBLISH_TOKEN / Release publishing PAT')
    for description, pattern in required_scopes.items():
        if not any(re.search(pattern, entry) for entry in entries):
            errors.append(f'{section_name} RELEASE_PUBLISH_TOKEN / Release publishing PAT row does not directly describe blast radius for {description}')

if errors:
    print('; '.join(errors))
    sys.exit(1)
PY
)"
  status=$?
  if [[ $status -eq 0 ]]; then
    pass "threat model trust boundary and asset inventory cover PR package publish-token blast radius"
  else
    fail "threat model trust boundary and asset inventory cover PR package publish-token blast radius ($output)"
  fi
}

test_release_docs_residual_risk_and_operations_blast_radius_enumerate_pr_package_scopes() {
  local output status

  output="$(python3 - "$ROOT/docs/THREAT-MODEL.md" "$ROOT/docs/OPERATIONS.md" <<'PY'
import pathlib
import re
import sys

threat = pathlib.Path(sys.argv[1]).read_text()
operations = pathlib.Path(sys.argv[2]).read_text()
errors = []

t5a_match = re.search(r'(?ms)^### T5a\. Release preparation and publishing PAT compromise\n(?P<section>.*?)(?=^### |\Z)', threat)
if not t5a_match:
    errors.append('T5a release PAT threat section not found')
    t5a = ''
else:
    t5a = t5a_match.group('section')

residual_match = re.search(r'(?ms)\*Residual risk:\*(?P<residual>.*?)(?=\n### |\Z)', t5a)
if not residual_match:
    errors.append('T5a residual-risk wording not found')
    residual = ''
else:
    residual = residual_match.group('residual')

operations_blast_radius_match = re.search(
    r'(?ms)(?P<paragraph>^Configure the release publishing credential as Forgejo Actions secret `RELEASE_PUBLISH_TOKEN`.*?)(?=\n\s*\n|\Z)',
    operations,
)
if not operations_blast_radius_match:
    errors.append('operations release publishing PAT blast-radius paragraph not found')
    operations_blast_radius = ''
else:
    operations_blast_radius = operations_blast_radius_match.group('paragraph')

required_scopes = {
    'PR Docker/container package publish': r'(?is)(?:publish|publishes)[^.\n;]*(?:PR|pull request)[^.\n;]*(?:Docker|container)[^.\n;]*package|(?:PR|pull request)[^.\n;]*(?:Docker|container)[^.\n;]*package[^.\n;]*(?:publish|publishes)',
    'PR Docker/container package delete': r'(?is)(?:delete|deletes)[^.\n;]*(?:PR|pull request)[^.\n;]*(?:Docker|container)[^.\n;]*package|(?:PR|pull request)[^.\n;]*(?:Docker|container)[^.\n;]*package[^.\n;]*(?:delete|deletes)',
    'generic package publish': r'(?is)(?:publish|publishes)[^.\n;]*generic package|generic package[^.\n;]*(?:publish|publishes)',
    'generic package delete': r'(?is)(?:delete|deletes)[^.\n;]*generic package|generic package[^.\n;]*(?:delete|deletes)',
    'managed PR body/description edit': r'(?is)managed[^.\n;]*(?:PR body|PR description|pull request body|pull request description)[^.\n;]*(?:edit|update)|(?:edit|update)[^.\n;]*managed[^.\n;]*(?:PR body|PR description|pull request body|pull request description)',
}

for description, pattern in required_scopes.items():
    if not re.search(pattern, residual):
        errors.append(f'T5a residual-risk wording does not explicitly include {description}')
    if not re.search(pattern, operations_blast_radius):
        errors.append(f'operations release publishing PAT blast-radius paragraph does not explicitly include {description}')

if errors:
    print('; '.join(errors))
    sys.exit(1)
PY
)"
  status=$?
  if [[ $status -eq 0 ]]; then
    pass "release docs residual risk and operations blast radius enumerate PR package scopes"
  else
    fail "release docs residual risk and operations blast radius enumerate PR package scopes ($output)"
  fi
}

run_tests \
  test_changelog_uses_release_marker_without_unreleased_section \
  test_release_workflows_use_prepare_secret_and_protected_publish_token \
  test_release_docs_account_for_final_binary_assets_and_publish_token_scope \
  test_release_secrets_are_documented_for_operators \
  test_release_token_blast_radius_is_documented \
  test_threat_model_release_publish_token_boundary_and_asset_inventory_cover_pr_package_blast_radius \
  test_release_docs_residual_risk_and_operations_blast_radius_enumerate_pr_package_scopes

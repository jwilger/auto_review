# ar-review

Review-pipeline activities: clone the PR workspace,
build RAG context, render the LLM prompt, validate output via
self-heal, optionally verify findings, and map the result to a
Forgejo review request.

## Public surface

| Module | What's in it |
|--------|-------------|
| `pipeline::review_pull_request` | Top-level semantic review activity. Inputs via `ReviewArgs`; outputs a `ReviewOutcome`. Branches on `VerifyMode::{Simple, Agentic}`. |
| `config::RepoConfig` | `.auto_review.yaml` parser. `parse_repo_config` (permissive runtime loader) and `parse_repo_config_strict` (typo-rejecting validator) cover the two use cases. |
| `workspace::prepare_workspace` | Shallow `git clone` of the PR's head SHA into a tmpfs workdir. Token-redacting URL builder. |
| `verify::verify_findings`, `agentic_verify::verify_findings_agentic` | Two verifier modes; the agentic one uses the workspace tools. |
| `workspace_tools::{read_file, search}` | Read-only LLM-callable tools, sandboxed under the workspace root. |
| `heal::generate_with_self_heal` | LLM-call wrapper that retries on schema-validation failure with the validator's error appended to the prompt. |
| `mapping::output_to_review_request` | Convert validated `ReviewOutput` to a Forgejo `CreateReviewRequest`. |

## Pipeline shape

```
prepare_workspace
   ↓
load_repo_config (ignored_paths, guidelines, …)
   ↓
list_changed_files → filter (ignored_paths)
   ↓
build_review_context (RAG, optional)
   ↓
render_review_prompt
   ↓
generate_with_self_heal (reasoning tier)
   ↓
verify_findings / verify_findings_agentic
   ↓
filter by min_severity (AR_SEVERITY_FLOOR)
   ↓
output_to_review_request → forgejo.create_review
```

## Tests

`cargo test -p ar-review` covers the full pipeline end-to-end via
wiremock-stubbed Forgejo + canned-response LLM, plus per-module
unit tests. The integration test files
`tests/red_team_workspace_tools.rs` and `tests/red_team_pipeline.rs`
pin threat-model T3/T4/T7/T8/T9 mitigations as CI-enforced
contracts.

## Dependencies

Deterministic linters/tests/builds are expected to run in CI before the
CI-triggered semantic review endpoint calls this pipeline.

`globset` for the `.auto_review.yaml` `ignored_paths`,
`serde_yaml` for the config parser, `git2` indirectly via the
workspace's git ops (handled in `workspace.rs`).

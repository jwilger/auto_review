# ar-prompts

Prompt templates, JSON schemas, and validators for every LLM call
the bot makes. Schemas live under `schemas/`; their corresponding
Rust modules expose the validator + system prompt + DTO triple.

## Public surface

| Schema | Module | Used for |
|--------|--------|----------|
| `schemas/review.json` | `prompt`, `schema`, `types`, `validate` | Reasoning-tier review output (summary, walkthrough, findings). |
| `schemas/triage.json` | `triage` | Cheap-tier per-file triage classification. |
| `schemas/verification.json` | `verification` | Cheap-tier finding-by-finding verifier. |

Every schema sets `additionalProperties: false` and uses
`serde(deny_unknown_fields)` on its DTO so prompt-injection
attempts to add new fields fail validation. This is one of the
load-bearing T3 mitigations — see the
`red_team_pipeline.rs::t3_*` tests in `ar-review` for the contract.

## Public types and helpers

| Item | Purpose |
|------|---------|
| `prompt::ReviewPromptInputs`, `render_review_prompt` | User-prompt rendering for the reasoning model. |
| `prompt::system_prompt` | Static system prompt for the review pipeline. |
| `validate::validate_review_output` | Schema-validation entry point used by the self-heal loop in `ar-review`. |
| `triage::*`, `verification::*` | Same triple pattern (system prompt, schema, validator) for the other LLM calls. |

## Tests

`cargo test -p ar-prompts` covers each schema's allow-list shape,
unknown-field rejection, well-formed-output acceptance, and
`additionalProperties: false`. The schema files are static-
included via `include_str!` so a CI lint failure surfaces
schema-file drift immediately.

## Dependencies

`serde`, `serde_json`, `serde_yaml` (for the validator's
configuration parsing in adjacent crates). The schema files
themselves are JSON; no compile-time codegen.

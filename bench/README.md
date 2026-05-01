# `auto_review` benchmark fixtures

The `bench` subcommand of the `auto_review` CLI replays one or more PR
fixtures through the LLM-review path (prompt rendering → reasoning
model → self-heal → optional verifier) and reports per-fixture
finding counts and latency, plus an aggregate over the batch.

It is **not** a precision/recall benchmark — there is no labelled
ground truth here. It is a regression-tracking and model-comparison
harness: run the same fixture set against two models, two prompt
revisions, or before/after a code change, and see how the numbers
move.

## Running

```sh
auto_review bench bench/fixtures \
    --llm-base-url http://localhost:11434 \
    --llm-model qwen2.5-coder:32b
```

Optional verifier pass (drops findings the cheap model doesn't
corroborate):

```sh
auto_review bench bench/fixtures \
    --llm-base-url http://localhost:11434 \
    --llm-model qwen2.5-coder:32b \
    --llm-cheap-model qwen2.5-coder:7b
```

`--json` switches the aggregate output to a single line of JSON,
suitable for piping into a regression dashboard.

## Fixture format

One JSON file per PR fixture. Required fields:

| Field | Type | Notes |
|---|---|---|
| `name` | string | Used in the per-row output and aggregate. |
| `pr_title` | string | |
| `diff` | string | Unified diff text. The same shape Forgejo's `/pulls/{n}.diff` returns. |

Optional fields:

| Field | Type | Default | Notes |
|---|---|---|---|
| `repo_full_name` | string | `""` | `{owner}/{repo}` for prompt context. |
| `pr_number` | u64 | `0` | |
| `pr_body` | string | `""` | |
| `changed_files` | string[] | `[]` | List of repo-relative paths; surfaced to the prompt. |
| `linter_findings` | Finding[] | `[]` | Pre-computed linter findings to inject as supplementary context. Same shape as `ar_tools::Finding`. |
| `guidelines` | string | `""` | Repo guidelines, as if loaded from `.auto_review.yaml`. |
| `repo_context` | string | `""` | Pre-rendered RAG context. Skip if you don't want to wire RAG retrieval. |

Two starter fixtures live in this directory; treat them as the
minimal baseline. Add your own real PRs (with sensitive content
removed) to grow the corpus over time.

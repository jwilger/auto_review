# Benchmarks

`auto-review bench run` replays one or more PR fixtures through the LLM review
path: prompt rendering, reasoning model, self-heal, optional verifier, and
aggregate reporting.

Use it to compare models, prompt revisions, or code changes against a stable
fixture set.

## Running

```sh
auto-review bench run bench/fixtures \
  --llm-base-url http://localhost:11434 \
  --llm-model qwen2.5-coder:32b
```

With the optional cheap-tier verifier:

```sh
auto-review bench run bench/fixtures \
  --llm-base-url http://localhost:11434 \
  --llm-model qwen2.5-coder:32b \
  --llm-cheap-model qwen2.5-coder:7b
```

`--json` emits one aggregate JSON line for dashboards or baseline files.

## Baseline comparison

```sh
auto-review bench run bench/fixtures \
  --llm-base-url http://localhost:11434 \
  --llm-model qwen2.5-coder:32b \
  --json > baseline.json

auto-review bench run bench/fixtures \
  --llm-base-url http://localhost:11434 \
  --llm-model qwen2.5-coder:32b \
  --baseline baseline.json
```

`--fail-on-regression` requires `--baseline` and exits non-zero when success
rate, precision, or recall drops by more than 5 percentage points, or p99
latency rises by more than 5 seconds.

## Fixture format

One JSON file per PR fixture. Required fields:

| Field | Type | Notes |
|---|---|---|
| `name` | string | Name shown in output. |
| `pr_title` | string | PR title. |
| `diff` | string | Unified diff, matching Forgejo's `/pulls/{n}.diff` shape. |

Optional fields:

| Field | Type | Default | Notes |
|---|---|---|---|
| `repo_full_name` | string | `""` | `{owner}/{repo}` prompt context. |
| `pr_number` | u64 | `0` | PR number prompt context. |
| `pr_body` | string | `""` | PR body prompt context. |
| `changed_files` | string[] | `[]` | Changed paths surfaced to the prompt. |
| `guidelines` | string | `""` | Repo guidance as if loaded from `.auto_review.yaml`. |
| `repo_context` | string | `""` | Pre-rendered RAG context. |
| `expected` | ExpectedFinding[] | `[]` | Ground-truth findings, each `{path, line, note?}`. |

## Labelled fixtures

Labelled fixtures carry an `expected` array describing findings a good reviewer
should surface:

```json
"expected": [
  {
    "path": "src/users.py",
    "line": 14,
    "note": "SQL injection — email is user-controlled and concatenated"
  }
]
```

Matching is by `(path, line)` because wording varies across models. The shipped
labelled fixtures cover SQL injection, command injection, hardcoded secrets, path
traversal, and XSS. Grow the corpus by adding more `labelled-<class>.json`
fixtures with sensitive content removed.

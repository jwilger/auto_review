# `auto_review` benchmark fixtures

The `bench` subcommand of the `auto_review` CLI replays one or more PR
fixtures through the LLM-review path (prompt rendering → reasoning
model → self-heal → optional verifier) and reports per-fixture
finding counts and latency, plus an aggregate over the batch.

By default it's a regression-tracking and model-comparison
harness: run the same fixture set against two models, two prompt
revisions, or before/after a code change, and see how the numbers
move.

Fixtures that include an `expected` array (see *Labelled fixtures*
below) additionally produce precision/recall scores against the
labelled ground truth. Five labelled fixtures ship today, covering
the most common web-app vulnerability classes: `labelled-sql-injection`,
`labelled-command-injection`, `labelled-hardcoded-secret`,
`labelled-path-traversal`, `labelled-xss`. A contract test in
`ar-cli` (`shipped_labelled_fixtures_parse_with_expected_findings`)
asserts each parses cleanly, has a non-empty `expected` array, and
that every expected `path` is actually in the fixture's
`changed_files` list. Adding a new labelled fixture is a matter of
dropping in another `labelled-<class>.json` next to these.

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
| `expected` | ExpectedFinding[] | `[]` | Ground-truth findings the reviewer is expected to surface. Each has `{path, line, note?}`. When present, the harness compares the model's findings to this list by `(path, line)` and contributes to the run's aggregate precision/recall. |

## Labelled fixtures

A labelled fixture carries an `expected` array describing the
findings a *good* reviewer would surface for the diff:

```json
"expected": [
    {
        "path": "src/users.py",
        "line": 14,
        "note": "SQL injection — email is user-controlled and concatenated"
    }
]
```

`note` is for human readers only; matching is by `(path, line)`
because the LLM's exact wording will vary across runs and models.

The aggregate report adds a *Label scoring* section with:

- **expected total** — total expected findings across labelled
  fixtures.
- **matched** — model finding shared `(path, line)` with an
  expected entry. Each expected entry can be matched at most once
  (a duplicate model finding at the same coordinate counts as
  spurious).
- **missed** — expected entries no model finding claimed.
- **spurious** — model findings that don't share `(path, line)`
  with any expected entry.
- **precision** = matched / (matched + spurious).
- **recall** = matched / (matched + missed).

A few starter fixtures live in this directory; the
`labelled-*.json` files are the labelled ones. Adding more
labelled fixtures (with sensitive content removed) is the
intended way to grow a real corpus over time — `(path, line)`
labels are quick to write per PR and the precision/recall numbers
become meaningful as the corpus grows past a few dozen entries.

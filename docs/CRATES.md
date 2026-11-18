# Crate map

The workspace is split into small crates with narrow responsibilities. This file
is the central crate-level navigation aid; operator installation and deployment
instructions live in [Quickstart](./QUICKSTART.md) and [Deployment](./DEPLOYMENT.md).

| Crate | Responsibility |
|---|---|
| `ar-agentcore` | AWS Bedrock AgentCore-compatible HTTP runtime surface, including `/ping` health handling, provider-neutral `/invocations` payload parsing, handler-backed serving, in-memory and DynamoDB invocation idempotency, semantic-review dispatch, and stale-head rejection. |
| `ar-gateway` | HTTP server, Forgejo HMAC and GitHub `X-Hub-Signature-256` verification, Forgejo and GitHub App webhook intake, provider-routed CI-triggered dispatch, Forgejo chat poller, health/readiness/info/metrics routes. |
| `ar-orchestrator` | `JobDispatcher`, production `SpawningDispatcher`, in-memory/SQLite/DynamoDB review history, one pull-request fetch per job to resolve the base branch that repo config is read from, review diff and changed-file list computed locally in the workspace from the merge base (host diff as fallback), incremental diff computed locally from the previously reviewed commit (compare-diff as fallback), changed-file triage, and status calls, and per-PR lifecycle observations for metrics. |
| `ar-forge` | Provider-neutral repository-host DTOs, host error type, `HostCapabilities`, and `ReviewHost` trait for clone URLs, PR summaries, diffs, reviews, issue comments, repo config reads, issue lookups (`get_issue` returning `IssueSummary`, `None` on 404, with an `is_pull_request` marker telling PRs from issues), PR metadata, statuses, host capabilities, and the bot login, shared by Forgejo and GitHub adapters. See [Host capabilities](#host-capabilities) for the per-version matrix. Comment editing goes through `edit_issue_comment` and `edit_review_comment`, whose default implementations return a `HostError`. |
| `ar-forgejo` | Forgejo REST client, webhook DTOs, review/comment/status/webhook APIs, Basic-auth bootstrap client, server-version capability probe and bot-login lookup (see [Host capabilities](#host-capabilities)). `edit_issue_comment` and `edit_review_comment` both `PATCH issues/comments/{id}` (Forgejo v11+ edits inline code comments there too); a `204 No Content` reply means the comment was not edited and is an error. |
| `ar-github` | GitHub App REST client foundation, including app JWT minting, installation-token exchange/cache, `ReviewHost` wrapper, clone URL auth material, PR summary reads, PR and compare-diff reads, changed-file reads, top-level PR comment reads/posts, pull-review and review-comment reads, PR metadata updates, a PR-diff fallback that assembles the diff from the per-file `patch` fields of `pulls/{n}/files` when GitHub answers the diff media type with `406 too_large` (files without a patch, binary or too large, keep a header-only entry; file names holding control characters, `"` or `\` are C-quoted as git does; the fallback logs one INFO line with `repo`, `pr`, `files` and `without_patch`), repo config file reads, issue fetches (`None` on 404, PRs marked), pull request review posting, commit-status posting, `X-Hub-Signature-256` webhook signature verification, typed issue-comment and review-comment webhook payloads, and the long-lived `AppReviewHost` that signs fresh app JWTs, resolves per-repository installations, and mints repository-scoped installation tokens for the gateway. Both hosts report every capability and resolve the `<app slug>[bot]` login (see [Host capabilities](#host-capabilities)). Both `ReviewHost` implementations edit comments with `edit_issue_comment` (`PATCH issues/comments/{id}`) and `edit_review_comment` (`PATCH pulls/comments/{id}`). |
| `ar-llm` | Provider trait, OpenAI-compatible provider, and tier router for `Reasoning`, `Cheap`, and `Embedding`. |
| `ar-index` | Tree-sitter symbols, embeddings, SQLite/in-memory vector stores, and in-memory/SQLite/DynamoDB learnings store. `RepoScope` (lowercased `owner/repo`) scopes learnings: `LearningsStore::add` takes the scope and origin PR, `list` takes a filter (all, one repository, or unscoped), `remove` takes an optional scope (`None` for operator CLI removal), and `query_nearest` only returns learnings in the given scope. A stored repository value that does not parse as `owner/repo` is read as unscoped. |
| `ar-prompts` | Prompt templates, JSON schemas, DTOs, and validators for review and verification LLM calls; `fence_untrusted` and `fence_nonce` are the reusable helpers that wrap untrusted text in nonce-tagged `UNTRUSTED_DATA` markers (with marker neutralization, invisible-character stripping, CRLF normalization and a byte cap) for the review and metadata-check prompts today and for verifiers, chat and later features. |
| `ar-review` | Review activities: provider-supplied clone URL workspace prep, repo config, the trusted base-branch loader for repository-controlled instruction files (`trusted_files`), RAG context, prompt rendering, self-heal, verification, severity filtering, review mapping, and the bounded JSON-action agent loop (`agent_loop::run_agent_loop`) behind the agentic verifier. |
| `ar-chat` | `@<bot>` parser and provider-neutral `ReviewHost` handlers for `help`, `remember`, `forget`, `re-review`, `autofix`, `tests`, and free-form Q&A. |
| `ar-cli` | The `auto-review` binary: gateway and AgentCore entrypoints plus auth, webhook, config, review, bench, ops, history, and learnings commands. |

## Runtime pipeline

```text
Forgejo or GitHub App webhook / CI trigger
  -> ar-gateway
  -> ar-orchestrator job dispatch (one dispatcher per provider)
  -> ar-review workspace/context/review/self-heal/filter/verify pipeline
  -> ReviewHost adapter (ar-forgejo or ar-github) review + commit status
```

LLM calls flow through `ar-llm::Router`. Review prompts and schemas come from
`ar-prompts`. RAG context, persistent learnings, and vector storage come from
`ar-index`. Provider-neutral host DTOs and the `ReviewHost` trait come from
`ar-forge`; Forgejo keeps compatibility DTO re-exports for existing callers
while host-neutral code migrates to the common crate.

`Router::with_inflight_limit(n)` caps simultaneous provider calls across the
router and all its clones (`AR_LLM_MAX_INFLIGHT` in the gateway).
`OpenAiProvider::with_omit_temperature(true)` drops `temperature` from every
chat-completions body whatever the caller set, for OpenAI reasoning models that
reject a non-default temperature. The gateway, `agentcore serve`, `review once`
and `bench run` set it from `LLM_OMIT_TEMPERATURE`.

## `ar-prompts` review contract

The review LLM call is constrained by `crates/ar-prompts/schemas/review.json`
(OpenAI strict mode: every property is listed in `required`) and parsed into
`ReviewOutput` by `validate_review_output`. Each `ReviewFinding` carries
`path`, `line_start`, `line_end`, `severity`, `message`, plus:

- `title`: a short headline, 8 words or fewer. Inline comments show it in bold
  after the severity label; `ar-review` trims it, collapses whitespace and
  control characters to single spaces, and caps it at 120 bytes before
  rendering.
- `category`: exactly one `FindingCategory` from a closed taxonomy, the single
  category set for metrics, noise controls, feedback suppression, fan-out and
  the bench. There is no catch-all value; an unknown or wrong-case category
  fails validation with `ValidationError::SchemaMismatch`, so
  `FindingCategory::as_str` is the only source of category labels.

| Category | Meaning |
|---|---|
| `correctness` | Wrong behaviour, logic errors, broken invariants. |
| `security` | Any security issue, even when it is also a correctness bug. |
| `reliability` | Error handling, panics, retries, resource leaks, races. |
| `performance` | Needless work, allocations, slow queries or algorithms. |
| `compatibility` | Breaking API, schema, config or platform changes. |
| `ci_coverage` | Missing CI linter or check (the CI coverage advisory). |
| `tests` | Missing, weak or incorrect tests. |
| `documentation` | Missing, stale or wrong docs and comments. |
| `refactor` | Structural suggestions such as extracting or merging code. |
| `naming` | Unclear or misleading names. |
| `style` | Formatting and stylistic nits. |

JSON that omits `title` or `category` still deserializes, with `title = ""` and
`category = correctness`, so older fixtures parse and nothing is accidentally
treated as a low-value category. Serialized findings always include both
fields. The review event and severity floor are derived from `severity` only;
`category` does not change them.

## `ar-review` local pull-request diff

`prepare_workspace_from_clone_url(clone_url, head_sha, base_sha,
previous_sha)` clones the head. With `Some(base_sha)` it then, before the
credential-bearing `origin` remote is removed, fetches the base commit and
deepens both tips in the bounded steps of `MERGE_BASE_DEEPEN_STEPS` (64, 256
and 680 commits, together `MERGE_BASE_HISTORY_CAP`, 1000 commits) until
`git merge-base` finds a common ancestor, within `MERGE_BASE_FETCH_TIMEOUT`
(120 seconds); on expiry the running git process is killed. Both SHAs are
validated as hex before git runs. A base that cannot be fetched, no merge base
within the bound, or a timeout still returns the workspace, with
`PreparedWorkspace::merge_base()` `None`. Workspace preparation logs nothing
about it: the diff methods return the reason as a `MergeBaseUnavailable`
(`NotRequested`, `NoPullRequestMergeBase`, `Fetch`, `Deepen`,
`UnrelatedHistories`, `HistoryCap`, `TimedOut`) for the caller to log with its
repository and pull request.

With a merge base, `PreparedWorkspace::pr_diff()` returns the unified diff from
the merge base to the head, the comparison GitHub and Forgejo show for a pull
request, so inline-comment line numbers match. It does not depend on a host's
whole-diff endpoint or its 300-file limit. Binary files appear as
`Binary files ... differ` lines without their content, renames as renames, and
external diff drivers and textconv filters are disabled. At most
`LOCAL_DIFF_MAX_BYTES` (32 MiB) of diff text is read, cut at a line boundary,
and git is stopped there, so a huge pull request is never buffered in full;
`incremental_diff()` has the same bound.
`PreparedWorkspace::pr_changed_files()` returns the same comparison as
`ar_forge::ChangedFile` values in the host's status vocabulary (`added`,
`modified`, `removed`, `renamed`, `copied`, `changed`), binary files with zero
line counts, renamed and copied files with their old path in
`previous_filename` (the field GitHub and Forgejo also return from their
changed-files APIs), and `patch` `None`; no host file-count cap applies. Both read
`.gitattributes` from the merge base, not the head checkout, so a pull request
cannot mark its own text changes as binary (THREAT-MODEL T3a); they pass git's
`--attr-source` option, so git older than 2.42 fails instead of reading the
head's attributes. Without a merge base both return
`WorkspaceError::NoMergeBase` with the reason. The caller still applies
`filter_diff_paths` and `cap_diff` to what reaches the model.
`ar_review::is_valid_git_sha` is the same hex check, exported so callers can
decide before cloning whether a base SHA is usable.

With `Some(previous_sha)` as well, once the merge base is found, it also fetches
the commit an earlier review covered, with the same validation and bounded
deepening. `PreparedWorkspace::incremental_diff()` then returns the diff from
the merge base of that commit and the head to the head, the host's
`previous...head` comparison, with attributes still read from the pull
request's merge base. It returns `WorkspaceError::NoIncrementalBase` with the
reason when the previous commit could not be fetched (for example after a
force push) or no merge base was found.

## `ar-orchestrator` review diff source

`ReviewJob::base_sha` carries the pull request's base commit when the intake
has it: `ReviewJob::from(&PullRequestEvent)` and the gateway's `/reviews/ci`
endpoint fill it; chat `re-review`, the poller, the CLI and AgentCore leave it
`None`. `run_review_job` uses it, or else the base SHA of the pull request it
fetches, to prepare the workspace with
`prepare_workspace_from_clone_url` (a non-hex base clones the head without a
merge base). With a merge base, the full review's diff and
changed-file list come from `PreparedWorkspace::pr_diff()` and
`pr_changed_files()`: the RAG context and `review_pull_request` both use that
diff (`ReviewArgs::diff_override`) and file list
(`ReviewArgs::changed_files_override`), and `ReviewHost::get_pr_diff` and the
pipeline's `list_changed_files` are not called. The pre-clone trivial-files
triage still reads the host's changed-file list. Without a merge base, or when
the local git commands fail, the review fetches the host's diff and file list
as before, after one warning that names the repository, the pull request and
the reason. Incremental reviews also pass the previously reviewed commit to
`prepare_workspace_from_clone_url` and take their diff from
`PreparedWorkspace::incremental_diff()`, so the host's compare endpoint and its
size limit are not used. Without a workspace, or when the previous commit is
not in it, they log the reason at info level (a force push removing the
reviewed commit is expected) and fall back to `ReviewHost::get_compare_diff`,
and when that fails too, to a full review as before. The same-SHA, trivial-files and
disabled-by-config skips still run before the clone. `auto-review review-once`
reviews through `run_review_job` and gets the same behaviour; its `--dry-run`
prompt stays on the Forgejo API diff, which has no file limit. The chat
commands that read the pull request diff have no workspace and stay on
`ReviewHost::get_pr_diff`; on GitHub the `ar-github` per-file patch fallback
covers pull requests over its diff limits.

## Host capabilities

`ReviewHost::capabilities()` (in `ar-forge`) reports which optional
review-thread features the connected host supports, and
`ReviewHost::bot_login()` returns the bot's own login, cached per host
(ADR-0026). The trait defaults report no capabilities and an error for the
login. `ar-forgejo` probes `GET /api/v1/version` and caches a successful
probe per client (`ar_forgejo::capabilities::parse_major_version` reads the
leading major component after an optional `v`); a failed probe is retried on the
next call. It resolves the login through `GET /api/v1/user`. Both requests are
bounded by a 10-second timeout. `ar-github` reports every capability and
resolves `<app slug>[bot]` through `GET /app`, bounded by the same 10-second
timeout (`ar_github::IDENTITY_TIMEOUT`, overridable with
`with_identity_timeout`); a timed-out lookup is not cached. Each host type offers
`with_bot_login` to seed a known login, and hosts built by the gateway are
seeded with the startup bot identity.

| Host | multi-line comments | suggestion blocks | thread replies | resolve threads |
|---|---|---|---|---|
| Forgejo ≤15 (including legacy `1.x` numbering) | no | no | no | no |
| Forgejo 16, 17 | yes | no | no | no |
| Forgejo ≥18 | yes | yes | yes | yes |
| Forgejo, failed or unparseable version probe | no | no | no | no |
| GitHub | yes | yes | yes | yes |

## Test conventions

- Pure helpers get adjacent `#[cfg(test)] mod tests`.
- Forgejo HTTP paths use `wiremock`.
- LLM paths use canned/scripted providers. The shared FIFO
  `ar_llm::testing::ScriptedProvider` (records requests, scripts usage tokens)
  sits behind ar-llm's `test-support` feature; enable it only from
  `[dev-dependencies]`.
- Security boundary claims are pinned by red-team tests referenced from
  [Threat Model](./THREAT-MODEL.md).
- Do not turn prose-only documentation changes into deterministic wording
  tests. Tests may read docs only when the document is generated, consumed by
  tooling, or acts as a public contract such as `docs/CLI.md`; explain that
  contract next to the test.

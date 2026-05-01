# ar-cli

Operator CLI. The single binary `auto_review` exposed by this
crate is the only thing operators need to install on their
workstations. Subcommands cover deploy bootstrap, ongoing
operations, debugging, and benchmark.

## Subcommands

### Deploy bootstrap

| Command | Purpose |
|---------|---------|
| `init` | Mints the bot user's first PAT via Basic auth; prints the one-time secret + suggested env-var. |
| `register-webhook` | Registers a `pull_request` + `issue_comment` webhook on a repo, pointed at the gateway's `/webhooks/forgejo`. |
| `list-webhooks` | Audits webhooks already installed on a repo. |
| `unregister-webhook` | Deletes a webhook by id or by URL substring. |

### Ongoing operations

| Command | Purpose |
|---------|---------|
| `doctor` | Probes outbound deps (Forgejo PAT validity, LLM reachability + model availability) and sanity-checks the webhook secret. Drop into a deploy script. |
| `test-webhook` | HMAC-signed `ping` (or `pull_request`) to a running gateway. Smoke-tests the intake path. |
| `status` | One-screen live snapshot from `/version` + `/info` + `/metrics`. |
| `validate-config` | Parses `.auto_review.yaml` files (with `--strict` to reject unknown top-level keys). |
| `list-linters` | Prints the bundled linter catalogue; `--language <tag>` filter, `--json` for piping. |
| `explain-routing` | Shows which linters route to a given set of files. Useful for tuning `disabled_tools:`. |
| `list-learnings` / `forget-learning` | Direct admin of the persistent learnings store (alternative to `@<bot> remember`/`forget` chat commands). |
| `reset-pr` | Clears the review-history record for one PR so the next webhook triggers a fresh full review. |
| `purge-history` | Drops review-history rows older than N days (long-running-deploy cleanup; wire into a systemd timer). |

### Debugging and benchmark

| Command | Purpose |
|---------|---------|
| `review-once` | Runs the full pipeline against one PR without going through the gateway. Optional `--dry-run` prints the rendered LLM prompt and exits. |
| `bench` | Replays PR fixtures through the LLM-review path. `--baseline FILE` compares against a previous run; `--fail-on-regression` exits non-zero on precision/recall drop > 5pp or p99 jump > 5s. |

## Triad of pre/post-deploy validation

```
auto_review doctor          # outbound deps OK?
auto_review test-webhook    # intake works?
auto_review status          # what's actually running?
```

See [`docs/OPERATIONS.md`](../../docs/OPERATIONS.md) §0 for the
full pre-deploy / post-deploy validation flow.

## Tests

`cargo test -p ar-cli` covers every subcommand's clap parsing
and behavioural path. Several integration tests bring up an
in-process gateway via `axum::serve` to exercise the webhook
+ status + admin paths end-to-end.

## Dependencies

`clap` (derive + env), `reqwest` for the HTTP-talking
subcommands, `rpassword` for `init`'s password prompt,
`hmac` + `sha2` + `hex` for `test-webhook`'s signature
construction, plus inheritance from every other workspace crate
(the CLI is the integration point).

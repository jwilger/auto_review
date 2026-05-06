# ar-cli

Operator CLI and gateway entrypoint. The single public binary
`auto-review` exposed by this crate is the command operators install.
Subcommands are grouped by domain so deploy bootstrap, gateway startup,
ongoing operations, debugging, and benchmarks share one command surface.

## Subcommands

### Top-level groups

| Command | Purpose |
|---------|---------|
| `gateway` | Starts the long-running gateway service; same startup path as the `ar-gateway` binary. |
| `auth` | Authentication and token bootstrap commands. |
| `webhook` | Forgejo webhook registration, auditing, deletion, and smoke tests. |
| `config` | Repository configuration validation. |
| `review` | One-off review execution commands. |
| `bench` | Fixture benchmark commands. |
| `ops` | Operational diagnostics and status. |
| `history` | Review-history maintenance. |
| `learnings` | Learning-store audit and deletion. |

### Deploy bootstrap

| Command | Purpose |
|---------|---------|
| `auth init` | Mints the bot user's first PAT via Basic auth; prints the one-time secret + suggested env-var. |
| `webhook register` | Registers a `pull_request` + `issue_comment` webhook on a repo, pointed at the gateway's `/webhooks/forgejo`. |
| `webhook list` | Audits webhooks already installed on a repo. |
| `webhook unregister` | Deletes a webhook by id or by URL substring. |

### Gateway isolation rollout

Direct `auto-review gateway` startup fails closed while the embedded OCI rootfs
payload is rolled out. The published container image marks its packaged
container boundary with `AR_GATEWAY_EXTERNAL_ISOLATION=container` and startup
logs that the external marker was used. Direct binary operators must explicitly
opt out with `auto-review gateway --bare` or `AR_GATEWAY_BARE=true`, which logs
that only application-level controls are active and this is not
container-equivalent isolation. Full `/info` and `ops doctor` posture reporting
is tracked separately by issue #122.

### Ongoing operations

| Command | Purpose |
|---------|---------|
| `ops doctor` | Probes outbound deps (Forgejo PAT validity, LLM reachability + model availability) and sanity-checks the webhook secret. Drop into a deploy script. |
| `webhook test` | HMAC-signed `ping` (or `pull_request`) to a running gateway. Smoke-tests the intake path. |
| `ops status` | One-screen live snapshot from `/version` + `/info` + `/metrics`. |
| `config validate` | Parses `.auto_review.yaml` files (with `--strict` to reject unknown top-level keys). |
| `learnings list` / `learnings forget` | Direct admin of the persistent learnings store (alternative to `@<bot> remember`/`forget` chat commands). |
| `history reset-pr` | Clears the review-history record for one PR so the next CI-triggered or explicit forced review is a fresh full review. |
| `history purge` | Drops review-history rows older than N days (long-running-deploy cleanup; wire into a systemd timer). |

### Debugging and benchmark

| Command | Purpose |
|---------|---------|
| `review once` | Runs the full pipeline against one PR without going through the gateway. Optional `--dry-run` prints the rendered LLM prompt and exits. |
| `bench run` | Replays PR fixtures through the LLM-review path. `--baseline FILE` compares against a previous run; `--fail-on-regression` exits non-zero on precision/recall drop > 5pp or p99 jump > 5s. |

## Triad of pre/post-deploy validation

```
auto-review ops doctor      # outbound deps OK?
auto-review webhook test    # intake works?
auto-review ops status      # what's actually running?
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
subcommands, `rpassword` for `auth init`'s password prompt,
`hmac` + `sha2` + `hex` for `webhook test`'s signature
construction, plus inheritance from every other workspace crate
(the CLI is the integration point).

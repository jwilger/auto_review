# End-to-End Verification Runbook

Bring up auto_review against a real Forgejo instance and verify the
chain from webhook intake, CI-triggered review dispatch, and posted review. Use
this when:

- Cutting a release.
- Investigating a "review never posts" report from an operator.
- Validating a new Forgejo version (the test matrix in
  docs/OPERATIONS.md tracks which Forgejo majors we've verified).

The synthetic e2e tests at `crates/ar-orchestrator/tests/synthetic_e2e.rs`
cover the review-pipeline wiring bugs (dispatch hand-off and review-comment
payload shape). This runbook covers what only a real
Forgejo can validate: API-shape drift across versions, multi-line
comment quirks (gitea#36231), the actual webhook payload schema,
the CI review endpoint, and the install / PAT / hook-registration flow.

## Prerequisites

- A host with docker (or podman) and ~2 GB free disk for images.
- An LLM endpoint reachable from your shell. Cheapest: an Ollama
  process locally with `qwen2.5-coder:7b` pulled. The runbook
  works with any OpenAI-compatible endpoint.
- The `auto-review` binary built. Either:
  - `nix build .` (recommended; flake-pinned toolchain), then
    refer to `./result/bin/auto-review`.
  - `nix develop --command cargo build --release --workspace`,
    then refer to `./target/release/auto-review`.

Set a helper variable for the rest of the runbook:

```bash
BIN=./result/bin/auto-review
# or: BIN=./target/release/auto-review
CI_TOKEN=$(openssl rand -hex 32)
```

## 1. Boot Forgejo

```bash
docker run -d --name forgejo-e2e \
  -e USER_UID=1000 -e USER_GID=1000 \
  -p 3000:3000 -p 222:22 \
  -v forgejo-e2e:/data \
  codeberg.org/forgejo/forgejo:9
```

Wait for it to be reachable:

```bash
until curl -sf http://localhost:3000/api/v1/version >/dev/null; do
  sleep 1
done
curl -s http://localhost:3000/api/v1/version
# {"version":"9.x.x"}
```

## 2. First-run bootstrap (admin + db)

Forgejo first-run is a web form at `http://localhost:3000/`.
Headless equivalent — POST the install form:

```bash
curl -i -X POST http://localhost:3000/ \
  --data-urlencode "db_type=sqlite3" \
  --data-urlencode "db_path=/data/gitea/gitea.db" \
  --data-urlencode "app_name=auto_review-e2e" \
  --data-urlencode "repo_root_path=/data/git/repositories" \
  --data-urlencode "lfs_root_path=/data/git/lfs" \
  --data-urlencode "log_root_path=/data/gitea/log" \
  --data-urlencode "ssh_port=22" \
  --data-urlencode "http_port=3000" \
  --data-urlencode "app_url=http://localhost:3000/" \
  --data-urlencode "default_branch=main" \
  --data-urlencode "admin_name=admin" \
  --data-urlencode "admin_passwd=adminpass1234" \
  --data-urlencode "admin_confirm_passwd=adminpass1234" \
  --data-urlencode "admin_email=admin@example.com"
```

Forgejo restarts itself after install. Wait again:

```bash
until curl -sf -u admin:adminpass1234 \
    http://localhost:3000/api/v1/users/admin >/dev/null; do
  sleep 1
done
```

## 3. Create the bot user

```bash
curl -X POST http://localhost:3000/api/v1/admin/users \
  -u admin:adminpass1234 \
  -H "Content-Type: application/json" \
  -d '{
    "username": "auto_review_bot",
    "email": "bot@example.com",
    "password": "botpass1234",
    "must_change_password": false
  }'
```

## 4. Mint a PAT for the bot

```bash
PAT=$(curl -s -X POST http://localhost:3000/api/v1/users/auto_review_bot/tokens \
  -u auto_review_bot:botpass1234 \
  -H "Content-Type: application/json" \
  -d '{
    "name": "auto_review-e2e",
    "scopes": ["write:issue","write:repository","read:user"]
  }' | jq -r .sha1)
echo "PAT: $PAT"
```

## 5. Create a test repo with one commit

```bash
curl -X POST http://localhost:3000/api/v1/user/repos \
  -u auto_review_bot:botpass1234 \
  -H "Content-Type: application/json" \
  -d '{"name":"e2e-target","auto_init":true,"default_branch":"main"}'
```

## 6. Register the webhook

The gateway runs at `http://localhost:8080` by default; adjust if
you've bound a different port. The secret you set here MUST match
`WEBHOOK_SECRET` when you boot the gateway.

```bash
curl -X POST http://localhost:3000/api/v1/repos/auto_review_bot/e2e-target/hooks \
  -u auto_review_bot:botpass1234 \
  -H "Content-Type: application/json" \
  -d '{
    "type": "gitea",
    "config": {
      "url": "http://host.docker.internal:8080/webhooks/forgejo",
      "content_type": "json",
      "secret": "shared-secret"
    },
    "events": ["pull_request","issue_comment"],
    "active": true
  }'
```

(`host.docker.internal` resolves the host gateway from inside the
Forgejo container on Docker Desktop. On Linux, use the host LAN IP.)

## 7. Boot the gateway

```bash
FORGEJO_BASE_URL=http://localhost:3000 \
AR_FORGEJO_TOKEN="$PAT" \
WEBHOOK_SECRET=shared-secret \
AR_CI_REVIEW_TOKEN="$CI_TOKEN" \
LLM_BASE_URL=http://localhost:11434 \
LLM_REASONING_MODEL=qwen2.5-coder:7b \
"$BIN" gateway --bare
```

The E2E runbook uses explicit bare mode because it starts the direct binary on
the test host. Bare mode is an opt-out from the embedded OCI launcher; it keeps
application-level controls only and is not container-equivalent isolation.

## 8. Open a PR

```bash
# Add a file via API
curl -X POST http://localhost:3000/api/v1/repos/auto_review_bot/e2e-target/contents/src/main.rs \
  -u auto_review_bot:botpass1234 \
  -H "Content-Type: application/json" \
  -d '{
    "branch": "feature/e2e",
    "new_branch": "feature/e2e",
    "content": "'"$(printf 'fn main() {\n    println!("hello");\n}\n' | base64 -w0)"'",
    "message": "feat: hello"
  }'

# Open the PR and capture its current head SHA for the CI-triggered review.
PR_JSON=$(curl -s -X POST http://localhost:3000/api/v1/repos/auto_review_bot/e2e-target/pulls \
  -u auto_review_bot:botpass1234 \
  -H "Content-Type: application/json" \
  -d '{"title":"e2e","head":"feature/e2e","base":"main","body":"e2e test"}')
PR_NUMBER=$(jq -r .number <<<"$PR_JSON")
HEAD_SHA=$(jq -r .head.sha <<<"$PR_JSON")
```

The PR webhook should be accepted, but it does not dispatch semantic review work
by default. Trigger the normal review path the way a workflow job would after its
prerequisites pass:

```bash
curl -f -X POST http://localhost:8080/reviews/ci \
  -H "Authorization: Bearer $CI_TOKEN" \
  -H "Content-Type: application/json" \
  -d "{\"owner\":\"auto_review_bot\",\"repo\":\"e2e-target\",\"pr_number\":$PR_NUMBER,\"head_sha\":\"$HEAD_SHA\"}"
```

## 9. Verify

Within ~60 seconds (depending on the LLM tier), the gateway logs
should show:

```
INFO review posted repo=auto_review_bot/e2e-target pr=1 review_id=N findings=K
```

And Forgejo's UI should show:

- One review on PR #1 with K inline comments.
- The HEAD SHA's commit status badge → "auto_review: ..." (success
  if K == 0, request_changes if any Error-severity finding).

If any of those don't fire:

1. Check the gateway logs for HMAC mismatches, webhook-parsing errors, CI token
   rejection, or stale `head_sha` conflicts.
2. Hit `/metrics` on the gateway: counters
   `auto_review_webhooks_pull_request_total` should be ≥ 1,
   `auto_review_jobs_dispatched_total` should be ≥ 1 after the `/reviews/ci`
   request, and one of `reviews_succeeded_total` / `reviews_failed_*_total`
   should be ≥ 1.
3. If `reviews_failed_workspace_total` ticked, the clone phase
   failed — check that `host.docker.internal` (or the LAN IP) is
   reachable from the Forgejo container, and that the bot's PAT
   has `write:repository`.

## 10. Tear down

```bash
docker rm -f forgejo-e2e
docker volume rm forgejo-e2e
```

## What this runbook does NOT cover

- **Multi-tenant SaaS auth.** Out of scope per ADR-0001.
- **GitLab / Bitbucket.** Different webhook formats; not
  implemented.
- **CI runner isolation validation.** Deterministic tool execution belongs in
  your CI runner; validate that environment with the runner's own hardening
  tests before allowing untrusted PRs.
- **Performance benchmarks.** A separate corpus replay (TODO).

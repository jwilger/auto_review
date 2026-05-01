# End-to-End Verification Runbook

Bring up auto_review against a real Forgejo instance and verify the
chain from webhook intake through posted review. Use this when:

- Cutting a release.
- Investigating a "review never posts" report from an operator.
- Validating a new Forgejo version (the test matrix in
  docs/OPERATIONS.md tracks which Forgejo majors we've verified).

The synthetic e2e tests at `crates/ar-orchestrator/tests/synthetic_e2e.rs`
cover the wiring bugs (HMAC, webhook parsing, dispatcher hand-off,
review-comment payload shape). This runbook covers what only a real
Forgejo can validate: API-shape drift across versions, multi-line
comment quirks (gitea#36231), the actual webhook payload schema,
and the install / PAT / hook-registration flow.

## Prerequisites

- A host with docker (or podman) and ~2 GB free disk for images.
- An LLM endpoint reachable from your shell. Cheapest: an Ollama
  process locally with `qwen2.5-coder:7b` pulled. The runbook
  works with any OpenAI-compatible endpoint.
- The auto_review binaries built. Either:
  - `nix build .#ar-gateway .#ar-cli` (recommended;
    flake-pinned toolchain), then refer to
    `./result/bin/ar-gateway` and `./result-1/bin/auto_review`.
  - `nix develop --command cargo build --release --workspace`,
    then refer to `./target/release/ar-gateway` and
    `./target/release/auto_review`.

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
`AR_WEBHOOK_SECRET` when you boot the gateway.

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
AR_FORGEJO_BASE=http://localhost:3000 \
AR_FORGEJO_TOKEN="$PAT" \
AR_WEBHOOK_SECRET=shared-secret \
AR_LLM_REASONING_PROVIDER=ollama \
AR_LLM_REASONING_MODEL=qwen2.5-coder:7b \
AR_LLM_OLLAMA_BASE=http://localhost:11434 \
./target/release/ar-gateway
```

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

# Open the PR
curl -X POST http://localhost:3000/api/v1/repos/auto_review_bot/e2e-target/pulls \
  -u auto_review_bot:botpass1234 \
  -H "Content-Type: application/json" \
  -d '{"title":"e2e","head":"feature/e2e","base":"main","body":"e2e test"}'
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

1. Check the gateway logs for HMAC mismatches or webhook-parsing
   errors.
2. Hit `/metrics` on the gateway: counters
   `auto_review_webhooks_pull_request_total` should be ≥ 1, and
   one of `reviews_succeeded_total` / `reviews_failed_*_total`
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
- **The container-escape harness.** That's `cargo test -p
  ar-sandbox --test escape -- --ignored` against your sandbox
  image.
- **Performance benchmarks.** A separate corpus replay (TODO).

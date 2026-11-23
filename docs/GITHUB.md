# GitHub repositories

The gateway service reviews GitHub pull requests through a **GitHub App**. It
can serve GitHub alone, or GitHub alongside Forgejo from the same process.
Review behavior, chat commands, repo config (`.auto_review.yaml`), learnings,
and review history are the same as on Forgejo; only the repository-host adapter
differs.

```text
GitHub App webhook ──> POST /webhooks/github ──> chat commands
                                              └─> check readiness ──> review
```

The gateway never uses a personal access token for GitHub. Each API call uses
an installation access token scoped to the single repository under review. The
gateway mints and caches these tokens from the App's private key.

## 1. Create the GitHub App

Create the App under your organization (**Settings → Developer settings →
GitHub Apps → New GitHub App**) or under your user account.

- **Webhook URL:** `https://<gateway-host>/webhooks/github`
- **Webhook secret:** a fresh random value (`openssl rand -hex 32`). This
  becomes `GITHUB_WEBHOOK_SECRET`.
- **Repository permissions:**

  | Permission | Access | Used for |
  |---|---|---|
  | Contents | Read | clone at the PR head, read `.auto_review.yaml` |
  | Pull requests | Read & write | PR metadata, diffs, reviews, inline comments |
  | Issues | Read & write | PR conversation comments (chat replies) |
  | Checks | Read | wait for GitHub check runs before reviewing |
  | Commit statuses | Read & write | read CI statuses and publish `auto_review` |
  | Metadata | Read | required by GitHub |

- **Subscribe to events:** *Pull request* for automatic review, plus
  *Issue comment* and *Pull request review comment* for chat commands.
  For an existing App, update its settings and have installation owners
  accept the new **Checks: Read** permission before switching over.

After creating the App, note its **App ID** and generate a **private key**
(`.pem`). Install the App on the repositories it should review.

The bot's GitHub login is `<app-slug>[bot]`. Users mention it as
`@<app-slug>`. Name the App `auto-review` to keep the familiar
`@auto-review` handle.

## 2. Configure the gateway

Add the App credentials to the gateway environment, for example
`/etc/auto_review/auto_review.env`:

```sh
GITHUB_APP_ID=123456
GITHUB_APP_PRIVATE_KEY="-----BEGIN RSA PRIVATE KEY-----\n...\n-----END RSA PRIVATE KEY-----"
GITHUB_WEBHOOK_SECRET=<webhook secret from step 1>
LLM_BASE_URL=https://api.openai.com
LLM_API_KEY=<provider key>
LLM_REASONING_MODEL=gpt-4o-mini
# For OpenAI reasoning models (gpt-5.x, gpt-6.x, o-series) also set:
# LLM_OMIT_TEMPERATURE=true
```

| Variable | Required | Notes |
|---|---|---|
| `GITHUB_APP_ID` | yes | Numeric App ID. Setting any of the first three variables enables GitHub and requires all three. |
| `GITHUB_APP_PRIVATE_KEY` | yes | PEM private key. Literal `\n` escapes are converted to newlines. |
| `GITHUB_WEBHOOK_SECRET` | yes | Verifies `X-Hub-Signature-256`. Keep it distinct from Forgejo's `WEBHOOK_SECRET`. |
| `GITHUB_API_URL` | no | Defaults to `https://api.github.com`. For GitHub Enterprise Server use `https://<host>/api/v3`. |
| `GITHUB_BOT_LOGIN` | no | Defaults to `<app-slug>[bot]` from `GET /app`. Used to ignore the bot's own comments and find its prior reviews. |
| `GITHUB_BOT_NAME` | no | Defaults to `<app-slug>`. The `@<handle>` users mention. |

`FORGEJO_BASE_URL`, `AR_FORGEJO_TOKEN`, and `WEBHOOK_SECRET` become optional
once a GitHub App is configured. In a GitHub-only gateway,
`/webhooks/forgejo` answers `404` and the Forgejo chat poller does not run.

At startup the gateway authenticates as the App (`GET /app`) and refuses to
start if the App ID, private key, or API URL is wrong. Setting both
`GITHUB_BOT_LOGIN` and `GITHUB_BOT_NAME` skips that call.

Helm: set `config.githubAppId` (and optionally `config.githubApiUrl`,
`config.githubBotLogin`, `config.githubBotName`) plus
`secrets.githubAppPrivateKey` and `secrets.githubWebhookSecret`, or provide
them in `secrets.secretRef`. `config.forgejoBaseUrl` may then be left empty.

## 3. Verify

```sh
curl -fsS https://<gateway-host>/info | jq '.providers'   # ["github"] or ["forgejo","github"]
curl -fsS https://<gateway-host>/readyz                   # "github reachable (app <slug>)"
```

In the App settings, **Advanced → Recent Deliveries** should show the initial
`ping` delivery answered with `200`.

## 4. Verify automatic review

Open or update a non-draft PR. No review-specific workflow or repository
secret is needed, including for fork PRs. The gateway waits 30 seconds for
CI to register, then reads check runs and commit statuses visible to the
base repository installation on the current PR head and its GitHub merge
commit, when available. It does not start a review while a check is pending,
failing, or unreadable. Successful, skipped, and neutral completed check
runs pass the gate. The App's own `auto_review` status does not count.
If the review waits for a worker, the gateway checks readiness again when
that worker is available.

A successful read with no checks permits review. This cannot account for
checks that register after the review starts or checks reported only on the
base branch (for example, some `pull_request_target` workflows). A known
blocker on a replaced merge commit is not cleared just because the new
merge commit has no checks. The gateway retries blocked revisions with
backoff for up to two hours; a restart clears the in-memory watch. After
that, a new PR update or a maintainer's explicit `@auto-review re-review`
can request a review. That command bypasses CI.

Remove existing review-request steps after confirming App delivery. The
bundled action is now a no-op. Older pinned versions may still call
`/reviews/ci`; valid authenticated GitHub requests are accepted without
starting a review. Keep `AR_CI_REVIEW_TOKEN` and the corresponding workflow
secret while those pinned callers remain, or if Forgejo CI uses the endpoint.

## Chat commands

Mention the bot in a PR conversation comment or in an inline review comment:

```text
@auto-review help
@auto-review re-review
@auto-review remember Prefer thiserror for library error types
```

GitHub delivers inline review-comment webhooks reliably, so the GitHub path
needs no poller. Comments from bot accounts, including the App itself, are
ignored to prevent reply loops. Edited and deleted comments never re-run a
command.

## GitHub-specific behavior

- **Inline comments** anchor to file line numbers on the new side of the diff.
  GitHub rejects a whole review if any inline comment falls outside the diff.
  When that happens, the gateway reposts the review with those comments folded
  into the review body, so no finding is lost.
- **Commit status** uses the `auto_review` context. Descriptions are trimmed
  to GitHub's 140-character limit. Require the check in branch protection to
  gate merges on the review.
- **Review state** values from GitHub (`CHANGES_REQUESTED`, `COMMENTED`) are
  normalized, so override and outstanding-finding logic behaves the same as
  on Forgejo.
- **Clone URLs** come from the repository's `clone_url`, so GitHub.com,
  GitHub Enterprise Server, and GHE.com data-residency hosts all work.

## Running Forgejo and GitHub together

Both providers share the learnings store, review history, vector store,
and metrics. Each provider has its own review
dispatcher, so `AR_REVIEW_CONCURRENCY` caps in-flight reviews per provider.
Review history is keyed by owner, repository, and PR number. Avoid serving an
identically named repository from both forges on one gateway; incremental
review would compare against the other forge's last-reviewed SHA.

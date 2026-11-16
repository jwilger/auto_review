# GitHub repositories

The gateway service reviews GitHub pull requests through a **GitHub App**. It
can serve GitHub alone, or GitHub alongside Forgejo from the same process.
Review behavior, chat commands, repo config (`.auto_review.yaml`), learnings,
and review history are the same as on Forgejo; only the repository-host adapter
differs.

```text
GitHub App webhook ──> POST /webhooks/github ──> chat commands (@auto-review ...)
GitHub Actions job ──> POST /reviews/ci {"provider":"github"} ──> semantic review
                                         │
                                         └─> GitHub App installation token
                                             (one repository, short-lived)
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
  | Commit statuses | Read & write | the `auto_review` status check |
  | Metadata | Read | required by GitHub |

- **Subscribe to events:** *Issue comment* and *Pull request review comment*.
  These carry `@auto-review` chat commands. Pull request events are not
  needed: CI requests semantic review (step 4).

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
AR_CI_REVIEW_TOKEN=<32+ random characters>
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

## 4. Trigger semantic review from GitHub Actions

Semantic review runs after your own CI gates pass (ADR-0012). Add a job that
depends on them and calls the gateway through the bundled action:

```yaml
# .github/workflows/ci.yml
jobs:
  test:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - run: make test

  auto-review:
    if: ${{ github.event_name == 'pull_request' }}
    needs: [test]
    runs-on: ubuntu-latest
    steps:
      - uses: jwilger/auto_review/deploy/github-action@main
        with:
          gateway-url: https://reviewer.example.com
          action-token: ${{ secrets.AUTO_REVIEW_CI_TOKEN }}
```

Store the gateway's `AR_CI_REVIEW_TOKEN` as the `AUTO_REVIEW_CI_TOKEN`
repository or organization secret.

GitHub does not expose repository secrets to `pull_request` workflows from
forks, so fork PRs are not reviewed by this trigger. A maintainer can still
comment `@auto-review re-review` on a fork PR to force a review.

The action posts:

```json
{"provider": "github", "owner": "...", "repo": "...", "pr_number": 7,
 "head_sha": "...", "force": false, "installation_id": 42}
```

`installation_id` is optional; the gateway looks up the installation for the
repository when it is omitted. The gateway fetches the PR, rejects a stale
`head_sha` (`409`), drafts, and closed PRs, then reviews in the background.

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
metrics, and `AR_CI_REVIEW_TOKEN`. Each provider has its own review
dispatcher, so `AR_REVIEW_CONCURRENCY` caps in-flight reviews per provider.
Review history is keyed by owner, repository, and PR number. Avoid serving an
identically named repository from both forges on one gateway; incremental
review would compare against the other forge's last-reviewed SHA.

# ar-forgejo

Forgejo REST client. Hand-rolled on `reqwest` rather than
`forgejo-api` (which lacks the Reviews endpoint). Mirrors the
Gitea-compatible API contract; tested against Forgejo 7+.

## Public surface

| Module | Content |
|--------|---------|
| `client::Client` | Authenticated HTTP wrapper. Constructor takes `base_url + token`. |
| `client::Client` methods | `get_pr_diff`, `list_changed_files`, `create_review`, `post_commit_status`, `create_webhook`, `list_webhooks`, `delete_webhook`, `get_pull_request`, `get_compare_diff`, `post_issue_comment`, `list_pr_review_comments`, `get_server_version`. |
| `init::InitClient` | Basic-auth bootstrap for `auto_review init` (mints the bot's first PAT). |
| `types` | DTOs: `ChangedFile`, `CreateReviewRequest`, `ReviewComment`, `ReviewEvent`, `CommitStatus`, `CreateWebhookRequest`, `WebhookConfig`, `CreatedWebhook`, `WebhookSummary`, `PullRequestSummary`, `PullRequestEvent`, etc. |
| `webhook` | Pull-request and issue-comment webhook payload types (deserialised from Forgejo's JSON). |

## Forgejo specifics

- **Reviews API** — `POST /repos/{o}/{r}/pulls/{n}/reviews` with
  `comments: [{path, body, old_position, new_position}]`. Forgejo
  doesn't carry GitHub's `line` + `side` schema; positions are
  line offsets only. Multi-line ranges are partially supported
  (gitea#36231); the mapping layer in `ar-review` falls back to a
  `**Lines N–M:**` prefix in the body for safety.
- **Webhook delivery** — `pull_request` event covers
  `opened`/`synchronized`/`reopened`/`ready_for_review`. Inline
  review-thread replies don't fire reliably (gitea#26023); the
  gateway's `ChatPoller` covers that gap.
- **Webhook signing** — HMAC-SHA256 over the body, hex-encoded in
  `X-Forgejo-Signature` (with `X-Gitea-Signature` accepted as a
  fallback for older deployments).

## Tests

`cargo test -p ar-forgejo` covers every client method against
wiremock-stubbed Forgejo. See `client.rs` for the canonical
pattern; `wiremock`'s `body_json`, `body_partial_json`, and
`expect(N)` matchers verify both wire-format compliance and
single-call invariants.

## Dependencies

`reqwest`, `serde_json`, `url`. No async-trait — the client is
concrete.

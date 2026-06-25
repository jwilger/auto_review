# AgentCore Deployment Assets

This directory contains operator-owned examples for running `auto_review`
through AWS Bedrock AgentCore without a dedicated gateway host.

The runtime image runs:

```sh
auto-review agentcore serve
```

It listens on port 9000, responds to `/ping`, and accepts provider-neutral
semantic-review and chat-command payloads at `/invocations`. CI jobs invoke the
AgentCore runtime after deterministic repository tests pass. Each invocation
runs inline and returns a structured JSON outcome. Chat-command invocations
include `comment_body` and reuse the gateway's `@auto-review` command parser.

Configure one provider per runtime instance. Forgejo uses `FORGEJO_BASE_URL` and
`AR_FORGEJO_TOKEN`. GitHub uses `GITHUB_APP_ID`, `GITHUB_APP_PRIVATE_KEY`, and
optional `GITHUB_API_URL`; each GitHub invocation payload, including
chat-command payloads, must include `installation_id` so the runtime can mint a
repository-scoped installation token.

AgentCore runtime state can use DynamoDB:

- `AGENTCORE_IDEMPOTENCY_DYNAMODB_TABLE` for duplicate CI invocation claims.
- `AGENTCORE_HISTORY_DYNAMODB_TABLE` for last-reviewed SHA history.
- `AGENTCORE_LEARNINGS_DYNAMODB_TABLE` for remembered guidance.

Forgejo gateway deployment remains supported separately. This path is for
semantic review from CI when no dedicated gateway should be kept online.

Files:

- `Containerfile`: minimal operator-owned runtime image shape.
- `runtime-config.json`: runtime contract sketch for port, health, invocation,
  and required environment names.
- `iam-policy.md`: minimum DynamoDB state-table actions and TTL note.
- `github-actions-oidc.yml`: GitHub Actions OIDC invocation example.
- `forgejo-actions.yml`: Forgejo Actions invocation example.

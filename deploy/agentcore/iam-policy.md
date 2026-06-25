# AgentCore IAM Notes

This directory is an operator-owned deployment example for the no dedicated
gateway path. The runtime container runs `auto-review agentcore serve` on port
9000 and receives CI invocation payloads at `/invocations`.

Grant the runtime role read access to its normal secret source for Forgejo,
GitHub App, and LLM credentials. Do not bake tokens or private keys into the
image.

The DynamoDB tables are selected with:

- `AGENTCORE_IDEMPOTENCY_DYNAMODB_TABLE`
- `AGENTCORE_HISTORY_DYNAMODB_TABLE`
- `AGENTCORE_LEARNINGS_DYNAMODB_TABLE`

The runtime needs these DynamoDB actions on those tables:

```json
{
  "Version": "2012-10-17",
  "Statement": [
    {
      "Sid": "AgentCoreStateTables",
      "Effect": "Allow",
      "Action": [
        "dynamodb:PutItem",
        "dynamodb:UpdateItem",
        "dynamodb:DeleteItem",
        "dynamodb:GetItem",
        "dynamodb:Scan"
      ],
      "Resource": [
        "arn:aws:dynamodb:REGION:ACCOUNT_ID:table/IDEMPOTENCY_TABLE",
        "arn:aws:dynamodb:REGION:ACCOUNT_ID:table/HISTORY_TABLE",
        "arn:aws:dynamodb:REGION:ACCOUNT_ID:table/LEARNINGS_TABLE"
      ]
    }
  ]
}
```

Enable DynamoDB Time To Live on the idempotency table using the `expires_at`
attribute. Review history and learnings intentionally do not use TTL by default.

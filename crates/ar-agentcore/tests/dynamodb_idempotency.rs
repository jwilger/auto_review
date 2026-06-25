use async_trait::async_trait;
use std::sync::{Arc, Mutex};

#[derive(Clone)]
struct FixedClock {
    now: i64,
}

impl ar_agentcore::EpochSecondsClock for FixedClock {
    fn now_epoch_seconds(&self) -> i64 {
        self.now
    }
}

#[derive(Default)]
struct RecordingClient {
    requests: Mutex<Vec<ClaimRequest>>,
    result: Mutex<ar_agentcore::DynamoDbClaimResult>,
}

#[derive(Debug, PartialEq, Eq)]
struct ClaimRequest {
    table_name: String,
    key: String,
    expires_at_epoch_seconds: i64,
}

#[async_trait]
impl ar_agentcore::DynamoDbIdempotencyClient for RecordingClient {
    async fn put_claim(
        &self,
        table_name: &str,
        key: &str,
        expires_at_epoch_seconds: i64,
    ) -> Result<ar_agentcore::DynamoDbClaimResult, ar_agentcore::InvocationIdempotencyError> {
        self.requests.lock().expect("requests").push(ClaimRequest {
            table_name: table_name.to_string(),
            key: key.to_string(),
            expires_at_epoch_seconds,
        });
        Ok(*self.result.lock().expect("result"))
    }
}

#[tokio::test]
async fn dynamodb_idempotency_claim_uses_conditional_put_contract_with_ttl() {
    let client = Arc::new(RecordingClient::default());
    let store = ar_agentcore::DynamoDbInvocationIdempotency::from_parts(
        client.clone(),
        "agentcore-idempotency",
        900,
        Arc::new(FixedClock { now: 1_800_000_000 }),
    );

    assert!(ar_agentcore::InvocationIdempotency::claim(
        &store,
        "forgejo:semantic:alice/widgets#42"
    )
    .await
    .expect("claim"));

    assert_eq!(
        client.requests.lock().expect("requests").as_slice(),
        &[ClaimRequest {
            table_name: "agentcore-idempotency".to_string(),
            key: "forgejo:semantic:alice/widgets#42".to_string(),
            expires_at_epoch_seconds: 1_800_000_900,
        }]
    );

    *client.result.lock().expect("result") = ar_agentcore::DynamoDbClaimResult::Duplicate;
    assert!(!ar_agentcore::InvocationIdempotency::claim(
        &store,
        "forgejo:semantic:alice/widgets#42"
    )
    .await
    .expect("duplicate"));
}

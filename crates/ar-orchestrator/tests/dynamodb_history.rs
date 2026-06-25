use ar_orchestrator::{PrKey, ReviewHistory};
use async_trait::async_trait;
use std::sync::{Arc, Mutex};

#[derive(Clone)]
struct FixedClock {
    now: i64,
}

impl ar_orchestrator::HistoryEpochSecondsClock for FixedClock {
    fn now_epoch_seconds(&self) -> i64 {
        self.now
    }
}

#[derive(Default)]
struct RecordingClient {
    puts: Mutex<Vec<PutRequest>>,
    gets: Mutex<Vec<GetRequest>>,
    deletes: Mutex<Vec<DeleteRequest>>,
    get_result: Mutex<Option<String>>,
    scan_result: Mutex<Vec<PrKey>>,
}

#[derive(Debug, PartialEq)]
struct PutRequest {
    table_name: String,
    pk: String,
    key: PrKey,
    sha: String,
    updated_at_epoch_seconds: i64,
    per_review_cost_usd: f64,
}

#[derive(Debug, PartialEq, Eq)]
struct GetRequest {
    table_name: String,
    pk: String,
}

#[derive(Debug, PartialEq, Eq)]
struct DeleteRequest {
    table_name: String,
    pk: String,
}

#[async_trait]
impl ar_orchestrator::DynamoDbReviewHistoryClient for RecordingClient {
    async fn get_last_reviewed(
        &self,
        table_name: &str,
        pk: &str,
    ) -> Result<Option<String>, ar_orchestrator::HistoryError> {
        self.gets.lock().expect("gets").push(GetRequest {
            table_name: table_name.to_string(),
            pk: pk.to_string(),
        });
        Ok(self.get_result.lock().expect("get result").clone())
    }

    async fn put_reviewed(
        &self,
        table_name: &str,
        pk: &str,
        key: &PrKey,
        sha: &str,
        updated_at_epoch_seconds: i64,
        per_review_cost_usd: f64,
    ) -> Result<(), ar_orchestrator::HistoryError> {
        self.puts.lock().expect("puts").push(PutRequest {
            table_name: table_name.to_string(),
            pk: pk.to_string(),
            key: key.clone(),
            sha: sha.to_string(),
            updated_at_epoch_seconds,
            per_review_cost_usd,
        });
        Ok(())
    }

    async fn delete_reviewed(
        &self,
        table_name: &str,
        pk: &str,
    ) -> Result<(), ar_orchestrator::HistoryError> {
        self.deletes.lock().expect("deletes").push(DeleteRequest {
            table_name: table_name.to_string(),
            pk: pk.to_string(),
        });
        Ok(())
    }

    async fn list_known(
        &self,
        _table_name: &str,
    ) -> Result<Vec<PrKey>, ar_orchestrator::HistoryError> {
        Ok(self.scan_result.lock().expect("scan result").clone())
    }
}

#[tokio::test]
async fn dynamodb_review_history_uses_stable_key_and_timestamp_contract() {
    let client = Arc::new(RecordingClient::default());
    let history = ar_orchestrator::DynamoDbReviewHistory::from_parts(
        client.clone(),
        "agentcore-review-history",
        Arc::new(FixedClock { now: 1_800_000_000 }),
    );
    let key = PrKey {
        owner: "alice".to_string(),
        repo: "widgets".to_string(),
        pr_number: 42,
    };

    history
        .record_with_cost(&key, "abc123", 0.25)
        .await
        .expect("record");
    assert_eq!(
        client.puts.lock().expect("puts").as_slice(),
        &[PutRequest {
            table_name: "agentcore-review-history".to_string(),
            pk: "alice/widgets#42".to_string(),
            key: key.clone(),
            sha: "abc123".to_string(),
            updated_at_epoch_seconds: 1_800_000_000,
            per_review_cost_usd: 0.25,
        }]
    );

    *client.get_result.lock().expect("get result") = Some("abc123".to_string());
    assert_eq!(
        history.last_reviewed(&key).await.expect("last").as_deref(),
        Some("abc123")
    );
    assert_eq!(
        client.gets.lock().expect("gets").as_slice(),
        &[GetRequest {
            table_name: "agentcore-review-history".to_string(),
            pk: "alice/widgets#42".to_string(),
        }]
    );

    *client.scan_result.lock().expect("scan result") = vec![key.clone()];
    assert_eq!(history.list_known().await.expect("list"), vec![key.clone()]);

    history.clear(&key).await.expect("clear");
    assert_eq!(
        client.deletes.lock().expect("deletes").as_slice(),
        &[DeleteRequest {
            table_name: "agentcore-review-history".to_string(),
            pk: "alice/widgets#42".to_string(),
        }]
    );
}

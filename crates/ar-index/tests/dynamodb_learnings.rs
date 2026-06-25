use ar_index::{LearningRecord, LearningSource, LearningsStore};
use async_trait::async_trait;
use std::sync::{Arc, Mutex};

#[derive(Default)]
struct RecordingClient {
    allocated: Mutex<Vec<String>>,
    puts: Mutex<Vec<PutLearning>>,
    removes: Mutex<Vec<RemoveLearning>>,
    records: Mutex<Vec<LearningRecord>>,
    next_id: Mutex<u64>,
    remove_found: Mutex<bool>,
}

#[derive(Debug, PartialEq)]
struct PutLearning {
    table_name: String,
    pk: String,
    record: LearningRecord,
}

#[derive(Debug, PartialEq, Eq)]
struct RemoveLearning {
    table_name: String,
    pk: String,
}

#[async_trait]
impl ar_index::DynamoDbLearningsClient for RecordingClient {
    async fn allocate_id(&self, table_name: &str) -> Result<u64, ar_index::LearningsError> {
        self.allocated
            .lock()
            .expect("allocated")
            .push(table_name.to_string());
        let mut next = self.next_id.lock().expect("next id");
        let id = *next;
        *next += 1;
        Ok(id)
    }

    async fn put_learning(
        &self,
        table_name: &str,
        pk: &str,
        record: &LearningRecord,
    ) -> Result<(), ar_index::LearningsError> {
        self.puts.lock().expect("puts").push(PutLearning {
            table_name: table_name.to_string(),
            pk: pk.to_string(),
            record: record.clone(),
        });
        self.records.lock().expect("records").push(record.clone());
        Ok(())
    }

    async fn list_learnings(
        &self,
        _table_name: &str,
    ) -> Result<Vec<LearningRecord>, ar_index::LearningsError> {
        Ok(self.records.lock().expect("records").clone())
    }

    async fn remove_learning(
        &self,
        table_name: &str,
        pk: &str,
    ) -> Result<bool, ar_index::LearningsError> {
        self.removes.lock().expect("removes").push(RemoveLearning {
            table_name: table_name.to_string(),
            pk: pk.to_string(),
        });
        Ok(*self.remove_found.lock().expect("remove found"))
    }
}

#[tokio::test]
async fn dynamodb_learnings_store_uses_stable_keys_and_trait_behavior() {
    let client = Arc::new(RecordingClient {
        next_id: Mutex::new(7),
        remove_found: Mutex::new(true),
        ..RecordingClient::default()
    });
    let store = ar_index::DynamoDbLearningsStore::from_parts(client.clone(), "agentcore-learnings");

    let record = store
        .add(
            "Prefer ReviewHost over provider clients.".to_string(),
            LearningSource::Guideline,
            vec![0.1, 0.2, 0.3],
            1_800_000_000,
        )
        .await
        .expect("add learning");

    assert_eq!(record.id, 7);
    assert_eq!(
        client.allocated.lock().expect("allocated").as_slice(),
        &["agentcore-learnings".to_string()]
    );
    assert_eq!(
        client.puts.lock().expect("puts").as_slice(),
        &[PutLearning {
            table_name: "agentcore-learnings".to_string(),
            pk: "learning#7".to_string(),
            record: record.clone(),
        }]
    );

    assert_eq!(store.list().await.expect("list"), vec![record.clone()]);
    let nearest = store
        .query_nearest(&[0.1, 0.2, 0.3], 1)
        .await
        .expect("nearest");
    assert_eq!(nearest.len(), 1);
    assert_eq!(nearest[0].learning, record);

    store.remove(7).await.expect("remove");
    assert_eq!(
        client.removes.lock().expect("removes").as_slice(),
        &[RemoveLearning {
            table_name: "agentcore-learnings".to_string(),
            pk: "learning#7".to_string(),
        }]
    );

    *client.remove_found.lock().expect("remove found") = false;
    assert!(matches!(
        store.remove(99).await,
        Err(ar_index::LearningsError::NotFound(99))
    ));
}

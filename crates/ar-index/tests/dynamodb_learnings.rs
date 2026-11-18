//! Integration tests for the `DynamoDB` learnings store through its public client trait.

extern crate alloc;

use alloc::sync::Arc;
use ar_index::{LearningRecord, LearningSource, LearningsFilter, LearningsStore as _, RepoScope};
use async_trait::async_trait;
use std::sync::Mutex;

#[derive(Default)]
struct RecordingClient {
    allocated: Mutex<Vec<String>>,
    next_id: Mutex<u64>,
    puts: Mutex<Vec<PutLearning>>,
    records: Mutex<Vec<LearningRecord>>,
    remove_found: Mutex<bool>,
    removes: Mutex<Vec<RemoveLearning>>,
}

#[derive(Debug, PartialEq)]
#[expect(
    clippy::arbitrary_source_item_ordering,
    reason = "fields follow the client call's argument order (table, key, record), the order the \
              assertions below spell them"
)]
struct PutLearning {
    table_name: String,
    pk: String,
    record: LearningRecord,
}

#[derive(Debug, PartialEq, Eq)]
#[expect(
    clippy::arbitrary_source_item_ordering,
    reason = "fields follow the client call's argument order (table, key), the order the \
              assertions below spell them"
)]
struct RemoveLearning {
    table_name: String,
    pk: String,
}

#[async_trait]
impl ar_index::DynamoDbLearningsClient for RecordingClient {
    #[expect(
        clippy::arithmetic_side_effects,
        reason = "the test double's id counter starts small and is bumped a handful of times, so it cannot overflow"
    )]
    #[expect(
        clippy::expect_used,
        reason = "a failed store, lock or setup call should fail this test at that line with the labelled message"
    )]
    async fn allocate_id(&self, table_name: &str) -> Result<u64, ar_index::LearningsError> {
        self.allocated
            .lock()
            .expect("allocated")
            .push(table_name.to_owned());
        let mut next = self.next_id.lock().expect("next id");
        let id = *next;
        *next += 1;
        Ok(id)
    }

    #[expect(
        clippy::expect_used,
        reason = "a failed store, lock or setup call should fail this test at that line with the labelled message"
    )]
    async fn list_learnings(
        &self,
        _table_name: &str,
    ) -> Result<Vec<LearningRecord>, ar_index::LearningsError> {
        Ok(self.records.lock().expect("records").clone())
    }

    #[expect(
        clippy::expect_used,
        reason = "a failed store, lock or setup call should fail this test at that line with the labelled message"
    )]
    async fn put_learning(
        &self,
        table_name: &str,
        pk: &str,
        record: &LearningRecord,
    ) -> Result<(), ar_index::LearningsError> {
        self.puts.lock().expect("puts").push(PutLearning {
            table_name: table_name.to_owned(),
            pk: pk.to_owned(),
            record: record.clone(),
        });
        self.records.lock().expect("records").push(record.clone());
        Ok(())
    }

    #[expect(
        clippy::expect_used,
        reason = "a failed store, lock or setup call should fail this test at that line with the labelled message"
    )]
    async fn remove_learning(
        &self,
        table_name: &str,
        pk: &str,
    ) -> Result<bool, ar_index::LearningsError> {
        self.removes.lock().expect("removes").push(RemoveLearning {
            table_name: table_name.to_owned(),
            pk: pk.to_owned(),
        });
        Ok(*self.remove_found.lock().expect("remove found"))
    }
}

#[tokio::test]
#[expect(
    clippy::expect_used,
    reason = "a failed store, lock or setup call should fail this test at that line with the labelled message"
)]
#[expect(
    clippy::indexing_slicing,
    reason = "indexes results whose length this test sets up or asserts; an out-of-range index fails the test, as it should"
)]
#[expect(
    clippy::tests_outside_test_module,
    reason = "integration tests in tests/ are their own crate and are not wrapped in a #[cfg(test)] module"
)]
async fn dynamodb_learnings_store_uses_stable_keys_and_trait_behavior() {
    let client = Arc::new(RecordingClient {
        next_id: Mutex::new(7),
        remove_found: Mutex::new(true),
        ..RecordingClient::default()
    });
    let store = ar_index::DynamoDbLearningsStore::from_parts(
        Arc::<RecordingClient>::clone(&client),
        "agentcore-learnings",
    );

    let record = store
        .add(
            "Prefer ReviewHost over provider clients.".to_owned(),
            LearningSource::Guideline,
            vec![0.1, 0.2, 0.3],
            1_800_000_000,
            &RepoScope::new("Acme", "Widget"),
            Some(12),
        )
        .await
        .expect("add learning");

    assert_eq!(record.id, 7);
    assert_eq!(record.repo, Some(RepoScope::new("acme", "widget")));
    assert_eq!(record.origin_pr, Some(12));
    assert_eq!(
        client.allocated.lock().expect("allocated").as_slice(),
        &["agentcore-learnings".to_owned()]
    );
    assert_eq!(
        client.puts.lock().expect("puts").as_slice(),
        &[PutLearning {
            table_name: "agentcore-learnings".to_owned(),
            pk: "learning#7".to_owned(),
            record: record.clone(),
        }]
    );

    assert_eq!(
        store.list(LearningsFilter::All).await.expect("list"),
        vec![record.clone()]
    );
    let nearest = store
        .query_nearest(&[0.1, 0.2, 0.3], 1, &RepoScope::new("acme", "widget"))
        .await
        .expect("nearest");
    assert_eq!(nearest.len(), 1);
    assert_eq!(nearest[0].learning, record);

    store.remove(7, None).await.expect("remove");
    assert_eq!(
        client.removes.lock().expect("removes").as_slice(),
        &[RemoveLearning {
            table_name: "agentcore-learnings".to_owned(),
            pk: "learning#7".to_owned(),
        }]
    );

    *client.remove_found.lock().expect("remove found") = false;
    assert!(matches!(
        store.remove(99, None).await,
        Err(ar_index::LearningsError::NotFound(99))
    ));
}

//! DynamoDB-backed [`LearningsStore`] for `AgentCore` cold-start survival.

use crate::learnings::{
    rank_in_scope, removable_in_scope, LearningRecord, LearningSource, LearningsError,
    LearningsFilter, LearningsStore, ScoredLearning,
};
use crate::repo_scope::{parse_legacy_correction_repo, RepoScope};
use alloc::sync::Arc;
use async_trait::async_trait;
use aws_sdk_dynamodb::types::{AttributeValue, ReturnValue};
use std::collections::HashMap;

#[async_trait]
pub trait DynamoDbLearningsClient: Send + Sync {
    async fn allocate_id(&self, table_name: &str) -> Result<u64, LearningsError>;

    async fn list_learnings(&self, table_name: &str)
        -> Result<Vec<LearningRecord>, LearningsError>;

    async fn put_learning(
        &self,
        table_name: &str,
        pk: &str,
        record: &LearningRecord,
    ) -> Result<(), LearningsError>;

    async fn remove_learning(&self, table_name: &str, pk: &str) -> Result<bool, LearningsError>;
}

/// [`LearningsStore`] kept in one `DynamoDB` table behind a
/// [`DynamoDbLearningsClient`].
pub struct DynamoDbLearningsStore {
    /// Table access, swappable for a fake in tests.
    client: Arc<dyn DynamoDbLearningsClient>,
    /// Name of the table holding the learnings and the id counter.
    table_name: String,
}

impl DynamoDbLearningsStore {
    /// Store over an explicit client, such as a test fake.
    #[inline]
    pub fn from_parts(
        client: Arc<dyn DynamoDbLearningsClient>,
        table_name: impl Into<String>,
    ) -> Self {
        Self {
            client,
            table_name: table_name.into(),
        }
    }

    /// Store over the AWS SDK client for `table_name`.
    #[inline]
    pub fn new(client: aws_sdk_dynamodb::Client, table_name: impl Into<String>) -> Self {
        Self::from_parts(
            Arc::new(AwsDynamoDbLearningsClient::new(client)),
            table_name,
        )
    }
}

#[async_trait]
impl LearningsStore for DynamoDbLearningsStore {
    #[inline]
    async fn add(
        &self,
        text: String,
        source: LearningSource,
        embedding: Vec<f32>,
        now: i64,
        scope: &RepoScope,
        origin_pr: Option<u64>,
    ) -> Result<LearningRecord, LearningsError> {
        let id = self.client.allocate_id(&self.table_name).await?;
        let record = LearningRecord {
            id,
            text,
            source,
            embedding,
            created_at: now,
            repo: Some(scope.clone()),
            origin_pr,
        };
        self.client
            .put_learning(&self.table_name, &learning_pk(id), &record)
            .await?;
        Ok(record)
    }

    #[inline]
    async fn list(&self, filter: LearningsFilter) -> Result<Vec<LearningRecord>, LearningsError> {
        let mut records = self.client.list_learnings(&self.table_name).await?;
        records.retain(|record| filter.matches(record));
        Ok(records)
    }

    #[inline]
    async fn query_nearest(
        &self,
        query: &[f32],
        top_k: usize,
        scope: &RepoScope,
    ) -> Result<Vec<ScoredLearning>, LearningsError> {
        let records = self.client.list_learnings(&self.table_name).await?;
        Ok(rank_in_scope(records, query, top_k, scope))
    }

    #[inline]
    async fn remove(&self, id: u64, scope: Option<&RepoScope>) -> Result<(), LearningsError> {
        if scope.is_some() {
            // A legacy item's scope comes from its text, which a
            // DynamoDB condition expression cannot evaluate, so check
            // ownership on the decoded record before deleting.
            let owned = self
                .client
                .list_learnings(&self.table_name)
                .await?
                .iter()
                .any(|record| record.id == id && removable_in_scope(record, scope));
            if !owned {
                return Err(LearningsError::NotFound(id));
            }
        }
        if !self
            .client
            .remove_learning(&self.table_name, &learning_pk(id))
            .await?
        {
            return Err(LearningsError::NotFound(id));
        }
        Ok(())
    }
}

/// [`DynamoDbLearningsClient`] backed by the AWS SDK.
pub struct AwsDynamoDbLearningsClient {
    /// AWS SDK `DynamoDB` client.
    client: aws_sdk_dynamodb::Client,
}

impl AwsDynamoDbLearningsClient {
    /// Wrap an AWS SDK `DynamoDB` client.
    #[inline]
    #[must_use = "wrapping the client has no side effects; the wrapper is the only result"]
    pub fn new(client: aws_sdk_dynamodb::Client) -> Self {
        Self { client }
    }
}

#[async_trait]
impl DynamoDbLearningsClient for AwsDynamoDbLearningsClient {
    #[inline]
    async fn allocate_id(&self, table_name: &str) -> Result<u64, LearningsError> {
        let response = self
            .client
            .update_item()
            .table_name(table_name)
            .key("pk", AttributeValue::S("__counter".to_owned()))
            .update_expression("ADD next_id :one")
            .expression_attribute_values(":one", AttributeValue::N("1".to_owned()))
            .return_values(ReturnValue::UpdatedNew)
            .send()
            .await
            .map_err(|error| LearningsError::Storage(format!("allocate learning id: {error}")))?;
        let attributes = response
            .attributes
            .as_ref()
            .ok_or_else(|| LearningsError::Storage("missing next_id".to_owned()))?;
        attr_u64(attributes, "next_id")
    }

    #[inline]
    async fn list_learnings(
        &self,
        table_name: &str,
    ) -> Result<Vec<LearningRecord>, LearningsError> {
        let response = self
            .client
            .scan()
            .table_name(table_name)
            .filter_expression("begins_with(pk, :learning_prefix)")
            .expression_attribute_values(
                ":learning_prefix",
                AttributeValue::S("learning#".to_owned()),
            )
            .send()
            .await
            .map_err(|error| {
                LearningsError::Storage(format!("scan DynamoDB learnings: {error}"))
            })?;
        let Some(items) = response.items else {
            return Ok(Vec::new());
        };
        items
            .iter()
            .map(item_to_learning)
            .collect::<Result<Vec<_>, _>>()
    }

    #[inline]
    async fn put_learning(
        &self,
        table_name: &str,
        pk: &str,
        record: &LearningRecord,
    ) -> Result<(), LearningsError> {
        self.client
            .put_item()
            .table_name(table_name)
            .set_item(Some(learning_to_item(pk, record)))
            .send()
            .await
            .map_err(|error| LearningsError::Storage(format!("put DynamoDB learning: {error}")))?;
        Ok(())
    }

    #[inline]
    async fn remove_learning(&self, table_name: &str, pk: &str) -> Result<bool, LearningsError> {
        let response = self
            .client
            .delete_item()
            .table_name(table_name)
            .key("pk", AttributeValue::S(pk.to_owned()))
            .return_values(ReturnValue::AllOld)
            .send()
            .await
            .map_err(|error| {
                LearningsError::Storage(format!("delete DynamoDB learning: {error}"))
            })?;
        Ok(response
            .attributes
            .as_ref()
            .is_some_and(|attributes| !attributes.is_empty()))
    }
}

/// Partition key of learning `id`.
fn learning_pk(id: u64) -> String {
    format!("learning#{id}")
}

/// String attribute `name`, or `None` when absent or not a string.
fn attr_string(item: &HashMap<String, AttributeValue>, name: &str) -> Option<String> {
    item.get(name)
        .and_then(|attribute| attribute.as_s().ok())
        .cloned()
}

/// Number attribute `name` parsed as `u64`.
fn attr_u64(item: &HashMap<String, AttributeValue>, name: &str) -> Result<u64, LearningsError> {
    match item.get(name).map(AttributeValue::as_n) {
        Some(Ok(value)) => value
            .parse::<u64>()
            .map_err(|error| LearningsError::Storage(format!("invalid {name}: {error}"))),
        _ => Err(LearningsError::Storage(format!("missing {name}"))),
    }
}

/// Number attribute `name` parsed as `i64`.
fn attr_i64(item: &HashMap<String, AttributeValue>, name: &str) -> Result<i64, LearningsError> {
    match item.get(name).map(AttributeValue::as_n) {
        Some(Ok(value)) => value
            .parse::<i64>()
            .map_err(|error| LearningsError::Storage(format!("invalid {name}: {error}"))),
        _ => Err(LearningsError::Storage(format!("missing {name}"))),
    }
}

/// Binary attribute `name` as raw bytes.
fn attr_blob(
    item: &HashMap<String, AttributeValue>,
    name: &str,
) -> Result<Vec<u8>, LearningsError> {
    match item.get(name).map(AttributeValue::as_b) {
        Some(Ok(value)) => Ok(value.clone().into_inner()),
        _ => Err(LearningsError::Storage(format!("missing {name}"))),
    }
}

/// `DynamoDB` attribute map for `record`. `repo` is the lowercased
/// `owner/repo` string; `origin_pr` is omitted when unknown.
fn learning_to_item(pk: &str, record: &LearningRecord) -> HashMap<String, AttributeValue> {
    let mut item = HashMap::from([
        ("pk".to_owned(), AttributeValue::S(pk.to_owned())),
        ("id".to_owned(), AttributeValue::N(record.id.to_string())),
        ("text".to_owned(), AttributeValue::S(record.text.clone())),
        (
            "source".to_owned(),
            AttributeValue::S(source_to_str(record.source).to_owned()),
        ),
        (
            "embedding".to_owned(),
            AttributeValue::B(vec_f32_to_blob(&record.embedding).into()),
        ),
        (
            "created_at".to_owned(),
            AttributeValue::N(record.created_at.to_string()),
        ),
    ]);
    if let Some(repo) = record.repo.as_ref() {
        item.insert("repo".to_owned(), AttributeValue::S(repo.to_string()));
    }
    if let Some(pr) = record.origin_pr {
        item.insert("origin_pr".to_owned(), AttributeValue::N(pr.to_string()));
    }
    item
}

/// Decode an item. Items written before learnings were scoped have no
/// `repo` attribute; their scope comes from a legacy
/// `Repository <owner>/<repo> only.` text prefix, or they stay unscoped.
/// An item whose `repo` attribute does not parse is unscoped too.
fn item_to_learning(
    item: &HashMap<String, AttributeValue>,
) -> Result<LearningRecord, LearningsError> {
    let source = attr_string(item, "source")
        .ok_or_else(|| LearningsError::Storage("missing source".to_owned()))
        .and_then(|source| source_from_str(&source))?;
    let text = attr_string(item, "text")
        .ok_or_else(|| LearningsError::Storage("missing text".to_owned()))?;
    let repo = match attr_string(item, "repo") {
        // An unparseable `repo` makes the item unscoped rather than
        // failing the read, so one bad item cannot break every query;
        // the SQLite store treats such rows the same way.
        Some(repo) => {
            let scope = RepoScope::parse(&repo);
            if scope.is_none() {
                tracing::warn!(
                    pk = attr_string(item, "pk").unwrap_or_default(),
                    repo = %repo,
                    "learning has an unparseable repo; treating it as unscoped"
                );
            }
            scope
        }
        None => parse_legacy_correction_repo(&text),
    };
    let origin_pr = match item.get("origin_pr") {
        Some(_) => Some(attr_u64(item, "origin_pr")?),
        None => None,
    };
    Ok(LearningRecord {
        id: attr_u64(item, "id")?,
        text,
        source,
        embedding: blob_to_vec_f32(&attr_blob(item, "embedding")?),
        created_at: attr_i64(item, "created_at")?,
        repo,
        origin_pr,
    })
}

/// Stable `source` attribute value for each [`LearningSource`].
fn source_to_str(source: LearningSource) -> &'static str {
    match source {
        LearningSource::Chat => "chat",
        LearningSource::Guideline => "guideline",
        LearningSource::Inferred => "inferred",
    }
}

/// Parse the `source` attribute written by [`source_to_str`].
fn source_from_str(source: &str) -> Result<LearningSource, LearningsError> {
    match source {
        "chat" => Ok(LearningSource::Chat),
        "guideline" => Ok(LearningSource::Guideline),
        "inferred" => Ok(LearningSource::Inferred),
        other => Err(LearningsError::Storage(format!(
            "unknown source variant in DynamoDB: {other}"
        ))),
    }
}

/// Encode an embedding as a little-endian f32 binary attribute.
#[expect(
    clippy::arithmetic_side_effects,
    reason = "the input slice already occupies len * 4 bytes in memory, so the byte count cannot \
              overflow usize"
)]
#[expect(
    clippy::little_endian_bytes,
    reason = "stored embedding attributes are little-endian f32, the same format as the SQLite \
              store; writing them any other way would make existing items unreadable"
)]
fn vec_f32_to_blob(values: &[f32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(values.len() * 4);
    for value in values {
        out.extend_from_slice(&value.to_le_bytes());
    }
    out
}

/// Decode a little-endian f32 binary attribute written by
/// [`vec_f32_to_blob`]. Trailing bytes that do not fill a whole f32 are
/// ignored.
#[expect(
    clippy::little_endian_bytes,
    reason = "stored embedding attributes are little-endian f32; reading them any other way \
              would misdecode existing items"
)]
fn blob_to_vec_f32(bytes: &[u8]) -> Vec<f32> {
    bytes
        .as_chunks::<4>()
        .0
        .iter()
        .map(|chunk| f32::from_le_bytes(*chunk))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// Fake client that keeps raw attribute maps, so records pass
    /// through the same encode/decode as the AWS client and legacy
    /// items can be seeded directly.
    #[derive(Default)]
    struct ItemClient {
        items: Mutex<Vec<HashMap<String, AttributeValue>>>,
        next_id: Mutex<u64>,
    }

    impl ItemClient {
        #[expect(
            clippy::unwrap_used,
            reason = "a failed store, lock or setup call should fail this test at that line; unwrap reports the error"
        )]
        fn seed_legacy(&self, id: u64, text: &str) {
            let mut item = learning_to_item(
                &learning_pk(id),
                &LearningRecord {
                    id,
                    text: text.into(),
                    source: LearningSource::Inferred,
                    embedding: vec![1.0, 0.0],
                    created_at: 0,
                    repo: None,
                    origin_pr: None,
                },
            );
            item.remove("repo");
            self.items.lock().unwrap().push(item);
            let mut next = self.next_id.lock().unwrap();
            *next = (*next).max(id);
        }
    }

    #[async_trait]
    impl DynamoDbLearningsClient for ItemClient {
        #[expect(
            clippy::arithmetic_side_effects,
            reason = "the test double's id counter starts small and is bumped a handful of times, so it cannot overflow"
        )]
        #[expect(
            clippy::unwrap_used,
            reason = "a failed store, lock or setup call should fail this test at that line; unwrap reports the error"
        )]
        async fn allocate_id(&self, _table_name: &str) -> Result<u64, LearningsError> {
            let mut next = self.next_id.lock().unwrap();
            *next += 1;
            Ok(*next)
        }

        #[expect(
            clippy::unwrap_used,
            reason = "a failed store, lock or setup call should fail this test at that line; unwrap reports the error"
        )]
        async fn list_learnings(
            &self,
            _table_name: &str,
        ) -> Result<Vec<LearningRecord>, LearningsError> {
            self.items
                .lock()
                .unwrap()
                .iter()
                .map(item_to_learning)
                .collect()
        }

        #[expect(
            clippy::unwrap_used,
            reason = "a failed store, lock or setup call should fail this test at that line; unwrap reports the error"
        )]
        async fn put_learning(
            &self,
            _table_name: &str,
            pk: &str,
            record: &LearningRecord,
        ) -> Result<(), LearningsError> {
            self.items
                .lock()
                .unwrap()
                .push(learning_to_item(pk, record));
            Ok(())
        }

        #[expect(
            clippy::unwrap_used,
            reason = "a failed store, lock or setup call should fail this test at that line; unwrap reports the error"
        )]
        async fn remove_learning(
            &self,
            _table_name: &str,
            pk: &str,
        ) -> Result<bool, LearningsError> {
            let mut items = self.items.lock().unwrap();
            let before = items.len();
            items.retain(|item| attr_string(item, "pk").as_deref() != Some(pk));
            Ok(items.len() != before)
        }
    }

    fn store() -> (Arc<ItemClient>, DynamoDbLearningsStore) {
        let client = Arc::new(ItemClient::default());
        let store =
            DynamoDbLearningsStore::from_parts(Arc::<ItemClient>::clone(&client), "learnings");
        (client, store)
    }

    /// One learning in each of two scopes plus one unscoped legacy item
    /// (id 1), all with the same embedding.
    #[expect(
        clippy::unwrap_used,
        reason = "a failed store, lock or setup call should fail this test at that line; unwrap reports the error"
    )]
    async fn two_scopes_and_legacy() -> (DynamoDbLearningsStore, LearningRecord, LearningRecord) {
        let (client, store) = store();
        client.seed_legacy(1, "Prefer Result over panics.");
        let in_a = store
            .add(
                "from a".into(),
                LearningSource::Chat,
                vec![1.0, 0.0],
                0,
                &RepoScope::new("acme", "a"),
                Some(3),
            )
            .await
            .unwrap();
        let in_b = store
            .add(
                "from b".into(),
                LearningSource::Chat,
                vec![1.0, 0.0],
                0,
                &RepoScope::new("acme", "b"),
                None,
            )
            .await
            .unwrap();
        (store, in_a, in_b)
    }

    #[tokio::test]
    #[expect(
        clippy::indexing_slicing,
        reason = "indexes results whose length this test sets up or asserts; an out-of-range index fails the test, as it should"
    )]
    #[expect(
        clippy::semicolon_outside_block,
        reason = "the restriction group also enables the contradicting semicolon_inside_block, so this guard-scoping block keeps its original statement-inside form"
    )]
    #[expect(
        clippy::unwrap_used,
        reason = "a failed store, lock or setup call should fail this test at that line; unwrap reports the error"
    )]
    async fn new_items_carry_repo_and_origin_pr_attributes() {
        let (client, store) = store();
        let scope = RepoScope::new("Acme", "Widget");
        let with_pr = store
            .add(
                "x".into(),
                LearningSource::Chat,
                vec![1.0],
                0,
                &scope,
                Some(42),
            )
            .await
            .unwrap();
        store
            .add("y".into(), LearningSource::Chat, vec![1.0], 0, &scope, None)
            .await
            .unwrap();
        {
            let items = client.items.lock().unwrap();
            assert_eq!(
                attr_string(&items[0], "repo").as_deref(),
                Some("acme/widget")
            );
            assert!(matches!(items[0].get("origin_pr"), Some(AttributeValue::N(n)) if n == "42"));
            assert!(!items[1].contains_key("origin_pr"));
        }
        let listed = store.list(LearningsFilter::All).await.unwrap();
        assert_eq!(listed[0], with_pr);
        assert_eq!(listed[0].repo, Some(RepoScope::new("acme", "widget")));
        assert_eq!(listed[0].origin_pr, Some(42));
        assert_eq!(listed[1].origin_pr, None);
    }

    #[tokio::test]
    #[expect(
        clippy::indexing_slicing,
        reason = "indexes results whose length this test sets up or asserts; an out-of-range index fails the test, as it should"
    )]
    #[expect(
        clippy::unwrap_used,
        reason = "a failed store, lock or setup call should fail this test at that line; unwrap reports the error"
    )]
    async fn legacy_item_with_correction_prefix_is_scoped_at_read_time() {
        let (client, store) = store();
        client.seed_legacy(
            5,
            "Repository Acme/Widget only. Review correction from PR feedback: x",
        );
        let hits = store
            .query_nearest(&[1.0, 0.0], 5, &RepoScope::new("acme", "widget"))
            .await
            .unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].learning.id, 5);
        assert_eq!(
            hits[0].learning.repo,
            Some(RepoScope::new("acme", "widget"))
        );
    }

    #[tokio::test]
    #[expect(
        clippy::assert_is_empty,
        reason = "the emptiness assertion stays as written; this lint-only change leaves assertions unedited"
    )]
    #[expect(
        clippy::min_ident_chars,
        reason = "short bindings such as `r`, `s`, `a` and `b` are read by this test's assertions; renaming them would edit those assertions"
    )]
    #[expect(
        clippy::unwrap_used,
        reason = "a failed store, lock or setup call should fail this test at that line; unwrap reports the error"
    )]
    async fn legacy_item_without_prefix_is_unscoped_and_never_retrieved() {
        let (client, store) = store();
        client.seed_legacy(5, "Prefer Result over panics.");
        let hits = store
            .query_nearest(&[1.0, 0.0], 5, &RepoScope::new("acme", "widget"))
            .await
            .unwrap();
        assert!(hits.is_empty());
        let unscoped = store.list(LearningsFilter::Unscoped).await.unwrap();
        assert_eq!(unscoped.iter().map(|r| r.id).collect::<Vec<_>>(), vec![5]);
    }

    #[tokio::test]
    #[expect(
        clippy::assert_is_empty,
        reason = "the emptiness assertion stays as written; this lint-only change leaves assertions unedited"
    )]
    #[expect(
        clippy::indexing_slicing,
        reason = "indexes results whose length this test sets up or asserts; an out-of-range index fails the test, as it should"
    )]
    #[expect(
        clippy::min_ident_chars,
        reason = "short bindings such as `r`, `s`, `a` and `b` are read by this test's assertions; renaming them would edit those assertions"
    )]
    #[expect(
        clippy::unwrap_used,
        reason = "a failed store, lock or setup call should fail this test at that line; unwrap reports the error"
    )]
    async fn item_with_unparseable_repo_is_unscoped_and_does_not_fail_queries() {
        let (client, store) = store();
        client.seed_legacy(5, "hand edited");
        client.items.lock().unwrap()[0].insert(
            "repo".to_owned(),
            AttributeValue::S("not-a-scope".to_owned()),
        );
        let hits = store
            .query_nearest(&[1.0, 0.0], 5, &RepoScope::new("acme", "widget"))
            .await
            .unwrap();
        assert!(hits.is_empty());
        let unscoped = store.list(LearningsFilter::Unscoped).await.unwrap();
        assert_eq!(unscoped.iter().map(|r| r.id).collect::<Vec<_>>(), vec![5]);
    }

    #[tokio::test]
    #[expect(
        clippy::min_ident_chars,
        reason = "short bindings such as `r`, `s`, `a` and `b` are read by this test's assertions; renaming them would edit those assertions"
    )]
    #[expect(
        clippy::unwrap_used,
        reason = "a failed store, lock or setup call should fail this test at that line; unwrap reports the error"
    )]
    async fn query_nearest_excludes_other_scopes_and_unscoped_records() {
        let (store, a, _) = two_scopes_and_legacy().await;
        let r = store
            .query_nearest(&[1.0, 0.0], 5, &RepoScope::new("Acme", "A"))
            .await
            .unwrap();
        assert_eq!(
            r.iter().map(|s| s.learning.id).collect::<Vec<_>>(),
            vec![a.id]
        );
    }

    #[tokio::test]
    #[expect(
        clippy::expect_used,
        reason = "a failed store, lock or setup call should fail this test at that line with the labelled message"
    )]
    #[expect(
        clippy::min_ident_chars,
        reason = "short bindings such as `r`, `s`, `a` and `b` are read by this test's assertions; renaming them would edit those assertions"
    )]
    #[expect(
        clippy::unwrap_used,
        reason = "a failed store, lock or setup call should fail this test at that line; unwrap reports the error"
    )]
    async fn scoped_remove_only_deletes_records_of_that_scope() {
        let (store, a, b) = two_scopes_and_legacy().await;
        let scope_b = RepoScope::new("acme", "b");
        for id in [a.id, 1, 999] {
            let err = store
                .remove(id, Some(&scope_b))
                .await
                .expect_err("not found");
            assert!(matches!(err, LearningsError::NotFound(n) if n == id));
        }
        assert_eq!(store.list(LearningsFilter::All).await.unwrap().len(), 3);
        store.remove(b.id, Some(&scope_b)).await.unwrap();
        store.remove(1, None).await.unwrap();
        assert_eq!(store.list(LearningsFilter::All).await.unwrap(), vec![a]);
    }

    #[tokio::test]
    #[expect(
        clippy::min_ident_chars,
        reason = "short bindings such as `r`, `s`, `a` and `b` are read by this test's assertions; renaming them would edit those assertions"
    )]
    #[expect(
        clippy::unwrap_used,
        reason = "a failed store, lock or setup call should fail this test at that line; unwrap reports the error"
    )]
    async fn list_filters_by_scope() {
        let (store, _, b) = two_scopes_and_legacy().await;
        assert_eq!(
            store
                .list(LearningsFilter::Repo(RepoScope::new("acme", "b")))
                .await
                .unwrap(),
            vec![b]
        );
        let unscoped = store.list(LearningsFilter::Unscoped).await.unwrap();
        assert_eq!(unscoped.iter().map(|r| r.id).collect::<Vec<_>>(), vec![1]);
        assert_eq!(store.list(LearningsFilter::All).await.unwrap().len(), 3);
    }
}

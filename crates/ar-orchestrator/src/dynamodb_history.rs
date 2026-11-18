//! DynamoDB-backed [`ReviewHistory`] for `AgentCore` cold-start survival.

use crate::review_history::{HistoryError, PrKey, ReviewHistory};
use alloc::sync::Arc;
use async_trait::async_trait;
use aws_sdk_dynamodb::types::AttributeValue;
use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

#[async_trait]
pub trait DynamoDbReviewHistoryClient: Send + Sync {
    async fn delete_reviewed(&self, table_name: &str, pk: &str) -> Result<(), HistoryError>;

    async fn get_last_reviewed(
        &self,
        table_name: &str,
        pk: &str,
    ) -> Result<Option<String>, HistoryError>;

    async fn list_known(&self, table_name: &str) -> Result<Vec<PrKey>, HistoryError>;

    async fn put_reviewed(
        &self,
        table_name: &str,
        pk: &str,
        key: &PrKey,
        sha: &str,
        updated_at_epoch_seconds: i64,
        per_review_cost_usd: f64,
    ) -> Result<(), HistoryError>;
}

pub trait HistoryEpochSecondsClock: Send + Sync {
    fn now_epoch_seconds(&self) -> i64;
}

/// [`HistoryEpochSecondsClock`] backed by the system wall clock.
#[derive(Default)]
#[expect(
    clippy::exhaustive_structs,
    reason = "a stateless unit struct that callers construct by name; #[non_exhaustive] would \
              stop dependents from writing SystemHistoryEpochSecondsClock as a value"
)]
pub struct SystemHistoryEpochSecondsClock;

impl HistoryEpochSecondsClock for SystemHistoryEpochSecondsClock {
    #[inline]
    fn now_epoch_seconds(&self) -> i64 {
        match SystemTime::now().duration_since(UNIX_EPOCH) {
            Ok(duration) => i64::try_from(duration.as_secs()).unwrap_or(i64::MAX),
            Err(_) => 0,
        }
    }
}

/// [`ReviewHistory`] stored in a `DynamoDB` table, one item per PR.
pub struct DynamoDbReviewHistory {
    /// Table operations, real or fake.
    client: Arc<dyn DynamoDbReviewHistoryClient>,
    /// Source of the `updated_at` timestamp written with each record.
    clock: Arc<dyn HistoryEpochSecondsClock>,
    /// Name of the `DynamoDB` table.
    table_name: String,
}

impl DynamoDbReviewHistory {
    /// Build a history from an explicit client and clock.
    #[inline]
    pub fn from_parts(
        client: Arc<dyn DynamoDbReviewHistoryClient>,
        table_name: impl Into<String>,
        clock: Arc<dyn HistoryEpochSecondsClock>,
    ) -> Self {
        Self {
            client,
            clock,
            table_name: table_name.into(),
        }
    }

    /// Build a history over the AWS SDK client and the system clock.
    #[inline]
    pub fn new(client: aws_sdk_dynamodb::Client, table_name: impl Into<String>) -> Self {
        Self::from_parts(
            Arc::new(AwsDynamoDbReviewHistoryClient::new(client)),
            table_name,
            Arc::new(SystemHistoryEpochSecondsClock),
        )
    }
}

#[async_trait]
impl ReviewHistory for DynamoDbReviewHistory {
    #[inline]
    async fn clear(&self, key: &PrKey) -> Result<(), HistoryError> {
        self.client
            .delete_reviewed(&self.table_name, &history_pk(key))
            .await
    }

    #[inline]
    async fn last_reviewed(&self, key: &PrKey) -> Result<Option<String>, HistoryError> {
        self.client
            .get_last_reviewed(&self.table_name, &history_pk(key))
            .await
    }

    #[inline]
    async fn list_known(&self) -> Result<Vec<PrKey>, HistoryError> {
        self.client.list_known(&self.table_name).await
    }

    #[inline]
    async fn record(&self, key: &PrKey, sha: &str) -> Result<(), HistoryError> {
        self.record_with_cost(key, sha, 0.0).await
    }

    #[inline]
    async fn record_with_cost(
        &self,
        key: &PrKey,
        sha: &str,
        per_review_cost_usd: f64,
    ) -> Result<(), HistoryError> {
        self.client
            .put_reviewed(
                &self.table_name,
                &history_pk(key),
                key,
                sha,
                self.clock.now_epoch_seconds(),
                per_review_cost_usd,
            )
            .await
    }
}

/// [`DynamoDbReviewHistoryClient`] that talks to `DynamoDB` through the AWS SDK.
pub struct AwsDynamoDbReviewHistoryClient {
    /// AWS SDK `DynamoDB` client.
    client: aws_sdk_dynamodb::Client,
}

impl AwsDynamoDbReviewHistoryClient {
    /// Wrap an AWS SDK `DynamoDB` client.
    #[inline]
    #[must_use = "the client does nothing until a DynamoDbReviewHistory uses it"]
    pub fn new(client: aws_sdk_dynamodb::Client) -> Self {
        Self { client }
    }
}

#[async_trait]
impl DynamoDbReviewHistoryClient for AwsDynamoDbReviewHistoryClient {
    #[inline]
    async fn delete_reviewed(&self, table_name: &str, pk: &str) -> Result<(), HistoryError> {
        self.client
            .delete_item()
            .table_name(table_name)
            .key("pk", AttributeValue::S(pk.to_owned()))
            .send()
            .await
            .map_err(|error| {
                HistoryError::Storage(format!("delete DynamoDB review history: {error}"))
            })?;
        Ok(())
    }

    #[inline]
    async fn get_last_reviewed(
        &self,
        table_name: &str,
        pk: &str,
    ) -> Result<Option<String>, HistoryError> {
        let response = self
            .client
            .get_item()
            .table_name(table_name)
            .key("pk", AttributeValue::S(pk.to_owned()))
            .consistent_read(true)
            .send()
            .await
            .map_err(|error| {
                HistoryError::Storage(format!("get DynamoDB review history: {error}"))
            })?;
        Ok(response
            .item
            .as_ref()
            .and_then(|item| attr_string(item, "head_sha")))
    }

    #[inline]
    async fn list_known(&self, table_name: &str) -> Result<Vec<PrKey>, HistoryError> {
        let response = self
            .client
            .scan()
            .table_name(table_name)
            .projection_expression("#owner, repo, pr_number")
            .expression_attribute_names("#owner", "owner")
            .send()
            .await
            .map_err(|error| {
                HistoryError::Storage(format!("scan DynamoDB review history: {error}"))
            })?;
        let Some(items) = response.items else {
            return Ok(Vec::new());
        };
        items.iter().map(item_to_key).collect::<Result<Vec<_>, _>>()
    }

    #[inline]
    async fn put_reviewed(
        &self,
        table_name: &str,
        pk: &str,
        key: &PrKey,
        sha: &str,
        updated_at_epoch_seconds: i64,
        per_review_cost_usd: f64,
    ) -> Result<(), HistoryError> {
        self.client
            .put_item()
            .table_name(table_name)
            .item("pk", AttributeValue::S(pk.to_owned()))
            .item("owner", AttributeValue::S(key.owner.clone()))
            .item("repo", AttributeValue::S(key.repo.clone()))
            .item("pr_number", AttributeValue::N(key.pr_number.to_string()))
            .item("head_sha", AttributeValue::S(sha.to_owned()))
            .item(
                "updated_at",
                AttributeValue::N(updated_at_epoch_seconds.to_string()),
            )
            .item(
                "per_review_cost_usd",
                AttributeValue::N(per_review_cost_usd.to_string()),
            )
            .send()
            .await
            .map_err(|error| {
                HistoryError::Storage(format!("put DynamoDB review history: {error}"))
            })?;
        Ok(())
    }
}

/// Partition key for a PR: `owner/repo#number`.
fn history_pk(key: &PrKey) -> String {
    format!("{}/{}#{}", key.owner, key.repo, key.pr_number)
}

/// The string value of attribute `name`, if it is present and a string.
fn attr_string(item: &HashMap<String, AttributeValue>, name: &str) -> Option<String> {
    item.get(name).and_then(|attr| attr.as_s().ok()).cloned()
}

/// The numeric attribute `name` parsed as `u64`.
fn attr_u64(item: &HashMap<String, AttributeValue>, name: &str) -> Result<u64, HistoryError> {
    match item.get(name).and_then(|attr| attr.as_n().ok()) {
        Some(value) => value
            .parse::<u64>()
            .map_err(|error| HistoryError::Storage(format!("invalid {name}: {error}"))),
        None => Err(HistoryError::Storage(format!("missing {name}"))),
    }
}

/// Rebuild a [`PrKey`] from a scanned history item.
fn item_to_key(item: &HashMap<String, AttributeValue>) -> Result<PrKey, HistoryError> {
    Ok(PrKey {
        owner: attr_string(item, "owner")
            .ok_or_else(|| HistoryError::Storage("missing owner".to_owned()))?,
        repo: attr_string(item, "repo")
            .ok_or_else(|| HistoryError::Storage("missing repo".to_owned()))?,
        pr_number: attr_u64(item, "pr_number")?,
    })
}

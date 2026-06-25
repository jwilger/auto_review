//! DynamoDB-backed [`ReviewHistory`] for AgentCore cold-start survival.

use crate::review_history::{HistoryError, PrKey, ReviewHistory};
use async_trait::async_trait;
use aws_sdk_dynamodb::types::AttributeValue;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

#[async_trait]
pub trait DynamoDbReviewHistoryClient: Send + Sync {
    async fn get_last_reviewed(
        &self,
        table_name: &str,
        pk: &str,
    ) -> Result<Option<String>, HistoryError>;

    async fn put_reviewed(
        &self,
        table_name: &str,
        pk: &str,
        key: &PrKey,
        sha: &str,
        updated_at_epoch_seconds: i64,
        per_review_cost_usd: f64,
    ) -> Result<(), HistoryError>;

    async fn delete_reviewed(&self, table_name: &str, pk: &str) -> Result<(), HistoryError>;

    async fn list_known(&self, table_name: &str) -> Result<Vec<PrKey>, HistoryError>;
}

pub trait HistoryEpochSecondsClock: Send + Sync {
    fn now_epoch_seconds(&self) -> i64;
}

#[derive(Default)]
pub struct SystemHistoryEpochSecondsClock;

impl HistoryEpochSecondsClock for SystemHistoryEpochSecondsClock {
    fn now_epoch_seconds(&self) -> i64 {
        match SystemTime::now().duration_since(UNIX_EPOCH) {
            Ok(duration) => duration.as_secs().min(i64::MAX as u64) as i64,
            Err(_) => 0,
        }
    }
}

pub struct DynamoDbReviewHistory {
    client: Arc<dyn DynamoDbReviewHistoryClient>,
    table_name: String,
    clock: Arc<dyn HistoryEpochSecondsClock>,
}

impl DynamoDbReviewHistory {
    pub fn new(client: aws_sdk_dynamodb::Client, table_name: impl Into<String>) -> Self {
        Self::from_parts(
            Arc::new(AwsDynamoDbReviewHistoryClient::new(client)),
            table_name,
            Arc::new(SystemHistoryEpochSecondsClock),
        )
    }

    pub fn from_parts(
        client: Arc<dyn DynamoDbReviewHistoryClient>,
        table_name: impl Into<String>,
        clock: Arc<dyn HistoryEpochSecondsClock>,
    ) -> Self {
        Self {
            client,
            table_name: table_name.into(),
            clock,
        }
    }
}

#[async_trait]
impl ReviewHistory for DynamoDbReviewHistory {
    async fn last_reviewed(&self, key: &PrKey) -> Result<Option<String>, HistoryError> {
        self.client
            .get_last_reviewed(&self.table_name, &history_pk(key))
            .await
    }

    async fn record(&self, key: &PrKey, sha: &str) -> Result<(), HistoryError> {
        self.record_with_cost(key, sha, 0.0).await
    }

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

    async fn clear(&self, key: &PrKey) -> Result<(), HistoryError> {
        self.client
            .delete_reviewed(&self.table_name, &history_pk(key))
            .await
    }

    async fn list_known(&self) -> Result<Vec<PrKey>, HistoryError> {
        self.client.list_known(&self.table_name).await
    }
}

pub struct AwsDynamoDbReviewHistoryClient {
    client: aws_sdk_dynamodb::Client,
}

impl AwsDynamoDbReviewHistoryClient {
    pub fn new(client: aws_sdk_dynamodb::Client) -> Self {
        Self { client }
    }
}

#[async_trait]
impl DynamoDbReviewHistoryClient for AwsDynamoDbReviewHistoryClient {
    async fn get_last_reviewed(
        &self,
        table_name: &str,
        pk: &str,
    ) -> Result<Option<String>, HistoryError> {
        let response = self
            .client
            .get_item()
            .table_name(table_name)
            .key("pk", AttributeValue::S(pk.to_string()))
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
            .item("pk", AttributeValue::S(pk.to_string()))
            .item("owner", AttributeValue::S(key.owner.clone()))
            .item("repo", AttributeValue::S(key.repo.clone()))
            .item("pr_number", AttributeValue::N(key.pr_number.to_string()))
            .item("head_sha", AttributeValue::S(sha.to_string()))
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

    async fn delete_reviewed(&self, table_name: &str, pk: &str) -> Result<(), HistoryError> {
        self.client
            .delete_item()
            .table_name(table_name)
            .key("pk", AttributeValue::S(pk.to_string()))
            .send()
            .await
            .map_err(|error| {
                HistoryError::Storage(format!("delete DynamoDB review history: {error}"))
            })?;
        Ok(())
    }

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
}

fn history_pk(key: &PrKey) -> String {
    format!("{}/{}#{}", key.owner, key.repo, key.pr_number)
}

fn attr_string(item: &HashMap<String, AttributeValue>, name: &str) -> Option<String> {
    match item.get(name) {
        Some(AttributeValue::S(value)) => Some(value.clone()),
        _ => None,
    }
}

fn attr_u64(item: &HashMap<String, AttributeValue>, name: &str) -> Result<u64, HistoryError> {
    match item.get(name) {
        Some(AttributeValue::N(value)) => value
            .parse::<u64>()
            .map_err(|error| HistoryError::Storage(format!("invalid {name}: {error}"))),
        _ => Err(HistoryError::Storage(format!("missing {name}"))),
    }
}

fn item_to_key(item: &HashMap<String, AttributeValue>) -> Result<PrKey, HistoryError> {
    Ok(PrKey {
        owner: attr_string(item, "owner")
            .ok_or_else(|| HistoryError::Storage("missing owner".to_string()))?,
        repo: attr_string(item, "repo")
            .ok_or_else(|| HistoryError::Storage("missing repo".to_string()))?,
        pr_number: attr_u64(item, "pr_number")?,
    })
}

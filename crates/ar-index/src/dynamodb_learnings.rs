//! DynamoDB-backed [`LearningsStore`] for AgentCore cold-start survival.

use crate::learnings::{
    LearningRecord, LearningSource, LearningsError, LearningsStore, ScoredLearning,
};
use async_trait::async_trait;
use aws_sdk_dynamodb::types::{AttributeValue, ReturnValue};
use std::collections::HashMap;
use std::sync::Arc;

#[async_trait]
pub trait DynamoDbLearningsClient: Send + Sync {
    async fn allocate_id(&self, table_name: &str) -> Result<u64, LearningsError>;

    async fn put_learning(
        &self,
        table_name: &str,
        pk: &str,
        record: &LearningRecord,
    ) -> Result<(), LearningsError>;

    async fn list_learnings(&self, table_name: &str)
        -> Result<Vec<LearningRecord>, LearningsError>;

    async fn remove_learning(&self, table_name: &str, pk: &str) -> Result<bool, LearningsError>;
}

pub struct DynamoDbLearningsStore {
    client: Arc<dyn DynamoDbLearningsClient>,
    table_name: String,
}

impl DynamoDbLearningsStore {
    pub fn new(client: aws_sdk_dynamodb::Client, table_name: impl Into<String>) -> Self {
        Self::from_parts(
            Arc::new(AwsDynamoDbLearningsClient::new(client)),
            table_name,
        )
    }

    pub fn from_parts(
        client: Arc<dyn DynamoDbLearningsClient>,
        table_name: impl Into<String>,
    ) -> Self {
        Self {
            client,
            table_name: table_name.into(),
        }
    }
}

#[async_trait]
impl LearningsStore for DynamoDbLearningsStore {
    async fn add(
        &self,
        text: String,
        source: LearningSource,
        embedding: Vec<f32>,
        now: i64,
    ) -> Result<LearningRecord, LearningsError> {
        let id = self.client.allocate_id(&self.table_name).await?;
        let record = LearningRecord {
            id,
            text,
            source,
            embedding,
            created_at: now,
        };
        self.client
            .put_learning(&self.table_name, &learning_pk(id), &record)
            .await?;
        Ok(record)
    }

    async fn list(&self) -> Result<Vec<LearningRecord>, LearningsError> {
        self.client.list_learnings(&self.table_name).await
    }

    async fn remove(&self, id: u64) -> Result<(), LearningsError> {
        if !self
            .client
            .remove_learning(&self.table_name, &learning_pk(id))
            .await?
        {
            return Err(LearningsError::NotFound(id));
        }
        Ok(())
    }

    async fn query_nearest(
        &self,
        query: &[f32],
        top_k: usize,
    ) -> Result<Vec<ScoredLearning>, LearningsError> {
        let mut scored: Vec<ScoredLearning> = self
            .list()
            .await?
            .into_iter()
            .map(|learning| ScoredLearning {
                score: cosine_similarity(query, &learning.embedding),
                learning,
            })
            .collect();
        scored.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        scored.truncate(top_k);
        Ok(scored)
    }
}

pub struct AwsDynamoDbLearningsClient {
    client: aws_sdk_dynamodb::Client,
}

impl AwsDynamoDbLearningsClient {
    pub fn new(client: aws_sdk_dynamodb::Client) -> Self {
        Self { client }
    }
}

#[async_trait]
impl DynamoDbLearningsClient for AwsDynamoDbLearningsClient {
    async fn allocate_id(&self, table_name: &str) -> Result<u64, LearningsError> {
        let response = self
            .client
            .update_item()
            .table_name(table_name)
            .key("pk", AttributeValue::S("__counter".to_string()))
            .update_expression("ADD next_id :one")
            .expression_attribute_values(":one", AttributeValue::N("1".to_string()))
            .return_values(ReturnValue::UpdatedNew)
            .send()
            .await
            .map_err(|error| LearningsError::Storage(format!("allocate learning id: {error}")))?;
        let attributes = response
            .attributes
            .as_ref()
            .ok_or_else(|| LearningsError::Storage("missing next_id".to_string()))?;
        attr_u64(attributes, "next_id")
    }

    async fn put_learning(
        &self,
        table_name: &str,
        pk: &str,
        record: &LearningRecord,
    ) -> Result<(), LearningsError> {
        self.client
            .put_item()
            .table_name(table_name)
            .item("pk", AttributeValue::S(pk.to_string()))
            .item("id", AttributeValue::N(record.id.to_string()))
            .item("text", AttributeValue::S(record.text.clone()))
            .item(
                "source",
                AttributeValue::S(source_to_str(record.source).to_string()),
            )
            .item(
                "embedding",
                AttributeValue::B(vec_f32_to_blob(&record.embedding).into()),
            )
            .item(
                "created_at",
                AttributeValue::N(record.created_at.to_string()),
            )
            .send()
            .await
            .map_err(|error| LearningsError::Storage(format!("put DynamoDB learning: {error}")))?;
        Ok(())
    }

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
                AttributeValue::S("learning#".to_string()),
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

    async fn remove_learning(&self, table_name: &str, pk: &str) -> Result<bool, LearningsError> {
        let response = self
            .client
            .delete_item()
            .table_name(table_name)
            .key("pk", AttributeValue::S(pk.to_string()))
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

fn learning_pk(id: u64) -> String {
    format!("learning#{id}")
}

fn attr_string(item: &HashMap<String, AttributeValue>, name: &str) -> Option<String> {
    match item.get(name) {
        Some(AttributeValue::S(value)) => Some(value.clone()),
        _ => None,
    }
}

fn attr_u64(item: &HashMap<String, AttributeValue>, name: &str) -> Result<u64, LearningsError> {
    match item.get(name) {
        Some(AttributeValue::N(value)) => value
            .parse::<u64>()
            .map_err(|error| LearningsError::Storage(format!("invalid {name}: {error}"))),
        _ => Err(LearningsError::Storage(format!("missing {name}"))),
    }
}

fn attr_i64(item: &HashMap<String, AttributeValue>, name: &str) -> Result<i64, LearningsError> {
    match item.get(name) {
        Some(AttributeValue::N(value)) => value
            .parse::<i64>()
            .map_err(|error| LearningsError::Storage(format!("invalid {name}: {error}"))),
        _ => Err(LearningsError::Storage(format!("missing {name}"))),
    }
}

fn attr_blob(
    item: &HashMap<String, AttributeValue>,
    name: &str,
) -> Result<Vec<u8>, LearningsError> {
    match item.get(name) {
        Some(AttributeValue::B(value)) => Ok(value.clone().into_inner()),
        _ => Err(LearningsError::Storage(format!("missing {name}"))),
    }
}

fn item_to_learning(
    item: &HashMap<String, AttributeValue>,
) -> Result<LearningRecord, LearningsError> {
    let source = attr_string(item, "source")
        .ok_or_else(|| LearningsError::Storage("missing source".to_string()))
        .and_then(|source| source_from_str(&source))?;
    Ok(LearningRecord {
        id: attr_u64(item, "id")?,
        text: attr_string(item, "text")
            .ok_or_else(|| LearningsError::Storage("missing text".to_string()))?,
        source,
        embedding: blob_to_vec_f32(&attr_blob(item, "embedding")?),
        created_at: attr_i64(item, "created_at")?,
    })
}

fn source_to_str(source: LearningSource) -> &'static str {
    match source {
        LearningSource::Chat => "chat",
        LearningSource::Guideline => "guideline",
        LearningSource::Inferred => "inferred",
    }
}

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

fn vec_f32_to_blob(values: &[f32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(values.len() * 4);
    for value in values {
        out.extend_from_slice(&value.to_le_bytes());
    }
    out
}

fn blob_to_vec_f32(bytes: &[u8]) -> Vec<f32> {
    bytes
        .chunks_exact(4)
        .map(|chunk| f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]))
        .collect()
}

fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    let dot: f32 = a.iter().zip(b.iter()).map(|(x, y)| x * y).sum();
    let norm_a: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let norm_b: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm_a == 0.0 || norm_b == 0.0 {
        return 0.0;
    }
    dot / (norm_a * norm_b)
}

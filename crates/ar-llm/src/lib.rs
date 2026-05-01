//! LLM provider trait + tiered routing.
//!
//! Triage runs on a cheap tier, review on a reasoning tier, embeddings on
//! their own tier. Config maps each tier to a concrete provider.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelTier {
    Cheap,
    Reasoning,
    Embedding,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompleteRequest {
    pub system: Option<String>,
    pub messages: Vec<Message>,
    pub max_tokens: Option<u32>,
    pub temperature: Option<f32>,
    pub response_format: Option<ResponseFormat>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    User,
    Assistant,
    Tool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    pub content: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ResponseFormat {
    Text,
    JsonSchema { schema: serde_json::Value },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompleteResponse {
    pub content: String,
    pub input_tokens: u32,
    pub output_tokens: u32,
}

#[async_trait]
pub trait LlmProvider: Send + Sync {
    async fn complete(&self, req: CompleteRequest) -> anyhow::Result<CompleteResponse>;
    async fn embed(&self, texts: &[String]) -> anyhow::Result<Vec<Vec<f32>>>;
    fn tier(&self) -> ModelTier;
}

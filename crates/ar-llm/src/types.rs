use async_trait::async_trait;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelTier {
    Cheap,
    Embedding,
    Reasoning,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    Assistant,
    Tool,
    User,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[expect(
    clippy::exhaustive_structs,
    reason = "ar-review (heal) builds Message by struct literal; #[non_exhaustive] \
              would break that dependent"
)]
#[expect(
    clippy::arbitrary_source_item_ordering,
    reason = "field order is the serialized JSON key order (role before content, as on the \
              OpenAI wire); reordering changes the serialized message"
)]
pub struct Message {
    pub role: Role,
    pub content: String,
}

impl Message {
    #[inline]
    pub fn assistant(content: impl Into<String>) -> Self {
        Self {
            role: Role::Assistant,
            content: content.into(),
        }
    }

    #[inline]
    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: Role::User,
            content: content.into(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ResponseFormat {
    /// JSON Schema constraint. Providers that support `response_format` use it
    /// natively; others fall back to a strict prompt instruction.
    JsonSchema {
        name: String,
        schema: serde_json::Value,
    },
    Text,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[expect(
    clippy::exhaustive_structs,
    reason = "dependents build CompleteRequest by struct literal with ..Default::default(); \
              #[non_exhaustive] would break every call site"
)]
#[expect(
    clippy::arbitrary_source_item_ordering,
    reason = "field order is the serialized JSON key order of a request; reordering changes the \
              serialized request"
)]
pub struct CompleteRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system: Option<String>,
    pub messages: Vec<Message>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_format: Option<ResponseFormat>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[expect(
    clippy::exhaustive_structs,
    reason = "ar-review, ar-chat and ar-orchestrator (including provider fakes) build \
              CompleteResponse by struct literal; #[non_exhaustive] would break them"
)]
pub struct CompleteResponse {
    pub content: String,
    #[serde(default)]
    pub input_tokens: u32,
    #[serde(default)]
    pub output_tokens: u32,
}

#[derive(Debug, thiserror::Error)]
#[expect(
    clippy::error_impl_error,
    reason = "public name imported crate-wide as ar_llm::Error; renaming it changes the crate API"
)]
pub enum Error {
    #[error("decode error: {0}")]
    Decode(String),
    #[error("HTTP error: {0}")]
    Http(#[from] reqwest::Error),
    #[error("invalid base URL: {0}")]
    InvalidBaseUrl(String),
    #[error("no provider configured for tier {0:?}")]
    NoProvider(ModelTier),
    #[error("provider error {status}: {body}")]
    Provider { status: u16, body: String },
    #[error("operation not supported by provider")]
    Unsupported,
}

#[async_trait]
pub trait LlmProvider: Send + Sync {
    async fn complete(&self, req: CompleteRequest) -> Result<CompleteResponse, Error>;

    #[inline]
    fn completion_model_name(&self) -> String {
        "completion-model".to_owned()
    }

    #[inline]
    async fn embed(&self, _texts: &[String]) -> Result<Vec<Vec<f32>>, Error> {
        Err(Error::Unsupported)
    }

    #[inline]
    async fn embed_with_usage(&self, texts: &[String]) -> Result<(Vec<Vec<f32>>, u32, u32), Error> {
        let vectors = self.embed(texts).await?;
        Ok((vectors, 0, 0))
    }

    #[inline]
    fn embedding_model_name(&self) -> String {
        "embedding-model".to_owned()
    }

    #[inline]
    fn provider_base_url(&self) -> String {
        String::new()
    }

    #[inline]
    fn provider_model_name(&self, tier: ModelTier) -> String {
        match tier {
            ModelTier::Embedding => self.embedding_model_name(),
            ModelTier::Cheap | ModelTier::Reasoning => self.completion_model_name(),
        }
    }
}

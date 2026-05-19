use async_trait::async_trait;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelTier {
    Cheap,
    Reasoning,
    Embedding,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
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

impl Message {
    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: Role::User,
            content: content.into(),
        }
    }
    pub fn assistant(content: impl Into<String>) -> Self {
        Self {
            role: Role::Assistant,
            content: content.into(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ResponseFormat {
    Text,
    /// JSON Schema constraint. Providers that support `response_format` use it
    /// natively; others fall back to a strict prompt instruction.
    JsonSchema {
        name: String,
        schema: serde_json::Value,
    },
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
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
pub struct CompleteResponse {
    pub content: String,
    #[serde(default)]
    pub input_tokens: u32,
    #[serde(default)]
    pub output_tokens: u32,
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("HTTP error: {0}")]
    Http(#[from] reqwest::Error),
    #[error("invalid base URL: {0}")]
    InvalidBaseUrl(String),
    #[error("provider error {status}: {body}")]
    Provider { status: u16, body: String },
    #[error("decode error: {0}")]
    Decode(String),
    #[error("operation not supported by provider")]
    Unsupported,
    #[error("no provider configured for tier {0:?}")]
    NoProvider(ModelTier),
}

#[async_trait]
pub trait LlmProvider: Send + Sync {
    async fn complete(&self, req: CompleteRequest) -> Result<CompleteResponse, Error>;

    async fn embed(&self, _texts: &[String]) -> Result<Vec<Vec<f32>>, Error> {
        Err(Error::Unsupported)
    }

    fn provider_base_url(&self) -> String {
        String::new()
    }

    fn completion_model_name(&self) -> String {
        "completion-model".to_string()
    }

    fn embedding_model_name(&self) -> String {
        "embedding-model".to_string()
    }

    fn provider_model_name(&self, tier: ModelTier) -> String {
        match tier {
            ModelTier::Embedding => self.embedding_model_name(),
            ModelTier::Cheap | ModelTier::Reasoning => self.completion_model_name(),
        }
    }
}

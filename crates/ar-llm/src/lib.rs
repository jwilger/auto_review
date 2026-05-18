//! LLM provider abstraction.
//!
//! A provider is bound to one model + base URL; the [`Router`] maps tiers
//! ([`ModelTier::Cheap`], [`ModelTier::Reasoning`], [`ModelTier::Embedding`])
//! to concrete provider instances. Triage and summarization run on the cheap
//! tier; review and verification run on the reasoning tier; embeddings have
//! their own tier.

pub mod openai;
pub mod pricing;
pub mod router;
pub mod types;

pub use openai::OpenAiProvider;
pub use router::Router;
pub use types::{
    CompleteRequest, CompleteResponse, Error, LlmProvider, Message, ModelTier, ResponseFormat, Role,
};

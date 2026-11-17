//! LLM provider abstraction.
//!
//! A provider is bound to one model + base URL; the [`Router`] maps tiers
//! ([`ModelTier::Cheap`], [`ModelTier::Reasoning`], [`ModelTier::Embedding`])
//! to concrete provider instances. Triage and summarization run on the cheap
//! tier; review and verification run on the reasoning tier; embeddings have
//! their own tier.

extern crate alloc;

pub mod openai;
pub mod pricing;
pub mod router;
#[cfg(any(test, feature = "test-support"))]
pub mod testing;
pub mod types;

#[expect(
    clippy::useless_attribute,
    reason = "clippy flags any lint attribute on a use item outside its fixed list, which omits \
              pub_use, yet the pub_use expectation below is what suppresses that lint here"
)]
#[expect(
    clippy::pub_use,
    reason = "ar-review, ar-index, ar-chat, ar-orchestrator, ar-gateway and ar-cli import the \
              provider, router and request types from the crate root; dropping the re-exports \
              breaks them"
)]
pub use crate::{
    openai::OpenAiProvider,
    router::Router,
    types::{
        CompleteRequest, CompleteResponse, Error, LlmProvider, Message, ModelTier, ResponseFormat,
        Role,
    },
};

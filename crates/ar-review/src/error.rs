//! Error type shared by the review pipeline activities.

use ar_prompts::ValidationError;

/// Failure of a review pipeline activity.
#[derive(Debug, thiserror::Error)]
#[expect(
    clippy::module_name_repetitions,
    reason = "ar-orchestrator (dispatcher) and ar-gateway (metrics) import this as \
              ar_review::ReviewError; renaming it breaks those crates"
)]
pub enum ReviewError {
    /// The Forgejo client failed.
    #[error("Forgejo error: {0}")]
    Forgejo(#[from] ar_forgejo::Error),
    /// The provider-neutral repository host failed.
    #[error("repository host error: {0}")]
    Host(#[from] ar_forge::HostError),
    /// The LLM provider failed.
    #[error("LLM error: {0}")]
    Llm(#[from] ar_llm::Error),
    /// Every self-heal attempt produced output that failed schema validation.
    #[error("LLM produced unhealable output after {attempts} attempts; last error: {last_error}")]
    Unhealable {
        /// Number of generation attempts made.
        attempts: u32,
        /// Validation error of the final attempt.
        last_error: ValidationError,
    },
    /// Preparing or reading the review workspace failed.
    #[error("workspace error: {0}")]
    Workspace(String),
}

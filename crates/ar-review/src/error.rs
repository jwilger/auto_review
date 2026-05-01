use ar_prompts::ValidationError;

#[derive(Debug, thiserror::Error)]
pub enum ReviewError {
    #[error("LLM error: {0}")]
    Llm(#[from] ar_llm::Error),
    #[error("Forgejo error: {0}")]
    Forgejo(#[from] ar_forgejo::Error),
    #[error("LLM produced unhealable output after {attempts} attempts; last error: {last_error}")]
    Unhealable {
        attempts: u32,
        last_error: ValidationError,
    },
    #[error("workspace error: {0}")]
    Workspace(String),
}

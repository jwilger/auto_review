//! Prompt templates and JSON schema for the review pipeline.
//!
//! Two responsibilities:
//! 1. Provide the strict JSON Schema the LLM emits its review against, plus a
//!    typed [`ReviewOutput`] for downstream activities.
//! 2. Render the user prompt that ships diff + PR context to the LLM.

pub mod fence;
pub mod prompt;
pub mod schema;
pub mod triage;
pub mod types;
pub mod validate;
pub mod verification;

pub use fence::{fence_nonce, fence_untrusted, process_nonce_salt, FENCE_MARKER_PREFIX};
pub use prompt::{
    render_review_prompt, system_prompt, ReviewPromptInputs, PR_BODY_MAX_BYTES, PR_TITLE_MAX_BYTES,
};
pub use schema::review_schema;
pub use triage::{
    triage_schema, triage_system_prompt, validate_triage_output, TriageClass, TriageEntry,
    TriageOutput, TriageValidationError,
};
pub use types::{ReviewFinding, ReviewOutput, ReviewSeverity};
pub use validate::{validate_review_output, ValidationError};
pub use verification::{
    validate_verification_output, verification_schema, verification_system_prompt,
    VerificationOutput, VerificationValidationError, VerificationVerdict,
};

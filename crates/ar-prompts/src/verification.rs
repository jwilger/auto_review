//! Verification prompt + JSON schema. A second-pass cheap-tier
//! check that drops findings the diff doesn't actually corroborate.
//! Runs after the reasoning-tier review and before the post step.

use serde::{Deserialize, Serialize};
use std::sync::OnceLock;

/// Source text of the verification JSON Schema, embedded at compile time.
const VERIFICATION_SCHEMA_JSON: &str = include_str!("../schemas/verification.json");

/// System prompt for the verifier model.
const VERIFICATION_SYSTEM_PROMPT: &str = "\
You are a verifier for code-review findings. Each finding cites a \
specific line in the diff. Your job, for each finding, is to decide \
whether the diff actually shows the problem the finding describes.

Output ONLY a JSON object matching the provided schema, listing one \
verdict per finding by index, with `keep: true` if the diff \
corroborates the finding and `keep: false` otherwise.

Be strict: when in doubt, drop the finding. False positives are \
worse than missed issues at this stage \u{2014} the reviewer will catch \
real bugs even if a few were dropped, but every false flag erodes \
trust.";

/// Parsed verification schema, initialized on first use by
/// [`verification_schema`].
static VERIFICATION_SCHEMA: OnceLock<serde_json::Value> = OnceLock::new();

/// The verifier model's reply: one verdict per reviewed finding.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[expect(
    clippy::exhaustive_structs,
    reason = "deny_unknown_fields wire DTO mirroring schemas/verification.json; every consumer is in \
              this publish = false workspace, so a new field is a coordinated schema edit"
)]
#[expect(
    clippy::module_name_repetitions,
    reason = "public name re-exported from the crate root as ar_prompts::VerificationOutput; \
              renaming it changes the crate API"
)]
pub struct VerificationOutput {
    pub verdicts: Vec<VerificationVerdict>,
}

/// Why [`validate_verification_output`] rejected a verifier reply.
#[derive(Debug, thiserror::Error)]
#[expect(
    clippy::module_name_repetitions,
    reason = "public name re-exported from the crate root as \
              ar_prompts::VerificationValidationError; renaming it changes the crate API"
)]
pub enum VerificationValidationError {
    #[error("output is not valid JSON: {0}")]
    NotJson(String),
    #[error("output does not match schema: {0}")]
    SchemaMismatch(String),
}

/// The verifier's keep-or-drop decision for one finding, by index.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[expect(
    clippy::exhaustive_structs,
    reason = "deny_unknown_fields wire DTO mirroring schemas/verification.json; every consumer is in \
              this publish = false workspace, so a new field is a coordinated schema edit"
)]
#[expect(
    clippy::module_name_repetitions,
    reason = "public name re-exported from the crate root as ar_prompts::VerificationVerdict; \
              renaming it changes the crate API"
)]
pub struct VerificationVerdict {
    pub finding_index: usize,
    pub keep: bool,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub reasoning: String,
}

/// Parse and validate a candidate verifier reply against
/// [`VerificationOutput`].
///
/// # Errors
///
/// Returns [`VerificationValidationError::NotJson`] when `json` does not
/// parse as JSON, and [`VerificationValidationError::SchemaMismatch`] when it
/// parses but does not deserialize into [`VerificationOutput`].
#[inline]
pub fn validate_verification_output(
    json: &str,
) -> Result<VerificationOutput, VerificationValidationError> {
    let value: serde_json::Value = serde_json::from_str(json)
        .map_err(|err| VerificationValidationError::NotJson(err.to_string()))?;
    serde_json::from_value::<VerificationOutput>(value)
        .map_err(|err| VerificationValidationError::SchemaMismatch(err.to_string()))
}

/// Returns the JSON Schema document the verifier model is constrained
/// against. Parsed once and reused.
///
/// # Panics
///
/// Panics on first call if the embedded `schemas/verification.json` is not
/// valid JSON. The unit tests parse it, so a broken schema fails the build
/// gate rather than a running review.
#[inline]
#[expect(
    clippy::module_name_repetitions,
    reason = "public name imported by ar-review (verify) as ar_prompts::verification_schema; \
              renaming it changes the crate API"
)]
#[expect(
    clippy::expect_used,
    reason = "the schema is include_str! of a checked-in file that every test run parses; a parse \
              failure is a build defect, and there is no verification to run without the schema"
)]
pub fn verification_schema() -> &'static serde_json::Value {
    VERIFICATION_SCHEMA.get_or_init(|| {
        serde_json::from_str(VERIFICATION_SCHEMA_JSON)
            .expect("verification schema is valid JSON at compile-time")
    })
}

/// The static system prompt for the verifier model.
#[inline]
#[must_use = "returns the verifier system prompt text without side effects"]
#[expect(
    clippy::module_name_repetitions,
    reason = "public name imported by ar-review (verify) as \
              ar_prompts::verification_system_prompt; renaming it changes the crate API"
)]
pub fn verification_system_prompt() -> &'static str {
    VERIFICATION_SYSTEM_PROMPT
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[expect(
        clippy::min_ident_chars,
        reason = "`s` is the schema under test in the assertion below; renaming it would edit \
                  that assertion line, which this lint-only change leaves untouched"
    )]
    #[expect(
        clippy::expect_used,
        reason = "a schema without a `required` array must fail this test outright"
    )]
    fn schema_titled_verification_with_verdicts_required() {
        let s = verification_schema();
        assert_eq!(s["title"], "Verification");
        let req: Vec<&str> = s["required"]
            .as_array()
            .expect("required is array")
            .iter()
            .filter_map(|value| value.as_str())
            .collect();
        assert_eq!(req, vec!["verdicts"]);
    }

    #[test]
    #[expect(
        clippy::expect_used,
        reason = "well-formed input must validate; a rejection aborts this test"
    )]
    #[expect(
        clippy::indexing_slicing,
        reason = "the preceding assertion pins two verdicts, so indexes 0 and 1 are in bounds"
    )]
    fn validate_accepts_well_formed_output() {
        let json = r#"{
            "verdicts": [
                {"finding_index": 0, "keep": true, "reasoning": "the line shows .unwrap()"},
                {"finding_index": 1, "keep": false}
            ]
        }"#;
        let out = validate_verification_output(json).expect("ok");
        assert_eq!(out.verdicts.len(), 2);
        assert!(out.verdicts[0].keep);
        assert_eq!(out.verdicts[0].reasoning, "the line shows .unwrap()");
        assert!(!out.verdicts[1].keep);
        assert_eq!(out.verdicts[1].reasoning, "");
    }

    #[test]
    #[expect(
        clippy::expect_used,
        reason = "expect_err is the assertion that non-JSON input is rejected"
    )]
    fn validate_rejects_non_json() {
        let err = validate_verification_output("nope").expect_err("err");
        assert!(matches!(err, VerificationValidationError::NotJson(_)));
    }

    #[test]
    #[expect(
        clippy::expect_used,
        reason = "expect_err is the assertion that a verdict without keep is rejected"
    )]
    fn validate_rejects_missing_keep_field() {
        let json = r#"{"verdicts":[{"finding_index": 0}]}"#;
        let err = validate_verification_output(json).expect_err("err");
        assert!(matches!(
            err,
            VerificationValidationError::SchemaMismatch(_)
        ));
    }

    #[test]
    #[expect(
        clippy::expect_used,
        reason = "expect_err is the assertion that an unknown top-level field is rejected"
    )]
    fn validate_rejects_unknown_top_level_field() {
        let json = r#"{"verdicts":[],"extra":1}"#;
        let err = validate_verification_output(json).expect_err("err");
        assert!(matches!(
            err,
            VerificationValidationError::SchemaMismatch(_)
        ));
    }

    #[test]
    #[expect(
        clippy::min_ident_chars,
        reason = "`p` is the prompt under test in the assertions below; renaming it would edit \
                  those assertion lines, which this lint-only change leaves untouched"
    )]
    fn system_prompt_emphasizes_strictness() {
        let p = verification_system_prompt();
        assert!(p.to_lowercase().contains("strict"));
        assert!(p.to_lowercase().contains("false positive"));
    }
}

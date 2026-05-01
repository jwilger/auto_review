//! Verification prompt + JSON schema. A second-pass cheap-tier
//! check that drops findings the diff doesn't actually corroborate.
//! Runs after the reasoning-tier review and before the post step.

use serde::{Deserialize, Serialize};
use std::sync::OnceLock;

const VERIFICATION_SCHEMA_JSON: &str = include_str!("../schemas/verification.json");

static VERIFICATION_SCHEMA: OnceLock<serde_json::Value> = OnceLock::new();

pub fn verification_schema() -> &'static serde_json::Value {
    VERIFICATION_SCHEMA.get_or_init(|| {
        serde_json::from_str(VERIFICATION_SCHEMA_JSON)
            .expect("verification schema is valid JSON at compile-time")
    })
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct VerificationVerdict {
    pub finding_index: usize,
    pub keep: bool,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub reasoning: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct VerificationOutput {
    pub verdicts: Vec<VerificationVerdict>,
}

const VERIFICATION_SYSTEM_PROMPT: &str = "\
You are a verifier for code-review findings. Each finding cites a \
specific line in the diff. Your job, for each finding, is to decide \
whether the diff actually shows the problem the finding describes.

Output ONLY a JSON object matching the provided schema, listing one \
verdict per finding by index, with `keep: true` if the diff \
corroborates the finding and `keep: false` otherwise.

Be strict: when in doubt, drop the finding. False positives are \
worse than missed issues at this stage — the reviewer will catch \
real bugs even if a few were dropped, but every false flag erodes \
trust.";

pub fn verification_system_prompt() -> &'static str {
    VERIFICATION_SYSTEM_PROMPT
}

#[derive(Debug, thiserror::Error)]
pub enum VerificationValidationError {
    #[error("output is not valid JSON: {0}")]
    NotJson(String),
    #[error("output does not match schema: {0}")]
    SchemaMismatch(String),
}

pub fn validate_verification_output(
    json: &str,
) -> Result<VerificationOutput, VerificationValidationError> {
    let value: serde_json::Value = serde_json::from_str(json)
        .map_err(|e| VerificationValidationError::NotJson(e.to_string()))?;
    serde_json::from_value::<VerificationOutput>(value)
        .map_err(|e| VerificationValidationError::SchemaMismatch(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schema_titled_verification_with_verdicts_required() {
        let s = verification_schema();
        assert_eq!(s["title"], "Verification");
        let req: Vec<&str> = s["required"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|v| v.as_str())
            .collect();
        assert_eq!(req, vec!["verdicts"]);
    }

    #[test]
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
    fn validate_rejects_non_json() {
        let err = validate_verification_output("nope").expect_err("err");
        assert!(matches!(err, VerificationValidationError::NotJson(_)));
    }

    #[test]
    fn validate_rejects_missing_keep_field() {
        let json = r#"{"verdicts":[{"finding_index": 0}]}"#;
        let err = validate_verification_output(json).expect_err("err");
        assert!(matches!(
            err,
            VerificationValidationError::SchemaMismatch(_)
        ));
    }

    #[test]
    fn validate_rejects_unknown_top_level_field() {
        let json = r#"{"verdicts":[],"extra":1}"#;
        let err = validate_verification_output(json).expect_err("err");
        assert!(matches!(
            err,
            VerificationValidationError::SchemaMismatch(_)
        ));
    }

    #[test]
    fn system_prompt_emphasizes_strictness() {
        let p = verification_system_prompt();
        assert!(p.to_lowercase().contains("strict"));
        assert!(p.to_lowercase().contains("false positive"));
    }
}

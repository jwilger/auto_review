//! Custom natural-language pre-merge checks.
//!
//! Repo authors list free-form English checks under
//! `pre_merge_checks:` in `.auto_review.yaml`; the cheap LLM tier
//! evaluates each against the PR diff and returns one
//! `pass | fail | skip` per check with a one-sentence rationale.
//!
//! Output schema lives at `schemas/pre_merge_custom.json` and the
//! [`validate_pre_merge_custom_output`] entry point enforces it via
//! `serde(deny_unknown_fields)`.

use serde::{Deserialize, Serialize};
use std::sync::OnceLock;

const PRE_MERGE_CUSTOM_SCHEMA_JSON: &str = include_str!("../schemas/pre_merge_custom.json");

static PRE_MERGE_CUSTOM_SCHEMA: OnceLock<serde_json::Value> = OnceLock::new();

pub fn pre_merge_custom_schema() -> &'static serde_json::Value {
    PRE_MERGE_CUSTOM_SCHEMA.get_or_init(|| {
        serde_json::from_str(PRE_MERGE_CUSTOM_SCHEMA_JSON)
            .expect("pre_merge_custom schema is valid JSON at compile-time")
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PreMergeCustomStatus {
    Pass,
    Fail,
    Skip,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PreMergeCustomResult {
    pub status: PreMergeCustomStatus,
    pub rationale: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PreMergeCustomOutput {
    pub checks: Vec<PreMergeCustomResult>,
}

const SYSTEM_PROMPT: &str = "\
You are evaluating repo-author-supplied natural-language pre-merge \
checks against a pull-request diff. For each check, decide whether \
the diff (a) clearly satisfies it, (b) clearly violates it, or (c) \
doesn't touch anything the check could apply to.
Status meanings, in order:
- pass: the diff demonstrates the check is satisfied (e.g. \
\"all new public APIs have rustdoc\" passes when every added pub \
fn/struct has /// above it).
- fail: the diff demonstrates the check is violated (e.g. \"no raw \
SQL\" fails when an added line contains `sqlx::query!(\"SELECT...\"))`.
- skip: the diff doesn't touch anything the check applies to (e.g. \
the check is about API documentation but the PR only edits build \
configuration).
Output ONLY a JSON object matching the provided schema, with one \
result per check in the same order they were given. Keep \
rationales to one concrete sentence — avoid vague phrasing like \
\"looks good\" or \"seems fine\". When a check is genuinely \
ambiguous, prefer skip over fail; false-positive failures erode \
trust faster than missed ones.";

pub fn pre_merge_custom_system_prompt() -> &'static str {
    SYSTEM_PROMPT
}

#[derive(Debug, thiserror::Error)]
pub enum PreMergeCustomValidationError {
    #[error("output is not valid JSON: {0}")]
    NotJson(String),
    #[error("output does not match schema: {0}")]
    SchemaMismatch(String),
    #[error("expected {expected} check result(s), got {got}")]
    LengthMismatch { expected: usize, got: usize },
}

/// Validate the LLM output against the schema and confirm exactly
/// `expected_count` results came back. Length mismatches are
/// surfaced as a distinct error so the caller can degrade
/// gracefully (skip-all-checks rather than misalign).
pub fn validate_pre_merge_custom_output(
    json: &str,
    expected_count: usize,
) -> Result<PreMergeCustomOutput, PreMergeCustomValidationError> {
    let value: serde_json::Value = serde_json::from_str(json)
        .map_err(|e| PreMergeCustomValidationError::NotJson(e.to_string()))?;
    let out: PreMergeCustomOutput = serde_json::from_value(value)
        .map_err(|e| PreMergeCustomValidationError::SchemaMismatch(e.to_string()))?;
    if out.checks.len() != expected_count {
        return Err(PreMergeCustomValidationError::LengthMismatch {
            expected: expected_count,
            got: out.checks.len(),
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schema_titled_pre_merge_custom_with_checks_required() {
        let s = pre_merge_custom_schema();
        assert_eq!(s["title"], "PreMergeCustom");
        let req: Vec<&str> = s["required"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|v| v.as_str())
            .collect();
        assert_eq!(req, vec!["checks"]);
    }

    #[test]
    fn validate_accepts_well_formed_output() {
        let json = r#"{
            "checks": [
                {"status": "pass", "rationale": "every new pub fn has /// above it"},
                {"status": "fail", "rationale": "added a raw sqlx::query! at src/db.rs:42"},
                {"status": "skip", "rationale": "diff is config-only"}
            ]
        }"#;
        let out = validate_pre_merge_custom_output(json, 3).expect("ok");
        assert_eq!(out.checks.len(), 3);
        assert_eq!(out.checks[0].status, PreMergeCustomStatus::Pass);
        assert_eq!(out.checks[1].status, PreMergeCustomStatus::Fail);
        assert_eq!(out.checks[2].status, PreMergeCustomStatus::Skip);
    }

    #[test]
    fn validate_rejects_unknown_status_value() {
        let json = r#"{"checks":[{"status":"maybe","rationale":"x"}]}"#;
        let err = validate_pre_merge_custom_output(json, 1).expect_err("err");
        assert!(matches!(
            err,
            PreMergeCustomValidationError::SchemaMismatch(_)
        ));
    }

    #[test]
    fn validate_rejects_unknown_field() {
        let json = r#"{"checks":[{"status":"pass","rationale":"x","extra":1}]}"#;
        let err = validate_pre_merge_custom_output(json, 1).expect_err("err");
        assert!(matches!(
            err,
            PreMergeCustomValidationError::SchemaMismatch(_)
        ));
    }

    #[test]
    fn validate_rejects_length_mismatch() {
        let json = r#"{"checks":[{"status":"pass","rationale":"x"}]}"#;
        let err = validate_pre_merge_custom_output(json, 2).expect_err("err");
        assert!(matches!(
            err,
            PreMergeCustomValidationError::LengthMismatch { expected: 2, got: 1 }
        ));
    }

    #[test]
    fn validate_rejects_non_json() {
        let err = validate_pre_merge_custom_output("not json", 0).expect_err("err");
        assert!(matches!(err, PreMergeCustomValidationError::NotJson(_)));
    }

    #[test]
    fn system_prompt_describes_three_statuses() {
        let p = pre_merge_custom_system_prompt();
        assert!(p.contains("pass"));
        assert!(p.contains("fail"));
        assert!(p.contains("skip"));
    }
}

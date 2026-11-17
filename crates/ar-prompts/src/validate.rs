use crate::types::ReviewOutput;

#[derive(Debug, thiserror::Error)]
pub enum ValidationError {
    #[error("output is not valid JSON: {0}")]
    NotJson(String),
    #[error("output does not match schema: {0}")]
    SchemaMismatch(String),
}

/// Parse and validate a candidate review JSON string against the
/// [`ReviewOutput`] schema.
///
/// Used by the self-heal loop in `ar-review`: if validation fails, the
/// returned error is fed back into the LLM with instructions to repair.
///
/// `deny_unknown_fields` on `ReviewOutput`/`ReviewFinding` and the strict
/// enum on `ReviewSeverity` mean serde performs schema-level validation
/// natively — anything that can't deserialize is reported as a
/// `SchemaMismatch`.
///
/// # Errors
///
/// Returns [`ValidationError::NotJson`] when `json` does not parse as JSON,
/// and [`ValidationError::SchemaMismatch`] when it parses but does not
/// deserialize into [`ReviewOutput`].
#[inline]
#[expect(
    clippy::module_name_repetitions,
    reason = "public name imported by ar-review (heal) as ar_prompts::validate_review_output; \
              renaming it changes the crate API"
)]
pub fn validate_review_output(json: &str) -> Result<ReviewOutput, ValidationError> {
    let value: serde_json::Value =
        serde_json::from_str(json).map_err(|err| ValidationError::NotJson(err.to_string()))?;
    serde_json::from_value::<ReviewOutput>(value)
        .map_err(|err| ValidationError::SchemaMismatch(err.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::FindingCategory;

    #[test]
    #[expect(
        clippy::expect_used,
        reason = "expect_err is the assertion that non-JSON input is rejected"
    )]
    fn rejects_non_json() {
        let err = validate_review_output("not json").expect_err("should err");
        assert!(matches!(err, ValidationError::NotJson(_)));
    }

    #[test]
    #[expect(
        clippy::expect_used,
        reason = "well-formed input must validate; a rejection aborts this test"
    )]
    #[expect(
        clippy::assert_is_empty,
        reason = "is_empty() states the expected empty findings directly; this lint-only change \
                  leaves the assertion unedited"
    )]
    fn accepts_minimal_well_formed_output() {
        let json = r#"{"summary":"ok","findings":[]}"#;
        let out = validate_review_output(json).expect("ok");
        assert_eq!(out.summary, "ok");
        assert!(out.findings.is_empty());
    }

    #[test]
    #[expect(
        clippy::expect_used,
        reason = "well-formed input must validate; a rejection aborts this test"
    )]
    #[expect(
        clippy::indexing_slicing,
        reason = "the preceding assertion pins two findings, so index 1 is in bounds"
    )]
    fn accepts_output_with_findings() {
        let json = r#"{
            "summary": "two issues",
            "findings": [
                {"path":"a.rs","line_start":1,"severity":"warning","message":"m"},
                {"path":"b.rs","line_start":3,"line_end":5,"severity":"error","message":"n"}
            ]
        }"#;
        let out = validate_review_output(json).expect("ok");
        assert_eq!(out.findings.len(), 2);
        assert_eq!(out.findings[1].line_end, Some(5));
    }

    #[test]
    #[expect(
        clippy::expect_used,
        reason = "validation, serialization and re-validation must all succeed; a failure aborts this test"
    )]
    #[expect(
        clippy::indexing_slicing,
        reason = "the fixture JSON holds exactly one finding, so index 0 is in bounds"
    )]
    fn finding_with_title_and_category_round_trips() {
        let json = r#"{"summary":"ok","findings":[
            {"path":"a.rs","line_start":1,"severity":"note","message":"m","title":"Nit","category":"style"}
        ]}"#;
        let out = validate_review_output(json).expect("ok");
        assert_eq!(out.findings[0].category, FindingCategory::Style);
        assert_eq!(out.findings[0].title, "Nit");
        let reserialized = serde_json::to_string(&out).expect("serialize");
        let again = validate_review_output(&reserialized).expect("round-trip");
        assert_eq!(again, out);
    }

    #[test]
    #[expect(
        clippy::expect_used,
        reason = "well-formed input must validate; a rejection aborts this test"
    )]
    #[expect(
        clippy::indexing_slicing,
        reason = "the fixture JSON holds exactly one finding, so index 0 is in bounds"
    )]
    fn finding_without_title_or_category_defaults_to_empty_title_and_correctness() {
        let json = r#"{"summary":"x","findings":[
            {"path":"a.rs","line_start":1,"severity":"warning","message":"m"}
        ]}"#;
        let out = validate_review_output(json).expect("ok");
        assert_eq!(out.findings[0].title, "");
        assert_eq!(out.findings[0].category, FindingCategory::Correctness);
    }

    #[test]
    #[expect(
        clippy::expect_used,
        reason = "expect_err is the assertion that each bad category is rejected"
    )]
    fn rejects_out_of_enum_wrong_case_and_non_string_categories() {
        for category in [r#""nitpick""#, r#""Style""#, "7"] {
            let json = format!(
                r#"{{"summary":"x","findings":[
                    {{"path":"a","line_start":1,"severity":"note","message":"m","category":{category}}}
                ]}}"#
            );
            let err = validate_review_output(&json).expect_err("should err");
            assert!(
                matches!(err, ValidationError::SchemaMismatch(_)),
                "category {category} gave {err:?}"
            );
        }
    }

    #[test]
    #[expect(
        clippy::expect_used,
        reason = "expect_err is the assertion that a missing summary is rejected"
    )]
    fn rejects_missing_required_field() {
        let json = r#"{"findings":[]}"#;
        let err = validate_review_output(json).expect_err("should err");
        assert!(matches!(err, ValidationError::SchemaMismatch(_)));
    }

    #[test]
    #[expect(
        clippy::expect_used,
        reason = "expect_err is the assertion that an unknown severity is rejected"
    )]
    fn rejects_unknown_severity() {
        let json = r#"{"summary":"x","findings":[
            {"path":"a","line_start":1,"severity":"catastrophic","message":"m"}
        ]}"#;
        let err = validate_review_output(json).expect_err("should err");
        assert!(matches!(err, ValidationError::SchemaMismatch(_)));
    }

    #[test]
    #[expect(
        clippy::expect_used,
        reason = "expect_err is the assertion that an unknown top-level field is rejected"
    )]
    fn rejects_unknown_top_level_field() {
        let json = r#"{"summary":"x","findings":[],"extra":1}"#;
        let err = validate_review_output(json).expect_err("should err");
        assert!(matches!(err, ValidationError::SchemaMismatch(_)));
    }
}

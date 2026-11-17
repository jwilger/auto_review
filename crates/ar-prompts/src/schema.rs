use std::sync::OnceLock;

/// Source text of the review JSON Schema, embedded at compile time.
const REVIEW_SCHEMA_JSON: &str = include_str!("../schemas/review.json");

/// Parsed review schema, initialized on first use by [`review_schema`].
static REVIEW_SCHEMA: OnceLock<serde_json::Value> = OnceLock::new();

/// Returns the JSON Schema document the LLM is constrained against when
/// generating reviews. Parsed once and reused.
///
/// # Panics
///
/// Panics on first call if the embedded `schemas/review.json` is not valid
/// JSON. The unit tests parse it, so a broken schema fails the build gate
/// rather than a running review.
#[inline]
#[expect(
    clippy::module_name_repetitions,
    reason = "public name imported by ar-review (heal) as ar_prompts::review_schema; renaming it \
              changes the crate API"
)]
#[expect(
    clippy::expect_used,
    reason = "the schema is include_str! of a checked-in file that every test run parses; a parse \
              failure is a build defect, and there is no review to run without the schema"
)]
pub fn review_schema() -> &'static serde_json::Value {
    REVIEW_SCHEMA.get_or_init(|| {
        serde_json::from_str(REVIEW_SCHEMA_JSON)
            .expect("review schema is valid JSON at compile-time")
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::FindingCategory;
    use crate::verification::verification_schema;

    #[test]
    #[expect(
        clippy::min_ident_chars,
        reason = "`s` is the schema under test in the assertion below; renaming it would edit \
                  that assertion line, which this lint-only change leaves untouched"
    )]
    fn returns_a_review_titled_schema() {
        let s = review_schema();
        assert_eq!(s["title"], "Review");
    }

    #[test]
    #[expect(
        clippy::expect_used,
        reason = "a schema without a `required` array must fail this test outright"
    )]
    fn schema_requires_summary_and_findings() {
        let schema = review_schema();
        let req = schema["required"].as_array().expect("required is array");
        let names: Vec<&str> = req.iter().filter_map(|value| value.as_str()).collect();
        assert!(names.contains(&"summary"));
        assert!(names.contains(&"findings"));
    }

    #[test]
    fn summary_schema_does_not_tell_model_to_state_what_pr_does() {
        let description = s_summary_description();

        assert!(
            !description.contains("state what the pr does"),
            "summary schema should not ask the model to restate PR contents; description was: {description}"
        );
    }

    #[expect(
        clippy::indexing_slicing,
        reason = "serde_json::Value indexing by &str yields Null for a missing key and never panics"
    )]
    #[expect(
        clippy::expect_used,
        reason = "a schema whose summary has no string description must fail the calling test"
    )]
    fn s_summary_description() -> String {
        review_schema()["properties"]["summary"]["description"]
            .as_str()
            .expect("summary.description is string")
            .to_lowercase()
    }

    #[test]
    #[expect(
        clippy::indexing_slicing,
        reason = "serde_json::Value indexing by &str yields Null for a missing key and never panics"
    )]
    #[expect(
        clippy::expect_used,
        reason = "a severity property without an enum array must fail this test outright"
    )]
    fn schema_constrains_finding_severity_to_three_levels() {
        let schema = review_schema();
        let enum_ = &schema["properties"]["findings"]["items"]["properties"]["severity"]["enum"];
        let levels: Vec<&str> = enum_
            .as_array()
            .expect("severity.enum is array")
            .iter()
            .filter_map(|value| value.as_str())
            .collect();
        assert_eq!(levels, vec!["note", "warning", "error"]);
    }

    #[test]
    #[expect(
        clippy::indexing_slicing,
        reason = "serde_json::Value indexing by &str yields Null for a missing key and never panics"
    )]
    #[expect(
        clippy::expect_used,
        reason = "a category property without an enum array must fail this test outright"
    )]
    fn schema_constrains_finding_category_to_closed_taxonomy() {
        let schema = review_schema();
        let enum_ = &schema["properties"]["findings"]["items"]["properties"]["category"]["enum"];
        let categories: Vec<&str> = enum_
            .as_array()
            .expect("category.enum is array")
            .iter()
            .filter_map(|value| value.as_str())
            .collect();
        assert_eq!(
            categories,
            vec![
                "correctness",
                "security",
                "reliability",
                "performance",
                "compatibility",
                "ci_coverage",
                "tests",
                "documentation",
                "refactor",
                "naming",
                "style",
            ]
        );
        let schema_set: Vec<&str> = FindingCategory::ALL
            .iter()
            .map(|category| category.as_str())
            .collect();
        assert_eq!(categories, schema_set);
    }

    #[test]
    #[expect(
        clippy::indexing_slicing,
        reason = "serde_json::Value indexing by &str yields Null for a missing key and never panics"
    )]
    #[expect(
        clippy::expect_used,
        reason = "a findings item without a `required` array must fail this test outright"
    )]
    fn schema_requires_finding_title_and_category() {
        let item = &review_schema()["properties"]["findings"]["items"];
        let required: Vec<&str> = item["required"]
            .as_array()
            .expect("required is array")
            .iter()
            .filter_map(|value| value.as_str())
            .collect();
        assert!(required.contains(&"title"));
        assert!(required.contains(&"category"));
        assert_eq!(item["properties"]["title"]["type"], "string");
        assert_eq!(item["additionalProperties"], false);
    }

    /// `OpenAI`'s `response_format: json_schema` strict mode requires every
    /// property listed in `properties` to appear in `required`. Optional
    /// fields are expressed via nullable types instead. A drift here turns
    /// every LLM call into a 400 from the API — caught the hard way during
    /// dogfooding (Review and Verification schemas both regressed).
    #[test]
    fn every_response_schema_satisfies_openai_strict_mode() {
        for (name, schema) in [
            ("review", review_schema()),
            ("verification", verification_schema()),
        ] {
            assert_strict_mode_compatible(schema, name);
        }
    }

    fn assert_strict_mode_compatible(schema: &serde_json::Value, label: &str) {
        #[expect(
            clippy::panic,
            reason = "a schema object without properties or required must fail the strict-mode \
                      test, and the panic message names the offending schema path"
        )]
        fn walk(node: &serde_json::Value, path: &str) {
            if node["type"] == "object" {
                let props = node["properties"]
                    .as_object()
                    .unwrap_or_else(|| panic!("{path}: object missing properties"));
                let required: Vec<&str> = node["required"]
                    .as_array()
                    .unwrap_or_else(|| panic!("{path}: object missing required array"))
                    .iter()
                    .filter_map(|value| value.as_str())
                    .collect();
                for name in props.keys() {
                    assert!(
                        required.contains(&name.as_str()),
                        "{path}: property `{name}` is not in required (strict mode forbids this)"
                    );
                }
                for (name, child) in props {
                    walk(child, &format!("{path}.{name}"));
                }
            }
            if node["type"] == "array" {
                walk(&node["items"], &format!("{path}[]"));
            }
        }
        walk(schema, &format!("${label}"));
    }
}

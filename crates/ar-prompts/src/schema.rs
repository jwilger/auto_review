use std::sync::OnceLock;

const REVIEW_SCHEMA_JSON: &str = include_str!("../schemas/review.json");

static REVIEW_SCHEMA: OnceLock<serde_json::Value> = OnceLock::new();

/// Returns the JSON Schema document the LLM is constrained against when
/// generating reviews. Parsed once and reused.
pub fn review_schema() -> &'static serde_json::Value {
    REVIEW_SCHEMA.get_or_init(|| {
        serde_json::from_str(REVIEW_SCHEMA_JSON)
            .expect("review schema is valid JSON at compile-time")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn returns_a_review_titled_schema() {
        let s = review_schema();
        assert_eq!(s["title"], "Review");
    }

    #[test]
    fn schema_requires_summary_and_findings() {
        let s = review_schema();
        let req = s["required"].as_array().expect("required is array");
        let names: Vec<&str> = req.iter().filter_map(|v| v.as_str()).collect();
        assert!(names.contains(&"summary"));
        assert!(names.contains(&"findings"));
    }

    #[test]
    fn schema_constrains_finding_severity_to_three_levels() {
        let s = review_schema();
        let enum_ = &s["properties"]["findings"]["items"]["properties"]["severity"]["enum"];
        let levels: Vec<&str> = enum_
            .as_array()
            .expect("severity.enum is array")
            .iter()
            .filter_map(|v| v.as_str())
            .collect();
        assert_eq!(levels, vec!["note", "warning", "error"]);
    }

    /// OpenAI's `response_format: json_schema` strict mode requires every
    /// property listed in `properties` to appear in `required`. Optional
    /// fields are expressed via nullable types instead. A drift here turns
    /// every LLM call into a 400 from the API — caught the hard way during
    /// dogfooding (Review and Verification schemas both regressed).
    #[test]
    fn every_response_schema_satisfies_openai_strict_mode() {
        for (name, schema) in [
            ("review", crate::schema::review_schema()),
            ("triage", crate::triage::triage_schema()),
            ("verification", crate::verification::verification_schema()),
        ] {
            assert_strict_mode_compatible(schema, name);
        }
    }

    fn assert_strict_mode_compatible(schema: &serde_json::Value, label: &str) {
        fn walk(node: &serde_json::Value, path: &str) {
            if node["type"] == "object" {
                let props = node["properties"]
                    .as_object()
                    .unwrap_or_else(|| panic!("{path}: object missing properties"));
                let required: Vec<&str> = node["required"]
                    .as_array()
                    .unwrap_or_else(|| panic!("{path}: object missing required array"))
                    .iter()
                    .filter_map(|v| v.as_str())
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

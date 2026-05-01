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
}

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
#[expect(
    clippy::arbitrary_source_item_ordering,
    reason = "variants run from least to most severe, the order of the severity enum in \
              schemas/review.json and of the severity definitions in the system prompt"
)]
pub enum ReviewSeverity {
    Note,
    Warning,
    Error,
}

/// Closed taxonomy for review findings. The single category set used by
/// metrics, noise controls, feedback suppression, fan-out and the bench.
///
/// There is deliberately no catch-all variant: an out-of-enum category from
/// the model fails validation instead of reaching a metric label.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[expect(
    clippy::arbitrary_source_item_ordering,
    reason = "variants follow the category enum order in schemas/review.json, which ALL repeats \
              and the schema test pins"
)]
pub enum FindingCategory {
    /// Default for findings that omit a category, so nothing is
    /// accidentally suppressed as a low-value category.
    #[default]
    Correctness,
    Security,
    Reliability,
    Performance,
    Compatibility,
    CiCoverage,
    Tests,
    Documentation,
    Refactor,
    Naming,
    Style,
}

impl FindingCategory {
    /// Every category, in schema order.
    pub const ALL: [FindingCategory; 11] = [
        FindingCategory::Correctness,
        FindingCategory::Security,
        FindingCategory::Reliability,
        FindingCategory::Performance,
        FindingCategory::Compatibility,
        FindingCategory::CiCoverage,
        FindingCategory::Tests,
        FindingCategory::Documentation,
        FindingCategory::Refactor,
        FindingCategory::Naming,
        FindingCategory::Style,
    ];

    /// The `snake_case` identifier, identical to the serde form. Safe to use
    /// as a metric label because the set is closed.
    #[inline]
    #[must_use = "returns the label without side effects"]
    pub fn as_str(self) -> &'static str {
        match self {
            FindingCategory::Correctness => "correctness",
            FindingCategory::Security => "security",
            FindingCategory::Reliability => "reliability",
            FindingCategory::Performance => "performance",
            FindingCategory::Compatibility => "compatibility",
            FindingCategory::CiCoverage => "ci_coverage",
            FindingCategory::Tests => "tests",
            FindingCategory::Documentation => "documentation",
            FindingCategory::Refactor => "refactor",
            FindingCategory::Naming => "naming",
            FindingCategory::Style => "style",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[expect(
    clippy::exhaustive_structs,
    reason = "ar-review (mapping, pipeline, verify, agentic_verify) and ar-cli (bench) build \
              findings by struct literal; every consumer is in this publish = false workspace"
)]
#[expect(
    clippy::arbitrary_source_item_ordering,
    reason = "field order is the serialized JSON key order and follows the finding properties in \
              schemas/review.json; reordering would change emitted JSON"
)]
pub struct ReviewFinding {
    pub path: String,
    pub line_start: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line_end: Option<u32>,
    pub severity: ReviewSeverity,
    pub message: String,
    /// Short headline (8 words or fewer). Empty when the model omitted it.
    #[serde(default)]
    pub title: String,
    /// Category from the closed taxonomy. Defaults to `correctness` when
    /// omitted.
    #[serde(default)]
    pub category: FindingCategory,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[expect(
    clippy::exhaustive_structs,
    reason = "ar-review (mapping, pipeline, verify, agentic_verify) and ar-cli (bench) build \
              review outputs by struct literal; every consumer is in this publish = false workspace"
)]
#[expect(
    clippy::arbitrary_source_item_ordering,
    reason = "field order is the serialized JSON key order and follows the top-level properties in \
              schemas/review.json; reordering would change emitted JSON"
)]
pub struct ReviewOutput {
    pub summary: String,
    /// Optional longer-form walkthrough in markdown.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub walkthrough: String,
    /// Optional Mermaid diagram source (without the `` ```mermaid `` fence).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub mermaid: String,
    pub findings: Vec<ReviewFinding>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[expect(
        clippy::expect_used,
        reason = "serializing a unit enum variant cannot fail; a failure must abort this test"
    )]
    fn category_as_str_matches_serde_form() {
        for category in FindingCategory::ALL {
            let serialized = serde_json::to_value(category).expect("serialize");
            assert_eq!(serialized, serde_json::Value::from(category.as_str()));
        }
    }

    #[test]
    fn ci_coverage_serializes_snake_case() {
        assert_eq!(FindingCategory::CiCoverage.as_str(), "ci_coverage");
    }

    #[test]
    fn default_category_is_correctness() {
        assert_eq!(FindingCategory::default(), FindingCategory::Correctness);
    }
}

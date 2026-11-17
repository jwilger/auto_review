//! Contract test pinning the shipped operator artifacts to the metrics the
//! gateway actually exposes.
//!
//! This is a deliberate docs-reading contract test under AGENTS.md: the
//! Prometheus rules (`deploy/prometheus/auto_review.rules.yaml`) and the
//! Grafana dashboard (`deploy/grafana/auto_review.dashboard.json`) are
//! executable operator contracts, not prose. A dashboard panel or alert that
//! references a metric `Metrics::render()` no longer emits (or a recording
//! rule the rules file never defines) silently stops measuring outcomes:
//! Prometheus returns an empty series instead of an error. This test turns
//! that silent drift into a failing build.
//!
//! Both files are embedded with `include_str!` so the test also runs inside
//! the Nix build sandbox, where `flake.nix` ships them into the source tree.

use std::collections::BTreeSet;

use ar_gateway::metrics::Metrics;

const RULES_FILE: &str = "deploy/prometheus/auto_review.rules.yaml";
const DASHBOARD_FILE: &str = "deploy/grafana/auto_review.dashboard.json";

const RULES: &str = include_str!("../../../deploy/prometheus/auto_review.rules.yaml");
const DASHBOARD: &str = include_str!("../../../deploy/grafana/auto_review.dashboard.json");

const METRIC_PREFIX: &str = "auto_review_";
const RECORDING_RULE_PREFIX: &str = "auto_review:";
const STRIPPED_SUFFIXES: [&str; 3] = ["_bucket", "_sum", "_count"];

/// Characters that may continue a Prometheus metric or recording-rule name.
/// Used to make sure a match is a whole token, not part of a longer
/// identifier.
fn is_name_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_' || c == ':'
}

/// Collect every whole token in `text` that starts with `prefix` and continues
/// with at least one character accepted by `body`.
fn scan_tokens(text: &str, prefix: &str, body: impl Fn(char) -> bool) -> BTreeSet<String> {
    let mut found = BTreeSet::new();
    for (start, _) in text.match_indices(prefix) {
        let preceded_by_name_char = text[..start].chars().next_back().is_some_and(is_name_char);
        if preceded_by_name_char {
            continue;
        }
        let rest = &text[start + prefix.len()..];
        let body_len: usize = rest
            .chars()
            .take_while(|c| body(*c))
            .map(char::len_utf8)
            .sum();
        if body_len == 0 {
            continue;
        }
        // A metric token immediately followed by another name character
        // (for example `:` in a recording-rule name) is not a metric.
        let next = rest[body_len..].chars().next();
        if next.is_some_and(is_name_char) {
            continue;
        }
        found.insert(format!("{prefix}{}", &rest[..body_len]));
    }
    found
}

/// Every `auto_review_[a-z0-9_]+` metric token in `text`.
fn metric_names(text: &str) -> BTreeSet<String> {
    scan_tokens(text, METRIC_PREFIX, |c| {
        c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_'
    })
}

/// Every `auto_review:[a-z0-9_:]+` recording-rule token in `text`.
fn recording_rule_names(text: &str) -> BTreeSet<String> {
    scan_tokens(text, RECORDING_RULE_PREFIX, |c| {
        c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == ':'
    })
}

/// Remove one trailing `_bucket`, `_sum` or `_count` suffix.
fn normalize(name: &str) -> &str {
    STRIPPED_SUFFIXES
        .iter()
        .find_map(|suffix| name.strip_suffix(suffix))
        .unwrap_or(name)
}

/// Names exposed by the Prometheus text exposition: every family on a
/// `# TYPE` line plus every series name at the start of a sample line,
/// normalized the same way as references.
fn exposed_names(exposition: &str) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    for line in exposition.lines() {
        let name = if let Some(rest) = line.strip_prefix("# TYPE ") {
            rest.split_whitespace().next()
        } else if line.starts_with('#') {
            None
        } else {
            line.split(|c: char| c == '{' || c.is_whitespace()).next()
        };
        if let Some(name) = name.filter(|n| !n.is_empty()) {
            names.insert(normalize(name).to_owned());
        }
    }
    names
}

/// Names defined by `record: <name>` entries in the rules file. Names that
/// only appear in an `expr:` or alert do not count.
fn defined_recording_rules(rules: &str) -> BTreeSet<String> {
    rules
        .lines()
        .filter_map(|line| {
            let trimmed = line.trim_start();
            let trimmed = trimmed.strip_prefix("- ").unwrap_or(trimmed);
            trimmed.strip_prefix("record:")
        })
        .map(|value| {
            value
                .trim()
                .trim_matches(|c| c == '"' || c == '\'')
                .to_owned()
        })
        .collect()
}

/// Metric references in `text` that `exposed` does not contain, tagged with
/// the file they came from.
fn missing_metrics(file: &str, text: &str, exposed: &BTreeSet<String>) -> Vec<String> {
    metric_names(text)
        .iter()
        .map(|name| normalize(name))
        .filter(|name| !exposed.contains(*name))
        .map(|name| format!("{name} (referenced in {file})"))
        .collect()
}

#[test]
fn deploy_artifacts_reference_only_exposed_metrics() {
    let exposed = exposed_names(&Metrics::new().render());
    let mut missing: Vec<String> = missing_metrics(RULES_FILE, RULES, &exposed);
    missing.extend(missing_metrics(DASHBOARD_FILE, DASHBOARD, &exposed));
    missing.sort();
    missing.dedup();
    assert!(
        missing.is_empty(),
        "deploy artifacts reference metrics not exposed by Metrics::render():\n  {}",
        missing.join("\n  ")
    );
}

#[test]
fn dashboard_recording_rules_are_defined_in_rules_file() {
    let defined = defined_recording_rules(RULES);
    let missing: Vec<String> = recording_rule_names(DASHBOARD)
        .into_iter()
        .filter(|name| !defined.contains(name))
        .map(|name| format!("{name} (used in {DASHBOARD_FILE}, no `record:` in {RULES_FILE})"))
        .collect();
    assert!(
        missing.is_empty(),
        "dashboard uses recording rules not defined in the rules file:\n  {}",
        missing.join("\n  ")
    );
}

mod scanner {
    use super::*;

    fn set(names: &[&str]) -> BTreeSet<String> {
        names.iter().map(|n| (*n).to_owned()).collect()
    }

    #[test]
    fn metric_names_end_at_delimiters() {
        let text = r#"rate(auto_review_a_total[5m]) auto_review_b{le="1"} sum(auto_review_c) "auto_review_d""#;
        assert_eq!(
            metric_names(text),
            set(&[
                "auto_review_a_total",
                "auto_review_b",
                "auto_review_c",
                "auto_review_d"
            ])
        );
    }

    #[test]
    fn metric_names_ignore_longer_identifiers() {
        assert!(metric_names("not_auto_review_x Xauto_review_y").is_empty());
    }

    #[test]
    fn recording_rule_is_not_mistaken_for_metric() {
        let text = "auto_review:reviews_completed:rate5m * 60";
        assert!(metric_names(text).is_empty());
        assert_eq!(
            recording_rule_names(text),
            set(&["auto_review:reviews_completed:rate5m"])
        );
    }

    #[test]
    fn recording_rule_needs_a_body() {
        assert!(recording_rule_names("auto_review: sustained failures").is_empty());
    }

    #[test]
    fn normalize_strips_one_suffix() {
        assert_eq!(normalize("auto_review_x_bucket"), "auto_review_x");
        assert_eq!(normalize("auto_review_x_sum"), "auto_review_x");
        assert_eq!(normalize("auto_review_x_count"), "auto_review_x");
        assert_eq!(
            normalize("auto_review_x_count_count"),
            "auto_review_x_count"
        );
        assert_eq!(normalize("auto_review_x_total"), "auto_review_x_total");
    }

    #[test]
    fn exposed_names_read_type_and_sample_lines() {
        let exposition = "# HELP auto_review_a_total help text\n\
                          # TYPE auto_review_a_total counter\n\
                          auto_review_a_total 0\n\
                          auto_review_h_bucket{le=\"1\"} 0\n\
                          auto_review_h_sum 0\n";
        assert_eq!(
            exposed_names(exposition),
            set(&["auto_review_a_total", "auto_review_h"])
        );
    }

    #[test]
    fn only_record_entries_define_recording_rules() {
        let rules = "- record: auto_review:a:5m\n  expr: auto_review:b:5m < 1\n";
        assert_eq!(defined_recording_rules(rules), set(&["auto_review:a:5m"]));
    }
}

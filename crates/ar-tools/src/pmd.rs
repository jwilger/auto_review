//! pmd runner. Parses `pmd check --format json` output for Java
//! static analysis.
//!
//! pmd covers ~300 rules across Best Practices, Code Style, Design,
//! Documentation, Error Prone, Multithreading, Performance, Security
//! categories. Routes on `.java` files; runs nowhere else.
//!
//! Output structure: `{files: [{filename, violations: [{beginline,
//! endline, begincolumn, description, rule, ruleset, priority}]}]}`.
//! priority is 1 (highest) to 5 (lowest); we map 1 → Error, 2/3 →
//! Warning, 4/5 → Note.

use crate::finding::{Finding, Severity};
use crate::runner::{run_in_sandbox, LinterRunner, RunnerError};
use ar_sandbox::Sandbox;
use async_trait::async_trait;
use serde::Deserialize;
use std::path::Path;

const TOOL: &str = "pmd";

#[derive(Debug, Deserialize)]
struct PmdOutput {
    #[serde(default)]
    files: Vec<PmdFile>,
}

#[derive(Debug, Deserialize)]
struct PmdFile {
    filename: String,
    #[serde(default)]
    violations: Vec<PmdViolation>,
}

#[derive(Debug, Deserialize)]
struct PmdViolation {
    #[serde(default)]
    beginline: u32,
    #[serde(default)]
    endline: u32,
    #[serde(default)]
    description: String,
    #[serde(default)]
    rule: Option<String>,
    #[serde(default)]
    priority: u8,
}

pub fn parse_pmd_output(json: &str) -> Result<Vec<Finding>, RunnerError> {
    let raw: PmdOutput = serde_json::from_str(json).map_err(|e| RunnerError::Parse {
        tool: TOOL.into(),
        detail: e.to_string(),
    })?;
    let mut out = Vec::new();
    for file in raw.files {
        for v in file.violations {
            let start = v.beginline.max(1);
            let end = if v.endline >= start { v.endline } else { start };
            out.push(Finding {
                source_tool: TOOL.into(),
                rule_id: v.rule,
                path: file.filename.clone(),
                line_start: start,
                line_end: end,
                severity: severity_from(v.priority),
                message: v.description,
            });
        }
    }
    Ok(out)
}

fn severity_from(priority: u8) -> Severity {
    match priority {
        1 => Severity::Error,
        2 | 3 => Severity::Warning,
        _ => Severity::Note,
    }
}

pub struct PmdRunner {
    /// Files to scan, repo-relative. Empty means "scan no files".
    pub files: Vec<String>,
}

#[async_trait]
impl LinterRunner for PmdRunner {
    fn name(&self) -> &str {
        TOOL
    }

    async fn run(
        &self,
        sandbox: &dyn Sandbox,
        repo_dir: &Path,
    ) -> Result<Vec<Finding>, RunnerError> {
        if self.files.is_empty() {
            return Ok(vec![]);
        }
        // pmd 7.x: `pmd check -d <files>` runs the analysis;
        // `--format json` emits the structured output we parse.
        // `--no-cache` avoids state bleed between runs.
        // `--no-fail-on-violation` keeps exit 0 so we can read stdout.
        // `-R rulesets/java/quickstart.xml` is a sensible default
        // when the repo doesn't ship its own ruleset.
        let mut args = vec![
            "check".into(),
            "--format".into(),
            "json".into(),
            "--no-cache".into(),
            "--no-fail-on-violation".into(),
            "-R".into(),
            "rulesets/java/quickstart.xml".into(),
            "-d".into(),
        ];
        // pmd accepts a comma-separated list after -d, or repeats.
        // Using a comma-separated list keeps the argv shorter.
        args.push(self.files.join(","));
        let output = run_in_sandbox(sandbox, repo_dir, "pmd", args, vec![]).await?;
        if output.stdout.is_empty() {
            return Ok(vec![]);
        }
        let stdout = String::from_utf8_lossy(&output.stdout);
        parse_pmd_output(&stdout)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_typical_pmd_output() {
        let json = r#"{
            "formatVersion": 1,
            "pmdVersion": "7.0.0",
            "files": [
                {
                    "filename": "src/main/java/Foo.java",
                    "violations": [
                        {
                            "beginline": 5,
                            "endline": 5,
                            "begincolumn": 1,
                            "endcolumn": 30,
                            "description": "Avoid unused imports such as 'java.util.List'.",
                            "rule": "UnusedImports",
                            "ruleset": "Best Practices",
                            "priority": 3
                        },
                        {
                            "beginline": 12,
                            "endline": 18,
                            "begincolumn": 1,
                            "endcolumn": 1,
                            "description": "Method too long.",
                            "rule": "ExcessiveMethodLength",
                            "ruleset": "Design",
                            "priority": 1
                        }
                    ]
                }
            ]
        }"#;
        let f = parse_pmd_output(json).expect("ok");
        assert_eq!(f.len(), 2);
        assert_eq!(f[0].path, "src/main/java/Foo.java");
        assert_eq!(f[0].line_start, 5);
        assert_eq!(f[0].rule_id.as_deref(), Some("UnusedImports"));
        assert_eq!(f[0].severity, Severity::Warning); // priority 3
        assert_eq!(f[1].line_start, 12);
        assert_eq!(f[1].line_end, 18);
        assert_eq!(f[1].severity, Severity::Error); // priority 1
    }

    #[test]
    fn priority_4_and_5_map_to_note() {
        let json = r#"{
            "files": [{
                "filename":"x.java",
                "violations":[
                    {"beginline":1,"endline":1,"description":"a","rule":"R","priority":4},
                    {"beginline":2,"endline":2,"description":"b","rule":"R","priority":5}
                ]
            }]
        }"#;
        let f = parse_pmd_output(json).expect("ok");
        assert_eq!(f[0].severity, Severity::Note);
        assert_eq!(f[1].severity, Severity::Note);
    }

    #[test]
    fn unknown_priority_falls_back_to_note() {
        let json = r#"{
            "files": [{
                "filename":"x.java",
                "violations":[{"beginline":1,"endline":1,"description":"a","rule":"R","priority":0}]
            }]
        }"#;
        let f = parse_pmd_output(json).expect("ok");
        assert_eq!(f[0].severity, Severity::Note);
    }

    #[test]
    fn empty_files_yields_zero_findings() {
        let f = parse_pmd_output(r#"{"files":[]}"#).expect("ok");
        assert!(f.is_empty());
    }

    #[test]
    fn missing_files_field_decodes_to_empty() {
        let f = parse_pmd_output("{}").expect("ok");
        assert!(f.is_empty());
    }

    #[test]
    fn malformed_json_is_a_parse_error() {
        let err = parse_pmd_output("not json").expect_err("err");
        assert!(matches!(err, RunnerError::Parse { .. }));
    }

    #[test]
    fn line_zero_coerces_to_one() {
        let json = r#"{
            "files":[{"filename":"x.java","violations":[
                {"beginline":0,"endline":0,"description":"d","rule":"R","priority":3}
            ]}]
        }"#;
        let f = parse_pmd_output(json).expect("ok");
        assert_eq!(f[0].line_start, 1);
        assert_eq!(f[0].line_end, 1);
    }

    #[test]
    fn end_line_below_start_is_clamped() {
        let json = r#"{
            "files":[{"filename":"x.java","violations":[
                {"beginline":10,"endline":5,"description":"d","rule":"R","priority":3}
            ]}]
        }"#;
        let f = parse_pmd_output(json).expect("ok");
        assert_eq!(f[0].line_start, 10);
        assert_eq!(f[0].line_end, 10);
    }
}

//! sqlfluff runner. Parses `sqlfluff lint --format json` output.
//!
//! sqlfluff is the de-facto SQL linter — covers PostgreSQL, MySQL,
//! Snowflake, BigQuery, Redshift, Spark, and several others via
//! configurable `dialect`. Output structure: a top-level array of
//! `{filepath, violations: [{line_no, code, description}]}` records.
//!
//! sqlfluff doesn't tier its findings by severity; we surface
//! everything as Warning. The rule code (e.g. `L010`, `CV01`) goes
//! into rule_id so the LLM can decide what's important.

use crate::finding::{Finding, Severity};
use crate::runner::{run_in_sandbox, LinterRunner, RunnerError};
use ar_sandbox::Sandbox;
use async_trait::async_trait;
use serde::Deserialize;
use std::path::Path;

const TOOL: &str = "sqlfluff";

#[derive(Debug, Deserialize)]
struct SqlfluffFile {
    filepath: String,
    #[serde(default)]
    violations: Vec<SqlfluffViolation>,
}

#[derive(Debug, Deserialize)]
struct SqlfluffViolation {
    line_no: u32,
    code: String,
    description: String,
}

pub fn parse_sqlfluff_output(json: &str) -> Result<Vec<Finding>, RunnerError> {
    let raw: Vec<SqlfluffFile> = serde_json::from_str(json).map_err(|e| RunnerError::Parse {
        tool: TOOL.into(),
        detail: e.to_string(),
    })?;
    let mut out = Vec::new();
    for file in raw {
        for v in file.violations {
            out.push(Finding {
                source_tool: TOOL.into(),
                rule_id: Some(v.code),
                path: file.filepath.clone(),
                line_start: v.line_no,
                line_end: v.line_no,
                severity: Severity::Warning,
                message: v.description,
            });
        }
    }
    Ok(out)
}

pub struct SqlfluffRunner {
    /// Files to scan, repo-relative. Empty means "scan no files".
    pub files: Vec<String>,
}

#[async_trait]
impl LinterRunner for SqlfluffRunner {
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
        let mut args = vec!["lint".into(), "--format".into(), "json".into()];
        args.extend(self.files.iter().cloned());
        let output = run_in_sandbox(sandbox, repo_dir, "sqlfluff", args, vec![]).await?;
        if output.stdout.is_empty() {
            return Ok(vec![]);
        }
        let stdout = String::from_utf8_lossy(&output.stdout);
        parse_sqlfluff_output(&stdout)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_typical_sqlfluff_output() {
        let json = r#"[
            {
                "filepath": "queries/users.sql",
                "violations": [
                    {
                        "line_no": 3,
                        "line_pos": 1,
                        "code": "L010",
                        "description": "Keywords must be consistently upper case."
                    },
                    {
                        "line_no": 7,
                        "line_pos": 12,
                        "code": "CV01",
                        "description": "Avoid using SELECT *."
                    }
                ]
            },
            {
                "filepath": "queries/clean.sql",
                "violations": []
            }
        ]"#;
        let f = parse_sqlfluff_output(json).expect("ok");
        assert_eq!(f.len(), 2);
        assert_eq!(f[0].path, "queries/users.sql");
        assert_eq!(f[0].rule_id.as_deref(), Some("L010"));
        assert_eq!(f[0].line_start, 3);
        assert_eq!(f[0].severity, Severity::Warning);
        assert!(f[0].message.contains("Keywords"));
        assert_eq!(f[1].rule_id.as_deref(), Some("CV01"));
    }

    #[test]
    fn empty_array_yields_zero_findings() {
        let f = parse_sqlfluff_output("[]").expect("ok");
        assert!(f.is_empty());
    }

    #[test]
    fn malformed_json_is_a_parse_error() {
        let err = parse_sqlfluff_output("not json").expect_err("err");
        assert!(matches!(err, RunnerError::Parse { .. }));
    }

    #[test]
    fn file_with_no_violations_contributes_nothing() {
        let json = r#"[
            {"filepath":"a.sql","violations":[]},
            {"filepath":"b.sql","violations":[]}
        ]"#;
        let f = parse_sqlfluff_output(json).expect("ok");
        assert!(f.is_empty());
    }

    #[test]
    fn line_end_mirrors_line_start() {
        let json = r#"[{
            "filepath":"x.sql",
            "violations":[{"line_no":42,"line_pos":1,"code":"L1","description":"d"}]
        }]"#;
        let f = parse_sqlfluff_output(json).expect("ok");
        assert_eq!(f[0].line_start, 42);
        assert_eq!(f[0].line_end, 42);
    }
}

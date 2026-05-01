//! Biome runner. Parses `biome lint --reporter=json` output.
//!
//! Biome is a single fast Rust binary that lints + formats JS / TS /
//! JSX / TSX. It overlaps in scope with eslint but covers different
//! rules and is significantly faster (no Node startup, no plugin
//! resolution). We run both because a repo using one frequently
//! configures the other for separate concerns.
//!
//! Output structure: a top-level `{diagnostics: [...]}` array of
//! `{location: {path, span: {start, end}}, severity, description,
//! category}` records. Spans are byte offsets, not lines — biome's
//! JSON reporter doesn't surface line numbers directly. We
//! conservatively map every finding to line 1 with a non-empty rule_id;
//! the LLM gets the message + file path which is enough to correlate
//! against the diff.

use crate::finding::{Finding, Severity};
use crate::runner::{run_in_sandbox, LinterRunner, RunnerError};
use ar_sandbox::Sandbox;
use async_trait::async_trait;
use serde::Deserialize;
use std::path::Path;

const TOOL: &str = "biome";

#[derive(Debug, Deserialize)]
struct BiomeOutput {
    #[serde(default)]
    diagnostics: Vec<BiomeDiagnostic>,
}

#[derive(Debug, Deserialize)]
struct BiomeDiagnostic {
    #[serde(default)]
    category: String,
    #[serde(default)]
    severity: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    location: Option<BiomeLocation>,
}

#[derive(Debug, Deserialize)]
struct BiomeLocation {
    #[serde(default)]
    path: BiomePath,
    /// Some biome versions emit a `sourceCode` section with line+column
    /// indexing in addition to the byte-offset span. When present, we
    /// take the line number from there.
    #[serde(default)]
    span: Option<BiomeSpan>,
}

#[derive(Debug, Deserialize, Default)]
struct BiomePath {
    #[serde(default)]
    file: BiomePathFile,
}

#[derive(Debug, Deserialize, Default)]
struct BiomePathFile {
    #[serde(default)]
    path: String,
}

#[derive(Debug, Deserialize, Default)]
struct BiomeSpan {
    #[serde(default)]
    line: Option<u32>,
}

pub fn parse_biome_output(json: &str) -> Result<Vec<Finding>, RunnerError> {
    let raw: BiomeOutput = serde_json::from_str(json).map_err(|e| RunnerError::Parse {
        tool: TOOL.into(),
        detail: e.to_string(),
    })?;
    Ok(raw
        .diagnostics
        .into_iter()
        .map(|d| {
            let path = d
                .location
                .as_ref()
                .map(|l| l.path.file.path.clone())
                .unwrap_or_default();
            let line = d
                .location
                .as_ref()
                .and_then(|l| l.span.as_ref())
                .and_then(|s| s.line)
                .unwrap_or(1);
            Finding {
                source_tool: TOOL.into(),
                rule_id: if d.category.is_empty() {
                    None
                } else {
                    Some(d.category)
                },
                path,
                line_start: line,
                line_end: line,
                severity: severity_from(&d.severity),
                message: d.description,
            }
        })
        .collect())
}

fn severity_from(level: &str) -> Severity {
    match level.to_ascii_lowercase().as_str() {
        "error" | "fatal" => Severity::Error,
        "warning" | "warn" => Severity::Warning,
        _ => Severity::Note,
    }
}

pub struct BiomeRunner {
    /// Files to scan, repo-relative. Empty means "scan no files".
    pub files: Vec<String>,
}

#[async_trait]
impl LinterRunner for BiomeRunner {
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
        let mut args = vec!["lint".into(), "--reporter=json".into()];
        args.extend(self.files.iter().cloned());
        let output = run_in_sandbox(sandbox, repo_dir, "biome", args, vec![]).await?;
        if output.stdout.is_empty() {
            return Ok(vec![]);
        }
        let stdout = String::from_utf8_lossy(&output.stdout);
        parse_biome_output(&stdout)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_typical_biome_output() {
        let json = r#"{
            "diagnostics": [
                {
                    "category": "lint/correctness/noUnusedVariables",
                    "severity": "warning",
                    "description": "This variable is unused.",
                    "location": {
                        "path": {"file": {"path": "src/a.ts"}},
                        "span": {"line": 7}
                    }
                },
                {
                    "category": "lint/style/useConst",
                    "severity": "error",
                    "description": "Use const.",
                    "location": {
                        "path": {"file": {"path": "src/b.tsx"}}
                    }
                }
            ]
        }"#;
        let f = parse_biome_output(json).expect("ok");
        assert_eq!(f.len(), 2);
        assert_eq!(
            f[0].rule_id.as_deref(),
            Some("lint/correctness/noUnusedVariables")
        );
        assert_eq!(f[0].path, "src/a.ts");
        assert_eq!(f[0].line_start, 7);
        assert_eq!(f[0].severity, Severity::Warning);
        // Line falls back to 1 when biome emits no span.line.
        assert_eq!(f[1].line_start, 1);
        assert_eq!(f[1].severity, Severity::Error);
    }

    #[test]
    fn empty_diagnostics_yields_zero_findings() {
        let f = parse_biome_output(r#"{"diagnostics":[]}"#).expect("ok");
        assert!(f.is_empty());
    }

    #[test]
    fn missing_diagnostics_field_decodes_to_empty() {
        let f = parse_biome_output("{}").expect("ok");
        assert!(f.is_empty());
    }

    #[test]
    fn malformed_json_is_a_parse_error() {
        let err = parse_biome_output("not json").expect_err("err");
        assert!(matches!(err, RunnerError::Parse { .. }));
    }

    #[test]
    fn empty_category_drops_rule_id() {
        let json = r#"{
            "diagnostics": [{
                "category": "",
                "severity": "warning",
                "description": "...",
                "location": {"path": {"file": {"path": "a.js"}}}
            }]
        }"#;
        let f = parse_biome_output(json).expect("ok");
        assert!(f[0].rule_id.is_none());
    }

    #[test]
    fn unknown_severity_falls_back_to_note() {
        let json = r#"{
            "diagnostics": [{
                "category": "lint/x",
                "severity": "info",
                "description": "...",
                "location": {"path": {"file": {"path": "a.js"}}}
            }]
        }"#;
        let f = parse_biome_output(json).expect("ok");
        assert_eq!(f[0].severity, Severity::Note);
    }
}

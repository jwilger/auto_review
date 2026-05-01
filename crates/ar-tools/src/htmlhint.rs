//! htmlhint runner. Parses `htmlhint -f json` output for HTML
//! lint findings.
//!
//! htmlhint covers tag pairing, attribute conventions, accessibility
//! basics (alt text, lang attribute), and common HTML mistakes.
//! Routes on `.html` / `.htm` / `.xhtml` files; runs nowhere else.
//!
//! Output structure: top-level array of `{file, messages: [{type,
//! message, rule: {id}, line, col}]}`. Severity values: `error` →
//! Error, `warning` → Warning. htmlhint emits per-file objects even
//! when the file has no findings (empty `messages` array); we just
//! skip those.

use crate::finding::{Finding, Severity};
use crate::runner::{run_in_sandbox, LinterRunner, RunnerError};
use ar_sandbox::Sandbox;
use async_trait::async_trait;
use serde::Deserialize;
use std::path::Path;

const TOOL: &str = "htmlhint";

#[derive(Debug, Deserialize)]
struct HtmlhintFile {
    file: String,
    #[serde(default)]
    messages: Vec<HtmlhintMessage>,
}

#[derive(Debug, Deserialize)]
struct HtmlhintMessage {
    #[serde(default, rename = "type")]
    kind: String,
    #[serde(default)]
    message: String,
    #[serde(default)]
    rule: HtmlhintRule,
    #[serde(default)]
    line: u32,
}

#[derive(Debug, Default, Deserialize)]
struct HtmlhintRule {
    #[serde(default)]
    id: Option<String>,
}

pub fn parse_htmlhint_output(json: &str) -> Result<Vec<Finding>, RunnerError> {
    let raw: Vec<HtmlhintFile> = serde_json::from_str(json).map_err(|e| RunnerError::Parse {
        tool: TOOL.into(),
        detail: e.to_string(),
    })?;
    let mut out = Vec::new();
    for file in raw {
        for m in file.messages {
            let line = m.line.max(1);
            out.push(Finding {
                source_tool: TOOL.into(),
                rule_id: m.rule.id,
                path: file.file.clone(),
                line_start: line,
                line_end: line,
                severity: severity_from(&m.kind),
                message: m.message,
            });
        }
    }
    Ok(out)
}

fn severity_from(kind: &str) -> Severity {
    match kind.to_ascii_lowercase().as_str() {
        "error" => Severity::Error,
        "warning" | "warn" => Severity::Warning,
        _ => Severity::Note,
    }
}

pub struct HtmlhintRunner {
    /// Files to scan, repo-relative. Empty means "scan no files".
    pub files: Vec<String>,
}

#[async_trait]
impl LinterRunner for HtmlhintRunner {
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
        // -f json    structured output
        // --nocolor  defensive against ANSI bytes
        let mut args = vec!["-f".into(), "json".into(), "--nocolor".into()];
        args.extend(self.files.iter().cloned());
        let output = run_in_sandbox(sandbox, repo_dir, "htmlhint", args, vec![]).await?;
        if output.stdout.is_empty() {
            return Ok(vec![]);
        }
        let stdout = String::from_utf8_lossy(&output.stdout);
        parse_htmlhint_output(&stdout)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_typical_htmlhint_output() {
        let json = r#"[
            {
                "file": "public/index.html",
                "messages": [
                    {
                        "type": "warning",
                        "message": "Tag must be paired, missing: [ </p> ]",
                        "rule": {"id": "tag-pair", "description": "..."},
                        "evidence": "<p>",
                        "line": 5,
                        "col": 1
                    },
                    {
                        "type": "error",
                        "message": "An <img> element must have an alt attribute.",
                        "rule": {"id": "alt-require"},
                        "line": 12,
                        "col": 1
                    }
                ]
            },
            {
                "file": "public/clean.html",
                "messages": []
            }
        ]"#;
        let f = parse_htmlhint_output(json).expect("ok");
        assert_eq!(f.len(), 2);
        assert_eq!(f[0].path, "public/index.html");
        assert_eq!(f[0].line_start, 5);
        assert_eq!(f[0].rule_id.as_deref(), Some("tag-pair"));
        assert_eq!(f[0].severity, Severity::Warning);
        assert!(f[0].message.contains("Tag must be paired"));
        assert_eq!(f[1].severity, Severity::Error);
        assert_eq!(f[1].rule_id.as_deref(), Some("alt-require"));
    }

    #[test]
    fn empty_array_yields_zero_findings() {
        let f = parse_htmlhint_output("[]").expect("ok");
        assert!(f.is_empty());
    }

    #[test]
    fn file_with_no_messages_contributes_nothing() {
        let json = r#"[{"file":"clean.html","messages":[]}]"#;
        let f = parse_htmlhint_output(json).expect("ok");
        assert!(f.is_empty());
    }

    #[test]
    fn malformed_json_is_a_parse_error() {
        let err = parse_htmlhint_output("not json").expect_err("err");
        assert!(matches!(err, RunnerError::Parse { .. }));
    }

    #[test]
    fn unknown_severity_falls_back_to_note() {
        let json = r#"[{"file":"x.html","messages":[
            {"type":"info","message":"m","rule":{"id":"r"},"line":1,"col":1}
        ]}]"#;
        let f = parse_htmlhint_output(json).expect("ok");
        assert_eq!(f[0].severity, Severity::Note);
    }

    #[test]
    fn missing_rule_id_drops_field() {
        let json = r#"[{"file":"x.html","messages":[
            {"type":"warning","message":"m","line":1,"col":1}
        ]}]"#;
        let f = parse_htmlhint_output(json).expect("ok");
        assert!(f[0].rule_id.is_none());
    }

    #[test]
    fn line_zero_coerces_to_one() {
        let json = r#"[{"file":"x.html","messages":[
            {"type":"warning","message":"m","rule":{"id":"R"},"line":0,"col":1}
        ]}]"#;
        let f = parse_htmlhint_output(json).expect("ok");
        assert_eq!(f[0].line_start, 1);
        assert_eq!(f[0].line_end, 1);
    }
}

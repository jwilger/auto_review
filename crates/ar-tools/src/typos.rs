//! typos runner. Parses `typos --format json` output for spelling
//! mistakes in source code (identifiers, comments, strings).
//!
//! typos is a fast Rust-based typo finder. Distinct from vale
//! (which only checks prose in markdown); typos walks the whole
//! source tree and catches misspellings in identifier names,
//! function/variable names, doc comments, and string literals.
//!
//! Output format: JSON-Lines, one record per typo with
//! `{type, path, line_num, byte_offset, typo, corrections}`. Most
//! records have `type: "typo"`; a top-level "summary" record may
//! also appear with `type: "summary"` (we filter it out).

use crate::finding::{Finding, Severity};
use crate::runner::{run_in_sandbox, LinterRunner, RunnerError};
use ar_sandbox::Sandbox;
use async_trait::async_trait;
use serde::Deserialize;
use std::path::Path;

const TOOL: &str = "typos";

#[derive(Debug, Deserialize)]
struct TyposRecord {
    #[serde(rename = "type")]
    #[serde(default)]
    kind: String,
    #[serde(default)]
    path: String,
    #[serde(default)]
    line_num: u32,
    #[serde(default)]
    typo: String,
    #[serde(default)]
    corrections: Vec<String>,
}

pub fn parse_typos_output(text: &str) -> Result<Vec<Finding>, RunnerError> {
    let mut out = Vec::new();
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() {
            continue;
        }
        let r: TyposRecord = serde_json::from_str(line).map_err(|e| RunnerError::Parse {
            tool: TOOL.into(),
            detail: format!("line {:?}: {e}", line),
        })?;
        // Filter out the summary record that typos emits at the end
        // of every run; only `type: "typo"` records carry findings.
        if r.kind != "typo" {
            continue;
        }
        let line_no = r.line_num.max(1);
        let suggestion = if r.corrections.is_empty() {
            String::new()
        } else {
            format!(" → {}", r.corrections.join(" / "))
        };
        out.push(Finding {
            source_tool: TOOL.into(),
            // typos doesn't have rule ids; use the typo'd token so
            // the reviewer can see which word triggered the finding
            // even without reading the full message.
            rule_id: Some(r.typo.clone()),
            path: r.path,
            line_start: line_no,
            line_end: line_no,
            severity: Severity::Note,
            message: format!("Possible typo: '{}'{}", r.typo, suggestion),
        });
    }
    Ok(out)
}

pub struct TyposRunner;

#[async_trait]
impl LinterRunner for TyposRunner {
    fn name(&self) -> &str {
        TOOL
    }

    async fn run(
        &self,
        sandbox: &dyn Sandbox,
        repo_dir: &Path,
    ) -> Result<Vec<Finding>, RunnerError> {
        // --format json     JSON-Lines output (one record per typo)
        // --no-config       skip user-level config; only the repo's
        //                   .typos.toml (if any) influences the run.
        //                   Repos without a config still get the
        //                   built-in dictionary.
        let output = run_in_sandbox(
            sandbox,
            repo_dir,
            "typos",
            vec!["--format".into(), "json".into()],
            vec![],
        )
        .await?;
        let stdout = String::from_utf8_lossy(&output.stdout);
        if stdout.trim().is_empty() {
            return Ok(vec![]);
        }
        parse_typos_output(&stdout)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_typical_typos_output() {
        let text = "\
{\"type\":\"typo\",\"path\":\"src/auth.rs\",\"line_num\":42,\"byte_offset\":12,\"typo\":\"recieve\",\"corrections\":[\"receive\"]}
{\"type\":\"typo\",\"path\":\"docs/intro.md\",\"line_num\":3,\"byte_offset\":0,\"typo\":\"seperate\",\"corrections\":[\"separate\"]}
{\"type\":\"summary\",\"typos\":2,\"files_with_typos\":2,\"files_checked\":17}
";
        let f = parse_typos_output(text).expect("ok");
        // The summary line is filtered.
        assert_eq!(f.len(), 2);
        assert_eq!(f[0].path, "src/auth.rs");
        assert_eq!(f[0].line_start, 42);
        assert_eq!(f[0].rule_id.as_deref(), Some("recieve"));
        assert_eq!(f[0].severity, Severity::Note);
        assert!(f[0].message.contains("recieve"));
        assert!(f[0].message.contains("receive"));
        assert_eq!(f[1].path, "docs/intro.md");
    }

    #[test]
    fn empty_input_yields_zero_findings() {
        let f = parse_typos_output("").expect("ok");
        assert!(f.is_empty());
    }

    #[test]
    fn summary_only_input_yields_zero_findings() {
        let text = r#"{"type":"summary","typos":0,"files_with_typos":0,"files_checked":99}"#;
        let f = parse_typos_output(text).expect("ok");
        assert!(f.is_empty());
    }

    #[test]
    fn malformed_line_is_a_parse_error() {
        let err = parse_typos_output("this is not json\n").expect_err("err");
        assert!(matches!(err, RunnerError::Parse { .. }));
    }

    #[test]
    fn typo_without_corrections_omits_arrow() {
        let text = r#"{"type":"typo","path":"x.rs","line_num":1,"typo":"xyzqp","corrections":[]}"#;
        let f = parse_typos_output(text).expect("ok");
        assert_eq!(f.len(), 1);
        // No "→" since there's nothing to suggest.
        assert!(!f[0].message.contains("→"));
        assert!(f[0].message.contains("xyzqp"));
    }

    #[test]
    fn multiple_corrections_are_joined() {
        let text = r#"{"type":"typo","path":"x.rs","line_num":1,"typo":"trough","corrections":["through","thorough"]}"#;
        let f = parse_typos_output(text).expect("ok");
        assert!(f[0].message.contains("through"));
        assert!(f[0].message.contains("thorough"));
    }

    #[test]
    fn line_zero_coerces_to_one() {
        let text = r#"{"type":"typo","path":"x","line_num":0,"typo":"a","corrections":[]}"#;
        let f = parse_typos_output(text).expect("ok");
        assert_eq!(f[0].line_start, 1);
        assert_eq!(f[0].line_end, 1);
    }
}

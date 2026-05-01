//! prettier runner. Parses `prettier --check` text output for
//! format-drift findings.
//!
//! prettier focuses purely on formatting (line length, quote style,
//! trailing commas, indentation) — orthogonal to biome/eslint/oxlint
//! which check semantics. Routes on JS/TS, CSS, JSON, YAML, and
//! Markdown — every file extension we already lint that prettier
//! covers. Repos without a `.prettierrc` skip silently because
//! `--check` errors before scanning when no config is found, but
//! `--config-precedence prefer-file` falls back to prettier's
//! built-in defaults so we still get useful coverage.
//!
//! prettier doesn't have a JSON reporter; the text output of
//! `--check` is one `[warn] <path>` line per file that needs
//! formatting plus a banner and summary. We emit one Finding per
//! flagged file at line 1 with a generic "Code-style drift detected
//! by prettier; run `prettier --write`." message.

use crate::finding::{Finding, Severity};
use crate::runner::{run_in_sandbox, LinterRunner, RunnerError};
use ar_sandbox::Sandbox;
use async_trait::async_trait;
use std::path::Path;

const TOOL: &str = "prettier";

pub fn parse_prettier_output(text: &str) -> Result<Vec<Finding>, RunnerError> {
    let mut out = Vec::new();
    for raw in text.lines() {
        let line = raw.trim();
        // We only care about per-file warning lines. The summary
        // and banner lines start with different prefixes; skip
        // anything else.
        let Some(rest) = line.strip_prefix("[warn] ") else {
            continue;
        };
        // The summary line `[warn] Code style issues found in N
        // files. …` doesn't refer to a single file. Skip it by
        // looking for the keyword.
        if rest.starts_with("Code style issues") || rest.starts_with("Code style issue") {
            continue;
        }
        let path = rest.to_string();
        if path.is_empty() {
            continue;
        }
        out.push(Finding {
            source_tool: TOOL.into(),
            rule_id: Some("format-drift".into()),
            path,
            line_start: 1,
            line_end: 1,
            severity: Severity::Warning,
            message: "Code-style drift detected by prettier; run `prettier --write` to fix.".into(),
        });
    }
    Ok(out)
}

pub struct PrettierRunner {
    /// Files to scan, repo-relative. Empty means "scan no files".
    pub files: Vec<String>,
}

#[async_trait]
impl LinterRunner for PrettierRunner {
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
        // --check    report files that need reformatting; never
        //            modify on disk.
        // --no-color defensive against ANSI bytes.
        // We don't pass --error-on-unmatched-pattern (defaults to
        // erroring); --no-error-on-unmatched-pattern keeps the
        // run going when one of our routed paths doesn't match
        // prettier's supported extensions.
        let mut args = vec![
            "--check".into(),
            "--no-color".into(),
            "--no-error-on-unmatched-pattern".into(),
        ];
        args.extend(self.files.iter().cloned());
        let output = run_in_sandbox(sandbox, repo_dir, "prettier", args, vec![]).await?;
        // prettier writes warnings to stderr.
        let combined = format!(
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
        if combined.trim().is_empty() {
            return Ok(vec![]);
        }
        parse_prettier_output(&combined)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_typical_prettier_output() {
        let text = "\
Checking formatting...
[warn] src/foo.ts
[warn] src/bar.tsx
[warn] Code style issues found in 2 files. Run Prettier to fix.
";
        let f = parse_prettier_output(text).expect("ok");
        assert_eq!(f.len(), 2);
        assert_eq!(f[0].path, "src/foo.ts");
        assert_eq!(f[0].line_start, 1);
        assert_eq!(f[0].rule_id.as_deref(), Some("format-drift"));
        assert_eq!(f[0].severity, Severity::Warning);
        assert!(f[0].message.contains("prettier"));
        assert_eq!(f[1].path, "src/bar.tsx");
    }

    #[test]
    fn empty_input_yields_zero_findings() {
        let f = parse_prettier_output("").expect("ok");
        assert!(f.is_empty());
    }

    #[test]
    fn banner_and_summary_lines_are_skipped() {
        let text = "\
Checking formatting...
[warn] Code style issues found in 5 files. Run Prettier to fix.
All matched files use Prettier code style!
";
        let f = parse_prettier_output(text).expect("ok");
        assert!(f.is_empty());
    }

    #[test]
    fn singular_summary_line_is_also_skipped() {
        // prettier uses "Code style issue" (singular) for N=1.
        let text = "\
[warn] Code style issue found in 1 file. Run Prettier to fix.
";
        let f = parse_prettier_output(text).expect("ok");
        assert!(f.is_empty());
    }

    #[test]
    fn unrecognized_lines_are_silently_skipped() {
        let text = "\
random progress noise
[error] some other channel
[warn] keepers/this.css
";
        let f = parse_prettier_output(text).expect("ok");
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].path, "keepers/this.css");
    }

    #[test]
    fn empty_warning_line_is_skipped() {
        let text = "[warn] \n";
        let f = parse_prettier_output(text).expect("ok");
        assert!(f.is_empty());
    }
}

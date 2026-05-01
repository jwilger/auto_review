//! shfmt runner. Parses `shfmt -d` unified-diff output for shell-
//! script formatting drift.
//!
//! shfmt focuses purely on formatting (indentation, spacing,
//! redirection layout, command-substitution style) — orthogonal to
//! shellcheck's bug-finding. Routes on `.sh` / `.bash` files
//! alongside shellcheck. Repos that get formatting from another
//! tool can disable shfmt via `.auto_review.yaml`'s
//! `disabled_tools`.
//!
//! Output: shfmt prints a unified diff to stdout for each file that
//! differs from canonical formatting. We parse just the `--- path`
//! and `+++ path` lines; one Finding per affected file at line 1
//! with a generic "Code-style drift detected by shfmt; run
//! `shfmt -w` to fix." message.

use crate::finding::{Finding, Severity};
use crate::runner::{run_in_sandbox, LinterRunner, RunnerError};
use ar_sandbox::Sandbox;
use async_trait::async_trait;
use std::collections::BTreeSet;
use std::path::Path;

const TOOL: &str = "shfmt";

pub fn parse_shfmt_output(text: &str) -> Result<Vec<Finding>, RunnerError> {
    // shfmt's diff headers look like `--- path.sh.orig` and
    // `+++ path.sh`. We pin on the `+++ ` prefix to get the
    // post-format file path; the `.orig` suffix on `---` is a
    // shfmt-specific artefact we don't need.
    let mut paths: BTreeSet<String> = BTreeSet::new();
    for raw in text.lines() {
        let line = raw.trim_end();
        let Some(rest) = line.strip_prefix("+++ ") else {
            continue;
        };
        // shfmt sometimes appends a tab + timestamp like
        // `+++ a.sh\t2024-01-01`. Cut at the first tab if present.
        let path = match rest.find('\t') {
            Some(idx) => &rest[..idx],
            None => rest,
        };
        let trimmed = path.trim();
        if trimmed.is_empty() || trimmed == "/dev/null" {
            continue;
        }
        paths.insert(trimmed.to_string());
    }
    Ok(paths
        .into_iter()
        .map(|path| Finding {
            source_tool: TOOL.into(),
            rule_id: Some("format-drift".into()),
            path,
            line_start: 1,
            line_end: 1,
            severity: Severity::Warning,
            message: "Code-style drift detected by shfmt; run `shfmt -w` to fix.".into(),
        })
        .collect())
}

pub struct ShfmtRunner {
    /// Files to scan, repo-relative. Empty means "scan no files".
    pub files: Vec<String>,
}

#[async_trait]
impl LinterRunner for ShfmtRunner {
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
        // -d   diff mode (no in-place edits)
        // -i 4 default indent width when the repo has no .editorconfig
        let mut args = vec!["-d".into(), "-i".into(), "4".into()];
        args.extend(self.files.iter().cloned());
        let output = run_in_sandbox(sandbox, repo_dir, "shfmt", args, vec![]).await?;
        if output.stdout.is_empty() {
            return Ok(vec![]);
        }
        let stdout = String::from_utf8_lossy(&output.stdout);
        parse_shfmt_output(&stdout)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_typical_shfmt_diff_output() {
        let text = "\
--- scripts/build.sh.orig
+++ scripts/build.sh
@@ -1,4 +1,4 @@
 #!/bin/bash
-set -e
+set -euo pipefail
 echo hi
--- scripts/deploy.sh.orig
+++ scripts/deploy.sh
@@ -3,4 +3,4 @@
   keepers
-foo
+    foo
   keepers
";
        let f = parse_shfmt_output(text).expect("ok");
        assert_eq!(f.len(), 2);
        assert_eq!(f[0].path, "scripts/build.sh");
        assert_eq!(f[0].line_start, 1);
        assert_eq!(f[0].rule_id.as_deref(), Some("format-drift"));
        assert_eq!(f[0].severity, Severity::Warning);
        assert!(f[0].message.contains("shfmt"));
        assert_eq!(f[1].path, "scripts/deploy.sh");
    }

    #[test]
    fn empty_input_yields_zero_findings() {
        let f = parse_shfmt_output("").expect("ok");
        assert!(f.is_empty());
    }

    #[test]
    fn input_without_diff_headers_yields_zero_findings() {
        let text = "Some progress chatter\nfoo\nbar\n";
        let f = parse_shfmt_output(text).expect("ok");
        assert!(f.is_empty());
    }

    #[test]
    fn multiple_changes_to_one_file_emit_one_finding() {
        // shfmt only emits one `+++ path` per file; the dedup is
        // structural in our parser via BTreeSet.
        let text = "\
+++ a.sh
@@ -1,2 +1,2 @@
 a
-b
+B
";
        let f = parse_shfmt_output(text).expect("ok");
        assert_eq!(f.len(), 1);
    }

    #[test]
    fn dev_null_target_is_skipped() {
        let text = "+++ /dev/null\n+++ real.sh\n";
        let f = parse_shfmt_output(text).expect("ok");
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].path, "real.sh");
    }

    #[test]
    fn timestamp_after_path_is_stripped() {
        let text = "+++ scripts/x.sh\t2024-01-01 12:00:00.000000000 +0000\n";
        let f = parse_shfmt_output(text).expect("ok");
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].path, "scripts/x.sh");
    }

    #[test]
    fn empty_path_is_skipped() {
        let text = "+++ \n+++ ok.sh\n";
        let f = parse_shfmt_output(text).expect("ok");
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].path, "ok.sh");
    }
}

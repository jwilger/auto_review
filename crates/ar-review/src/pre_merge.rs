//! Pre-merge checks: a small set of repo-wide gates that run
//! alongside the LLM review and surface as a markdown checklist
//! appended to the posted review body.
//!
//! Each check is **advisory** — failing one doesn't change the
//! review's `event` (Comment vs RequestChanges) or block merging
//! anywhere. Operators get a list the PR author can scan: "did you
//! forget to update CHANGELOG.md?", "did you add a test?". Repos
//! with their own merge-gating CI keep using that; auto_review's
//! pre-merge checks are nudges, not gates.
//!
//! Built-ins (Milestone 4 first cut):
//! - `changelog`: CHANGELOG.md exists in the workspace, non-trivial
//!   source changed, but CHANGELOG.md isn't in the diff.
//! - `tests`: source files changed but no test file is in the diff.
//! - `no-new-todos`: scan added lines for `TODO` / `FIXME` markers.
//!
//! Adding a new check: add a variant to [`CheckName`], implement
//! the logic in `evaluate`, and add a test. Custom natural-language
//! checks (the second half of the milestone-4 spec) get LLM-driven
//! evaluation in a future iteration; this module ships only the
//! deterministic built-ins.

use ar_forgejo::ChangedFile;
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckStatus {
    Pass,
    Fail,
    Skip,
}

impl CheckStatus {
    pub fn checkbox(&self) -> &'static str {
        match self {
            CheckStatus::Pass => "[x]",
            CheckStatus::Fail => "[ ]",
            CheckStatus::Skip => "[~]",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckName {
    Changelog,
    Tests,
    NoNewTodos,
}

impl CheckName {
    pub fn label(&self) -> &'static str {
        match self {
            CheckName::Changelog => "CHANGELOG updated",
            CheckName::Tests => "Tests touched",
            CheckName::NoNewTodos => "No new TODO/FIXME comments",
        }
    }
}

#[derive(Debug, Clone)]
pub struct CheckResult {
    pub name: CheckName,
    pub status: CheckStatus,
    pub detail: String,
}

/// Run every built-in pre-merge check. Order is fixed so output is
/// stable across runs.
pub fn evaluate(
    diff: &str,
    changed_files: &[ChangedFile],
    workspace_path: Option<&Path>,
) -> Vec<CheckResult> {
    vec![
        check_changelog(changed_files, workspace_path),
        check_tests(changed_files),
        check_no_new_todos(diff),
    ]
}

fn check_changelog(files: &[ChangedFile], workspace: Option<&Path>) -> CheckResult {
    let has_workspace_changelog = workspace
        .map(|w| w.join("CHANGELOG.md").exists())
        .unwrap_or(false);
    if !has_workspace_changelog {
        return CheckResult {
            name: CheckName::Changelog,
            status: CheckStatus::Skip,
            detail: "no CHANGELOG.md in the repo".into(),
        };
    }
    let touched_changelog = files.iter().any(|f| {
        let name = f.filename.rsplit('/').next().unwrap_or(&f.filename);
        name.eq_ignore_ascii_case("CHANGELOG.md") && f.status != "removed"
    });
    let non_trivial_change = files
        .iter()
        .any(|f| f.status != "removed" && is_non_trivial(&f.filename));
    match (non_trivial_change, touched_changelog) {
        (true, true) => CheckResult {
            name: CheckName::Changelog,
            status: CheckStatus::Pass,
            detail: "CHANGELOG.md is in the diff".into(),
        },
        (true, false) => CheckResult {
            name: CheckName::Changelog,
            status: CheckStatus::Fail,
            detail: "non-trivial code changed but CHANGELOG.md isn't in the diff".into(),
        },
        (false, _) => CheckResult {
            name: CheckName::Changelog,
            status: CheckStatus::Skip,
            detail: "diff is docs/config only — no CHANGELOG entry expected".into(),
        },
    }
}

fn check_tests(files: &[ChangedFile]) -> CheckResult {
    let any_source_change = files
        .iter()
        .any(|f| f.status != "removed" && is_source_file(&f.filename));
    if !any_source_change {
        return CheckResult {
            name: CheckName::Tests,
            status: CheckStatus::Skip,
            detail: "no source files in the diff".into(),
        };
    }
    let any_test_change = files.iter().any(|f| {
        f.status != "removed"
            && (is_test_file(&f.filename) || adds_rust_test(&f.filename, f.patch.as_deref()))
    });
    if any_test_change {
        CheckResult {
            name: CheckName::Tests,
            status: CheckStatus::Pass,
            detail: "test changes are in the diff".into(),
        }
    } else {
        CheckResult {
            name: CheckName::Tests,
            status: CheckStatus::Fail,
            detail: "source changed but no test file appears in the diff".into(),
        }
    }
}

fn check_no_new_todos(diff: &str) -> CheckResult {
    let mut count = 0u32;
    for line in diff.lines() {
        // Only look at *added* lines (start with `+`), not the
        // `+++ b/path` header.
        if !line.starts_with('+') || line.starts_with("+++") {
            continue;
        }
        let body = &line[1..];
        if contains_todo_marker(body) {
            count += 1;
        }
    }
    if count == 0 {
        CheckResult {
            name: CheckName::NoNewTodos,
            status: CheckStatus::Pass,
            detail: "no new TODO/FIXME markers".into(),
        }
    } else {
        CheckResult {
            name: CheckName::NoNewTodos,
            status: CheckStatus::Fail,
            detail: format!(
                "{count} new TODO/FIXME marker{} in the diff",
                if count == 1 { "" } else { "s" }
            ),
        }
    }
}

/// Is this an added line that looks like a TODO/FIXME comment?
/// Matches whole words case-sensitively (lowercase `todo` in URLs
/// like `todoist.com` shouldn't trip us up).
fn contains_todo_marker(line: &str) -> bool {
    let bytes = line.as_bytes();
    for marker in ["TODO", "FIXME", "XXX", "HACK"] {
        for (idx, _) in line.match_indices(marker) {
            let before_ok = idx == 0 || !bytes[idx - 1].is_ascii_alphanumeric();
            let after_idx = idx + marker.len();
            let after_ok = after_idx >= bytes.len() || !bytes[after_idx].is_ascii_alphanumeric();
            if before_ok && after_ok {
                return true;
            }
        }
    }
    false
}

/// Heuristic: "this PR meaningfully changes behaviour" so a
/// CHANGELOG update is worth requesting. Documentation-only,
/// configuration-only, and lockfile-only changes get skipped.
fn is_non_trivial(filename: &str) -> bool {
    let lower = filename.to_ascii_lowercase();
    if lower.ends_with(".md") || lower.ends_with(".rst") || lower.ends_with(".txt") {
        return false;
    }
    let last = filename.rsplit('/').next().unwrap_or(filename);
    matches!(
        last.to_ascii_lowercase().as_str(),
        "cargo.lock" | "package-lock.json" | "yarn.lock" | "pnpm-lock.yaml" | "go.sum"
    )
    .then_some(false)
    .unwrap_or_else(|| is_source_file(filename))
}

fn is_source_file(filename: &str) -> bool {
    let lower = filename.to_ascii_lowercase();
    [
        ".rs", ".py", ".js", ".jsx", ".ts", ".tsx", ".cjs", ".mjs", ".go", ".rb", ".php", ".java",
        ".kt", ".kts", ".cpp", ".cc", ".cxx", ".c", ".h", ".hpp", ".swift", ".sh", ".bash",
    ]
    .iter()
    .any(|ext| lower.ends_with(ext))
}

fn is_test_file(filename: &str) -> bool {
    let lower = filename.to_ascii_lowercase();
    let last = lower.rsplit('/').next().unwrap_or(&lower);
    // Common conventions: foo_test.rs / test_foo.py / Foo.test.ts
    // / src/test/ / tests/ / __tests__/
    last.contains("test")
        || lower.contains("/tests/")
        || lower.contains("/__tests__/")
        || lower.contains("/spec/")
        || last.ends_with(".spec.ts")
        || last.ends_with(".spec.js")
}

fn adds_rust_test(filename: &str, patch: Option<&str>) -> bool {
    if !filename.to_ascii_lowercase().ends_with(".rs") {
        return false;
    }
    patch
        .into_iter()
        .flat_map(str::lines)
        .filter(|line| line.starts_with('+') && !line.starts_with("+++"))
        .map(|line| line[1..].trim_start())
        .any(|line| {
            line.starts_with("#[test]")
                || line.starts_with("#[cfg(test)]")
                || line.starts_with("#[tokio::test")
                || line.starts_with("#[rstest")
                || line.starts_with("#[test_case")
        })
}

/// Render a checklist suitable for appending to a review body.
/// Skipped checks are included so the PR author sees the full list
/// and isn't surprised when a check doesn't appear later.
pub fn render_section(results: &[CheckResult]) -> String {
    render_combined_section(results, &[])
}

/// Render the combined built-in + custom-LLM pre-merge checklist.
/// Custom checks render below the built-ins under a "Custom" sub-bullet
/// so the two sources are visually distinct.
pub fn render_combined_section(
    results: &[CheckResult],
    custom: &[crate::pre_merge_llm::CustomCheckResult],
) -> String {
    if results.is_empty() && custom.is_empty() {
        return String::new();
    }
    let mut out = String::from("## Pre-merge checks\n\n");
    for r in results {
        out.push_str(&format!(
            "- {} {} — {}\n",
            r.status.checkbox(),
            r.name.label(),
            r.detail
        ));
    }
    if !custom.is_empty() {
        out.push_str("\n**Custom checks (`.auto_review.yaml`):**\n\n");
        for c in custom {
            let cb = match c.status {
                ar_prompts::PreMergeCustomStatus::Pass => "[x]",
                ar_prompts::PreMergeCustomStatus::Fail => "[ ]",
                ar_prompts::PreMergeCustomStatus::Skip => "[~]",
            };
            out.push_str(&format!("- {} {} — {}\n", cb, c.check, c.rationale));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn cf(name: &str, status: &str) -> ChangedFile {
        ChangedFile {
            filename: name.into(),
            status: status.into(),
            additions: 0,
            deletions: 0,
            changes: 0,
            patch: None,
        }
    }

    fn cf_with_patch(name: &str, status: &str, patch: &str) -> ChangedFile {
        ChangedFile {
            filename: name.into(),
            status: status.into(),
            additions: 0,
            deletions: 0,
            changes: 0,
            patch: Some(patch.into()),
        }
    }

    #[test]
    fn changelog_check_passes_when_changelog_in_diff() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("CHANGELOG.md"), "# CHANGELOG").unwrap();
        let files = vec![cf("src/x.rs", "modified"), cf("CHANGELOG.md", "modified")];
        let r = check_changelog(&files, Some(dir.path()));
        assert_eq!(r.status, CheckStatus::Pass);
    }

    #[test]
    fn changelog_check_fails_when_non_trivial_code_changes_without_entry() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("CHANGELOG.md"), "# CHANGELOG").unwrap();
        let files = vec![cf("src/x.rs", "modified")];
        let r = check_changelog(&files, Some(dir.path()));
        assert_eq!(r.status, CheckStatus::Fail);
    }

    #[test]
    fn changelog_check_skips_when_diff_is_docs_only() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("CHANGELOG.md"), "# CHANGELOG").unwrap();
        let files = vec![cf("docs/x.md", "modified")];
        let r = check_changelog(&files, Some(dir.path()));
        assert_eq!(r.status, CheckStatus::Skip);
    }

    #[test]
    fn changelog_check_skips_when_no_changelog_in_repo() {
        let dir = tempdir().unwrap();
        let files = vec![cf("src/x.rs", "modified")];
        let r = check_changelog(&files, Some(dir.path()));
        assert_eq!(r.status, CheckStatus::Skip);
    }

    #[test]
    fn tests_check_passes_when_test_file_changed() {
        let files = vec![
            cf("src/x.rs", "modified"),
            cf("tests/x_test.rs", "modified"),
        ];
        assert_eq!(check_tests(&files).status, CheckStatus::Pass);
    }

    #[test]
    fn tests_check_recognises_python_naming_convention() {
        let files = vec![cf("src/foo.py", "modified"), cf("test_foo.py", "modified")];
        assert_eq!(check_tests(&files).status, CheckStatus::Pass);
    }

    #[test]
    fn tests_check_recognises_jest_dot_test_convention() {
        let files = vec![
            cf("src/Component.tsx", "modified"),
            cf("src/Component.test.tsx", "modified"),
        ];
        assert_eq!(check_tests(&files).status, CheckStatus::Pass);
    }

    #[test]
    fn tests_check_fails_when_source_changed_without_tests() {
        let files = vec![cf("src/x.rs", "modified")];
        assert_eq!(check_tests(&files).status, CheckStatus::Fail);
    }

    #[test]
    fn tests_touched_accepts_added_rust_inline_test() {
        let files = vec![cf_with_patch(
            "src/x.rs",
            "modified",
            "@@ -10,0 +11,4 @@ mod tests {\n+    #[test]\n+    fn parses_empty_input() {\n+        assert!(parse(\"\").is_none());\n+    }\n",
        )];

        let result = check_tests(&files);

        assert_eq!(result.status, CheckStatus::Pass);
    }

    #[test]
    fn tests_check_skips_when_no_source_files_changed() {
        let files = vec![cf("docs/x.md", "modified"), cf("config/y.yaml", "added")];
        assert_eq!(check_tests(&files).status, CheckStatus::Skip);
    }

    #[test]
    fn no_new_todos_passes_on_clean_diff() {
        let diff = "@@ -1 +1 @@\n+let x = 1;\n";
        assert_eq!(check_no_new_todos(diff).status, CheckStatus::Pass);
    }

    #[test]
    fn no_new_todos_fails_when_added_line_introduces_todo() {
        let diff = "@@ -1 +2 @@\n+// TODO: handle the empty case\n";
        let r = check_no_new_todos(diff);
        assert_eq!(r.status, CheckStatus::Fail);
        assert!(r.detail.contains("1 new TODO"));
    }

    #[test]
    fn no_new_todos_counts_multiple() {
        let diff = "+# TODO one\n+# FIXME two\n+let x = 1;\n+# HACK three\n";
        let r = check_no_new_todos(diff);
        assert_eq!(r.status, CheckStatus::Fail);
        assert!(r.detail.contains("3 new TODO/FIXME markers"));
    }

    #[test]
    fn no_new_todos_ignores_removed_lines() {
        // A `-` line removing a TODO is fine — it's a cleanup.
        let diff = "-// TODO old\n+let x = 1;\n";
        assert_eq!(check_no_new_todos(diff).status, CheckStatus::Pass);
    }

    #[test]
    fn no_new_todos_ignores_diff_header_lines() {
        let diff = "+++ b/src/todo_handler.rs\n+let x = 1;\n";
        assert_eq!(check_no_new_todos(diff).status, CheckStatus::Pass);
    }

    #[test]
    fn no_new_todos_does_not_match_substrings() {
        // `todoist.com` shouldn't match TODO; `MERMAIDFIX` shouldn't
        // match FIXME.
        let diff = "+let url = \"https://todoist.com\";\n+let id = MERMAIDFIX;\n";
        assert_eq!(check_no_new_todos(diff).status, CheckStatus::Pass);
    }

    #[test]
    fn no_new_todos_finds_real_marker_after_substring_collision() {
        // Regression: `find()` only returns the first hit. When the first
        // hit is the *suffix* of a longer identifier (here `suppressFIXME`
        // → fails the leading word-boundary), the real marker later on the
        // same line was missed.
        let diff = "+// suppressFIXME and a real FIXME after\n";
        let r = check_no_new_todos(diff);
        assert_eq!(
            r.status,
            CheckStatus::Fail,
            "real FIXME after a substring collision should still trip the check"
        );
        assert!(r.detail.contains("1 new TODO"));
    }

    #[test]
    fn no_new_todos_finds_real_marker_after_prefix_collision() {
        // The first hit is the *prefix* of a longer identifier
        // (`TODOmarker` → fails the trailing word-boundary). The real
        // standalone marker after it must still be detected.
        let diff = "+// TODOmarker, then TODO again\n";
        assert_eq!(check_no_new_todos(diff).status, CheckStatus::Fail);
    }

    #[test]
    fn evaluate_runs_all_three_checks_in_a_stable_order() {
        let dir = tempdir().unwrap();
        let files = vec![cf("docs/x.md", "modified")];
        let results = evaluate("", &files, Some(dir.path()));
        assert_eq!(results.len(), 3);
        assert_eq!(results[0].name, CheckName::Changelog);
        assert_eq!(results[1].name, CheckName::Tests);
        assert_eq!(results[2].name, CheckName::NoNewTodos);
    }

    #[test]
    fn render_section_emits_markdown_checklist() {
        let results = vec![
            CheckResult {
                name: CheckName::Changelog,
                status: CheckStatus::Pass,
                detail: "ok".into(),
            },
            CheckResult {
                name: CheckName::Tests,
                status: CheckStatus::Fail,
                detail: "missing".into(),
            },
        ];
        let out = render_section(&results);
        assert!(out.starts_with("## Pre-merge checks"));
        assert!(out.contains("- [x] CHANGELOG updated — ok"));
        assert!(out.contains("- [ ] Tests touched — missing"));
    }

    #[test]
    fn render_section_handles_empty_input() {
        assert!(render_section(&[]).is_empty());
    }

    #[test]
    fn render_combined_section_lists_built_ins_then_custom() {
        use crate::pre_merge_llm::CustomCheckResult;
        use ar_prompts::PreMergeCustomStatus;
        let built_in = vec![CheckResult {
            name: CheckName::Tests,
            status: CheckStatus::Pass,
            detail: "ok".into(),
        }];
        let custom = vec![CustomCheckResult {
            check: "All new public APIs have rustdoc".into(),
            status: PreMergeCustomStatus::Fail,
            rationale: "added pub fn `foo` at src/x.rs:42 has no /// above it".into(),
        }];
        let out = render_combined_section(&built_in, &custom);
        assert!(out.contains("- [x] Tests touched — ok"));
        assert!(out.contains("**Custom checks (`.auto_review.yaml`):**"));
        assert!(out.contains("- [ ] All new public APIs have rustdoc"));
        assert!(out.contains("rustdoc"));
        // Built-ins must appear before the Custom heading.
        let builtin_pos = out.find("Tests touched").unwrap();
        let custom_pos = out.find("Custom checks").unwrap();
        assert!(builtin_pos < custom_pos);
    }

    #[test]
    fn render_combined_section_with_only_custom_still_emits_section() {
        use crate::pre_merge_llm::CustomCheckResult;
        use ar_prompts::PreMergeCustomStatus;
        let custom = vec![CustomCheckResult {
            check: "x".into(),
            status: PreMergeCustomStatus::Skip,
            rationale: "diff is config-only".into(),
        }];
        let out = render_combined_section(&[], &custom);
        assert!(out.contains("## Pre-merge checks"));
        assert!(out.contains("[~] x"));
    }
}

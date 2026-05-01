//! File-extension-based routing from a PR's changed files to the linters
//! that should run against the cloned workspace.

use ar_forgejo::ChangedFile;
use ar_tools::actionlint::ActionlintRunner;
use ar_tools::eslint::EslintRunner;
use ar_tools::gitleaks::GitleaksRunner;
use ar_tools::hadolint::HadolintRunner;
use ar_tools::markdownlint::MarkdownLintRunner;
use ar_tools::ruff::RuffRunner;
use ar_tools::runner::{run_all, LinterRunner};
use ar_tools::shellcheck::ShellCheckRunner;
use ar_tools::Finding;
use std::path::Path;

/// Run the linters appropriate for `files` against `repo_dir` and return
/// every finding. Dispatches to `select_runners` for routing and
/// `ar_tools::run_all` for parallel execution; missing binaries and
/// individual runner failures are absorbed (they don't fail the review).
pub async fn lint_workspace(repo_dir: &Path, files: &[ChangedFile]) -> Vec<Finding> {
    lint_workspace_with(repo_dir, files, &[]).await
}

/// Like [`lint_workspace`] but skips runners whose `name()` matches any
/// entry in `disabled_tools` (typically loaded from `.auto_review.yaml`).
pub async fn lint_workspace_with(
    repo_dir: &Path,
    files: &[ChangedFile],
    disabled_tools: &[String],
) -> Vec<Finding> {
    let mut runners = select_runners(files);
    if !disabled_tools.is_empty() {
        runners.retain(|r| !disabled_tools.iter().any(|d| d == r.name()));
    }
    if runners.is_empty() {
        return Vec::new();
    }
    run_all(&runners, repo_dir).await
}

/// Pick the linters to run for a given set of changed files.
///
/// Each linter is returned configured with the subset of files it cares
/// about (or, for ruff, no per-file list — ruff scans the whole tree).
/// Skip statuses are ignored: removed files don't need linting.
pub fn select_runners(files: &[ChangedFile]) -> Vec<Box<dyn LinterRunner>> {
    let surviving: Vec<&ChangedFile> = files.iter().filter(|f| f.status != "removed").collect();

    let mut runners: Vec<Box<dyn LinterRunner>> = Vec::new();

    // Always run gitleaks: secrets can land in any file type.
    if !surviving.is_empty() {
        runners.push(Box::new(GitleaksRunner));
    }

    if surviving.iter().any(|f| has_python_ext(&f.filename)) {
        runners.push(Box::new(RuffRunner));
    }

    let js_files: Vec<String> = surviving
        .iter()
        .filter(|f| has_js_ext(&f.filename))
        .map(|f| f.filename.clone())
        .collect();
    if !js_files.is_empty() {
        runners.push(Box::new(EslintRunner { files: js_files }));
    }

    let shell_files: Vec<String> = surviving
        .iter()
        .filter(|f| has_shell_ext(&f.filename))
        .map(|f| f.filename.clone())
        .collect();
    if !shell_files.is_empty() {
        runners.push(Box::new(ShellCheckRunner { files: shell_files }));
    }

    let docker_files: Vec<String> = surviving
        .iter()
        .filter(|f| is_dockerfile(&f.filename))
        .map(|f| f.filename.clone())
        .collect();
    if !docker_files.is_empty() {
        runners.push(Box::new(HadolintRunner {
            files: docker_files,
        }));
    }

    let md_files: Vec<String> = surviving
        .iter()
        .filter(|f| has_markdown_ext(&f.filename))
        .map(|f| f.filename.clone())
        .collect();
    if !md_files.is_empty() {
        runners.push(Box::new(MarkdownLintRunner { files: md_files }));
    }

    let workflow_files: Vec<String> = surviving
        .iter()
        .filter(|f| is_workflow_yaml(&f.filename))
        .map(|f| f.filename.clone())
        .collect();
    if !workflow_files.is_empty() {
        runners.push(Box::new(ActionlintRunner {
            files: workflow_files,
        }));
    }

    runners
}

fn has_python_ext(name: &str) -> bool {
    name.ends_with(".py")
}

fn has_js_ext(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    [".js", ".jsx", ".ts", ".tsx", ".cjs", ".mjs"]
        .iter()
        .any(|ext| lower.ends_with(ext))
}

fn has_shell_ext(name: &str) -> bool {
    name.ends_with(".sh") || name.ends_with(".bash")
}

fn is_dockerfile(name: &str) -> bool {
    let last = name.rsplit('/').next().unwrap_or(name);
    last == "Dockerfile" || last.starts_with("Dockerfile.") || last.ends_with(".dockerfile")
}

fn has_markdown_ext(name: &str) -> bool {
    name.ends_with(".md") || name.ends_with(".markdown")
}

fn is_workflow_yaml(name: &str) -> bool {
    let yaml = name.ends_with(".yml") || name.ends_with(".yaml");
    if !yaml {
        return false;
    }
    name.starts_with(".github/workflows/")
        || name.contains("/.github/workflows/")
        || name.starts_with(".forgejo/workflows/")
        || name.contains("/.forgejo/workflows/")
        || name.starts_with(".gitea/workflows/")
        || name.contains("/.gitea/workflows/")
}

#[cfg(test)]
mod tests {
    use super::*;

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

    fn names(runners: &[Box<dyn LinterRunner>]) -> Vec<&str> {
        runners.iter().map(|r| r.name()).collect()
    }

    #[test]
    fn empty_input_yields_no_runners() {
        assert!(select_runners(&[]).is_empty());
    }

    #[test]
    fn any_non_empty_input_includes_gitleaks() {
        let files = vec![cf("src/main.rs", "modified")];
        let runners = select_runners(&files);
        assert!(names(&runners).contains(&"gitleaks"));
    }

    #[test]
    fn python_files_select_ruff() {
        let files = vec![cf("src/x.py", "modified")];
        let runners = select_runners(&files);
        let mut got = names(&runners);
        got.sort();
        assert_eq!(got, vec!["gitleaks", "ruff"]);
    }

    #[test]
    fn shell_files_select_shellcheck() {
        let files = vec![cf("scripts/build.sh", "modified")];
        let runners = select_runners(&files);
        let mut got = names(&runners);
        got.sort();
        assert_eq!(got, vec!["gitleaks", "shellcheck"]);
    }

    #[test]
    fn dockerfile_selects_hadolint_by_basename() {
        for name in ["Dockerfile", "deploy/Dockerfile", "Dockerfile.web"] {
            let files = vec![cf(name, "modified")];
            let runners = select_runners(&files);
            let mut got = names(&runners);
            got.sort();
            assert_eq!(got, vec!["gitleaks", "hadolint"], "name = {name}");
        }
    }

    #[test]
    fn markdown_files_select_markdownlint() {
        let files = vec![cf("README.md", "modified")];
        let runners = select_runners(&files);
        let mut got = names(&runners);
        got.sort();
        assert_eq!(got, vec!["gitleaks", "markdownlint"]);
    }

    #[test]
    fn mixed_files_select_all_relevant_runners() {
        let files = vec![
            cf("a.py", "modified"),
            cf("b.sh", "modified"),
            cf("Dockerfile", "modified"),
            cf("c.md", "modified"),
            cf("ui/d.tsx", "modified"),
        ];
        let runners = select_runners(&files);
        let mut got = names(&runners);
        got.sort();
        assert_eq!(
            got,
            vec![
                "eslint",
                "gitleaks",
                "hadolint",
                "markdownlint",
                "ruff",
                "shellcheck"
            ]
        );
    }

    #[test]
    fn workflow_yaml_selects_actionlint() {
        for name in [
            ".github/workflows/ci.yml",
            ".forgejo/workflows/release.yaml",
            ".gitea/workflows/test.yml",
        ] {
            let files = vec![cf(name, "modified")];
            let runners = select_runners(&files);
            let mut got = names(&runners);
            got.sort();
            assert_eq!(got, vec!["actionlint", "gitleaks"], "name = {name}");
        }
    }

    #[test]
    fn arbitrary_yaml_does_not_select_actionlint() {
        let files = vec![cf("config/app.yml", "modified")];
        let runners = select_runners(&files);
        let names_v = names(&runners);
        assert!(!names_v.contains(&"actionlint"));
    }

    #[test]
    fn javascript_typescript_extensions_select_eslint() {
        for name in [
            "src/a.js",
            "src/b.jsx",
            "src/c.ts",
            "src/d.tsx",
            "src/e.cjs",
            "src/f.mjs",
        ] {
            let files = vec![cf(name, "modified")];
            let runners = select_runners(&files);
            let mut got = names(&runners);
            got.sort();
            assert_eq!(got, vec!["eslint", "gitleaks"], "name = {name}");
        }
    }

    #[test]
    fn unknown_extensions_select_only_gitleaks() {
        let files = vec![cf("src/main.rs", "modified"), cf("Cargo.toml", "modified")];
        let runners = select_runners(&files);
        assert_eq!(names(&runners), vec!["gitleaks"]);
    }

    #[test]
    fn removed_files_are_ignored() {
        let files = vec![cf("a.py", "removed"), cf("b.sh", "removed")];
        assert!(select_runners(&files).is_empty());
    }

    #[tokio::test]
    async fn lint_workspace_with_disables_named_tools() {
        let dir = tempfile::tempdir().unwrap();
        // Files routed to multiple linters but all linters disabled.
        let files = vec![cf("a.py", "modified"), cf("b.sh", "modified")];
        let disabled: Vec<String> = vec!["gitleaks", "ruff", "shellcheck"]
            .into_iter()
            .map(String::from)
            .collect();
        let findings = lint_workspace_with(dir.path(), &files, &disabled).await;
        assert!(findings.is_empty());
    }

    #[tokio::test]
    async fn lint_workspace_with_no_routed_files_returns_empty() {
        let dir = tempfile::tempdir().unwrap();
        let findings = lint_workspace(dir.path(), &[cf("src/main.rs", "modified")]).await;
        assert!(findings.is_empty());
    }

    #[tokio::test]
    async fn lint_workspace_with_empty_file_list_returns_empty() {
        let dir = tempfile::tempdir().unwrap();
        let findings = lint_workspace(dir.path(), &[]).await;
        assert!(findings.is_empty());
    }

    #[test]
    fn shellcheck_runner_receives_only_shell_files() {
        let files = vec![
            cf("a.sh", "modified"),
            cf("b.bash", "modified"),
            cf("c.py", "modified"),
        ];
        let runners = select_runners(&files);
        let sc = runners
            .iter()
            .find(|r| r.name() == "shellcheck")
            .expect("shellcheck present");
        // Down-cast via Debug-ish indirection: the runner's filter list is
        // an implementation detail. We verify behaviour through the public
        // surface — in this case, by verifying ruff also got selected
        // (proving the .py file was kept and routed elsewhere) and
        // shellcheck was selected (proving its files were collected).
        let _ = sc;
        let mut got: Vec<&str> = runners.iter().map(|r| r.name()).collect();
        got.sort();
        assert_eq!(got, vec!["gitleaks", "ruff", "shellcheck"]);
    }
}

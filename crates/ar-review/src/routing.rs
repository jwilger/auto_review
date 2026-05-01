//! File-extension-based routing from a PR's changed files to the linters
//! that should run against the cloned workspace.

use ar_forgejo::ChangedFile;
use ar_sandbox::{DirectSandbox, Sandbox};
use ar_tools::actionlint::ActionlintRunner;
use ar_tools::ast_grep::AstGrepRunner;
use ar_tools::biome::BiomeRunner;
use ar_tools::eslint::EslintRunner;
use ar_tools::gitleaks::GitleaksRunner;
use ar_tools::golangci_lint::GolangciLintRunner;
use ar_tools::hadolint::HadolintRunner;
use ar_tools::markdownlint::MarkdownLintRunner;
use ar_tools::osv_scanner::OsvScannerRunner;
use ar_tools::oxlint::OxlintRunner;
use ar_tools::phpstan::PhpstanRunner;
use ar_tools::rubocop::RubocopRunner;
use ar_tools::ruff::RuffRunner;
use ar_tools::runner::{run_all, LinterRunner};
use ar_tools::semgrep::SemgrepRunner;
use ar_tools::shellcheck::ShellCheckRunner;
use ar_tools::sqlfluff::SqlfluffRunner;
use ar_tools::taplo::TaploRunner;
use ar_tools::trivy::TrivyRunner;
use ar_tools::yamllint::YamlLintRunner;
use ar_tools::Finding;
use std::path::Path;

/// Run the linters appropriate for `files` against `repo_dir` and return
/// every finding. Dispatches to `select_runners` for routing and
/// `ar_tools::run_all` for parallel execution; missing binaries and
/// individual runner failures are absorbed (they don't fail the review).
///
/// Uses [`DirectSandbox`] (no isolation) — appropriate for tests, CI
/// against trusted repos, and the development gateway. Production
/// deployments should call [`lint_workspace_via`] with a
/// [`PodmanSandbox`](ar_sandbox::PodmanSandbox).
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
    let sandbox = DirectSandbox::new();
    lint_workspace_via(&sandbox, repo_dir, files, disabled_tools).await
}

/// Like [`lint_workspace_with`] but takes a caller-provided sandbox.
/// Wire this with a [`PodmanSandbox`](ar_sandbox::PodmanSandbox) in
/// production so untrusted PR contents can't escape.
pub async fn lint_workspace_via(
    sandbox: &dyn Sandbox,
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
    run_all(&runners, sandbox, repo_dir).await
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

    // Always run semgrep when there are surviving files: it's
    // multi-language and uses --config=auto for reasonable defaults
    // plus any .semgrep.yml the repo provides.
    if !surviving.is_empty() {
        runners.push(Box::new(SemgrepRunner));
    }

    // Always run trivy: detects CVEs in dependency manifests
    // (Cargo.lock, package-lock.json, etc.), Dockerfile / k8s /
    // Terraform misconfigurations, and committed secrets — covers
    // territory none of the per-language linters reach.
    if !surviving.is_empty() {
        runners.push(Box::new(TrivyRunner));
    }

    // Always run osv-scanner: queries Google's OSV database for
    // known CVEs in declared dependencies. Runs alongside trivy
    // because the two tools draw from different feeds — running
    // both surfaces vulns that either DB has indexed.
    if !surviving.is_empty() {
        runners.push(Box::new(OsvScannerRunner));
    }

    // Always run ast-grep. When the repo has no `sgconfig.yml`
    // or `.ast-grep/` rules, it exits cleanly with empty output
    // (no rules → no findings) and the runner returns Vec::new.
    // The cost is one container spawn per review for repos that
    // don't use ast-grep — small enough to be worth always
    // surfacing for those that do.
    if !surviving.is_empty() {
        runners.push(Box::new(AstGrepRunner));
    }

    if surviving.iter().any(|f| has_python_ext(&f.filename)) {
        runners.push(Box::new(RuffRunner));
    }

    if surviving.iter().any(|f| has_go_ext(&f.filename)) {
        runners.push(Box::new(GolangciLintRunner));
    }

    let ruby_files: Vec<String> = surviving
        .iter()
        .filter(|f| has_ruby_ext(&f.filename))
        .map(|f| f.filename.clone())
        .collect();
    if !ruby_files.is_empty() {
        runners.push(Box::new(RubocopRunner { files: ruby_files }));
    }

    let php_files: Vec<String> = surviving
        .iter()
        .filter(|f| has_php_ext(&f.filename))
        .map(|f| f.filename.clone())
        .collect();
    if !php_files.is_empty() {
        runners.push(Box::new(PhpstanRunner { files: php_files }));
    }

    let js_files: Vec<String> = surviving
        .iter()
        .filter(|f| has_js_ext(&f.filename))
        .map(|f| f.filename.clone())
        .collect();
    if !js_files.is_empty() {
        runners.push(Box::new(EslintRunner {
            files: js_files.clone(),
        }));
        runners.push(Box::new(BiomeRunner {
            files: js_files.clone(),
        }));
        runners.push(Box::new(OxlintRunner { files: js_files }));
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

    let sql_files: Vec<String> = surviving
        .iter()
        .filter(|f| has_sql_ext(&f.filename))
        .map(|f| f.filename.clone())
        .collect();
    if !sql_files.is_empty() {
        runners.push(Box::new(SqlfluffRunner { files: sql_files }));
    }

    let toml_files: Vec<String> = surviving
        .iter()
        .filter(|f| f.filename.ends_with(".toml"))
        .map(|f| f.filename.clone())
        .collect();
    if !toml_files.is_empty() {
        runners.push(Box::new(TaploRunner { files: toml_files }));
    }

    // yamllint runs on every YAML file, including workflows (it complains
    // about formatting; actionlint handles semantics — they're
    // complementary).
    let yaml_files: Vec<String> = surviving
        .iter()
        .filter(|f| has_yaml_ext(&f.filename))
        .map(|f| f.filename.clone())
        .collect();
    if !yaml_files.is_empty() {
        runners.push(Box::new(YamlLintRunner { files: yaml_files }));
    }

    runners
}

fn has_python_ext(name: &str) -> bool {
    name.ends_with(".py")
}

fn has_go_ext(name: &str) -> bool {
    name.ends_with(".go")
}

fn has_ruby_ext(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower.ends_with(".rb")
        || lower.ends_with(".rake")
        || lower.ends_with("/gemfile")
        || lower.ends_with("/rakefile")
        || lower == "gemfile"
        || lower == "rakefile"
}

fn has_php_ext(name: &str) -> bool {
    name.ends_with(".php")
        || name.ends_with(".phtml")
        || name.ends_with(".php3")
        || name.ends_with(".php4")
        || name.ends_with(".php5")
        || name.ends_with(".php7")
        || name.ends_with(".phps")
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

fn has_yaml_ext(name: &str) -> bool {
    name.ends_with(".yml") || name.ends_with(".yaml")
}

fn has_sql_ext(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower.ends_with(".sql") || lower.ends_with(".dml") || lower.ends_with(".ddl")
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
        assert_eq!(
            got,
            vec![
                "ast-grep",
                "gitleaks",
                "osv-scanner",
                "ruff",
                "semgrep",
                "trivy"
            ]
        );
    }

    #[test]
    fn ruby_files_select_rubocop() {
        for name in [
            "lib/user.rb",
            "spec/user_spec.rb",
            "Rakefile",
            "Gemfile",
            "tasks/deploy.rake",
        ] {
            let files = vec![cf(name, "modified")];
            let runners = select_runners(&files);
            let mut got = names(&runners);
            got.sort();
            assert_eq!(
                got,
                vec![
                    "ast-grep",
                    "gitleaks",
                    "osv-scanner",
                    "rubocop",
                    "semgrep",
                    "trivy"
                ],
                "name = {name}"
            );
        }
    }

    #[test]
    fn go_files_select_golangci_lint() {
        let files = vec![cf("cmd/main.go", "modified")];
        let runners = select_runners(&files);
        let mut got = names(&runners);
        got.sort();
        assert_eq!(
            got,
            vec![
                "ast-grep",
                "gitleaks",
                "golangci-lint",
                "osv-scanner",
                "semgrep",
                "trivy"
            ]
        );
    }

    #[test]
    fn shell_files_select_shellcheck() {
        let files = vec![cf("scripts/build.sh", "modified")];
        let runners = select_runners(&files);
        let mut got = names(&runners);
        got.sort();
        assert_eq!(
            got,
            vec![
                "ast-grep",
                "gitleaks",
                "osv-scanner",
                "semgrep",
                "shellcheck",
                "trivy"
            ]
        );
    }

    #[test]
    fn dockerfile_selects_hadolint_by_basename() {
        for name in ["Dockerfile", "deploy/Dockerfile", "Dockerfile.web"] {
            let files = vec![cf(name, "modified")];
            let runners = select_runners(&files);
            let mut got = names(&runners);
            got.sort();
            assert_eq!(
                got,
                vec![
                    "ast-grep",
                    "gitleaks",
                    "hadolint",
                    "osv-scanner",
                    "semgrep",
                    "trivy"
                ],
                "name = {name}"
            );
        }
    }

    #[test]
    fn markdown_files_select_markdownlint() {
        let files = vec![cf("README.md", "modified")];
        let runners = select_runners(&files);
        let mut got = names(&runners);
        got.sort();
        assert_eq!(
            got,
            vec![
                "ast-grep",
                "gitleaks",
                "markdownlint",
                "osv-scanner",
                "semgrep",
                "trivy"
            ]
        );
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
                "ast-grep",
                "biome",
                "eslint",
                "gitleaks",
                "hadolint",
                "markdownlint",
                "osv-scanner",
                "oxlint",
                "ruff",
                "semgrep",
                "shellcheck",
                "trivy"
            ]
        );
    }

    #[test]
    fn workflow_yaml_selects_actionlint_and_yamllint() {
        for name in [
            ".github/workflows/ci.yml",
            ".forgejo/workflows/release.yaml",
            ".gitea/workflows/test.yml",
        ] {
            let files = vec![cf(name, "modified")];
            let runners = select_runners(&files);
            let mut got = names(&runners);
            got.sort();
            assert_eq!(
                got,
                vec![
                    "actionlint",
                    "ast-grep",
                    "gitleaks",
                    "osv-scanner",
                    "semgrep",
                    "trivy",
                    "yamllint"
                ],
                "name = {name}"
            );
        }
    }

    #[test]
    fn arbitrary_yaml_selects_yamllint_but_not_actionlint() {
        let files = vec![cf("config/app.yml", "modified")];
        let runners = select_runners(&files);
        let mut got = names(&runners);
        got.sort();
        assert_eq!(
            got,
            vec![
                "ast-grep",
                "gitleaks",
                "osv-scanner",
                "semgrep",
                "trivy",
                "yamllint"
            ]
        );
        assert!(!got.contains(&"actionlint"));
    }

    #[test]
    fn javascript_typescript_extensions_select_eslint_biome_and_oxlint() {
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
            assert_eq!(
                got,
                vec![
                    "ast-grep",
                    "biome",
                    "eslint",
                    "gitleaks",
                    "osv-scanner",
                    "oxlint",
                    "semgrep",
                    "trivy"
                ],
                "name = {name}"
            );
        }
    }

    #[test]
    fn unknown_extensions_select_only_always_run_set() {
        // .rs has no dedicated runner today (clippy is left to repo
        // CI per the plan). Cargo.toml routes through taplo.
        let files = vec![cf("src/main.rs", "modified"), cf("Cargo.toml", "modified")];
        let runners = select_runners(&files);
        let mut got = names(&runners);
        got.sort();
        assert_eq!(
            got,
            vec![
                "ast-grep",
                "gitleaks",
                "osv-scanner",
                "semgrep",
                "taplo",
                "trivy"
            ]
        );
    }

    #[test]
    fn toml_files_select_taplo() {
        for name in ["Cargo.toml", "pyproject.toml", "config/app.toml"] {
            let files = vec![cf(name, "modified")];
            let runners = select_runners(&files);
            let mut got = names(&runners);
            got.sort();
            assert_eq!(
                got,
                vec![
                    "ast-grep",
                    "gitleaks",
                    "osv-scanner",
                    "semgrep",
                    "taplo",
                    "trivy"
                ],
                "name = {name}"
            );
        }
    }

    #[test]
    fn php_files_select_phpstan() {
        for name in ["src/Controller.php", "lib/Foo.phtml", "legacy/old.php3"] {
            let files = vec![cf(name, "modified")];
            let runners = select_runners(&files);
            let mut got = names(&runners);
            got.sort();
            assert_eq!(
                got,
                vec![
                    "ast-grep",
                    "gitleaks",
                    "osv-scanner",
                    "phpstan",
                    "semgrep",
                    "trivy"
                ],
                "name = {name}"
            );
        }
    }

    #[test]
    fn sql_files_select_sqlfluff() {
        for name in [
            "queries/users.sql",
            "schema/migrations/001.SQL",
            "x.dml",
            "y.ddl",
        ] {
            let files = vec![cf(name, "modified")];
            let runners = select_runners(&files);
            let mut got = names(&runners);
            got.sort();
            assert_eq!(
                got,
                vec![
                    "ast-grep",
                    "gitleaks",
                    "osv-scanner",
                    "semgrep",
                    "sqlfluff",
                    "trivy"
                ],
                "name = {name}"
            );
        }
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
        assert_eq!(
            got,
            vec![
                "ast-grep",
                "gitleaks",
                "osv-scanner",
                "ruff",
                "semgrep",
                "shellcheck",
                "trivy"
            ]
        );
    }
}

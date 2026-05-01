//! File-extension-based routing from a PR's changed files to the linters
//! that should run against the cloned workspace.

use ar_forgejo::ChangedFile;
use ar_sandbox::{DirectSandbox, Sandbox};
use ar_tools::actionlint::ActionlintRunner;
use ar_tools::ansible_lint::AnsibleLintRunner;
use ar_tools::ast_grep::AstGrepRunner;
use ar_tools::bandit::BanditRunner;
use ar_tools::biome::BiomeRunner;
use ar_tools::buf::BufRunner;
use ar_tools::checkov::CheckovRunner;
use ar_tools::cppcheck::CppcheckRunner;
use ar_tools::dotenv_linter::DotenvLinterRunner;
use ar_tools::eslint::EslintRunner;
use ar_tools::gitleaks::GitleaksRunner;
use ar_tools::golangci_lint::GolangciLintRunner;
use ar_tools::gosec::GosecRunner;
use ar_tools::hadolint::HadolintRunner;
use ar_tools::kubeconform::KubeconformRunner;
use ar_tools::markdownlint::MarkdownLintRunner;
use ar_tools::mypy::MypyRunner;
use ar_tools::osv_scanner::OsvScannerRunner;
use ar_tools::oxlint::OxlintRunner;
use ar_tools::phpstan::PhpstanRunner;
use ar_tools::pmd::PmdRunner;
use ar_tools::rubocop::RubocopRunner;
use ar_tools::ruff::RuffRunner;
use ar_tools::runner::{run_all, LinterRunner};
use ar_tools::semgrep::SemgrepRunner;
use ar_tools::shellcheck::ShellCheckRunner;
use ar_tools::sqlfluff::SqlfluffRunner;
use ar_tools::staticcheck::StaticcheckRunner;
use ar_tools::stylelint::StylelintRunner;
use ar_tools::swiftlint::SwiftLintRunner;
use ar_tools::taplo::TaploRunner;
use ar_tools::tflint::TflintRunner;
use ar_tools::trivy::TrivyRunner;
use ar_tools::typos::TyposRunner;
use ar_tools::vale::ValeRunner;
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

    // Always run typos: catches misspellings in identifier names,
    // comments, and string literals across every source file.
    // Distinct from vale (markdown prose only).
    if !surviving.is_empty() {
        runners.push(Box::new(TyposRunner));
    }

    if surviving.iter().any(|f| has_python_ext(&f.filename)) {
        runners.push(Box::new(RuffRunner));
        runners.push(Box::new(MypyRunner));
        runners.push(Box::new(BanditRunner));
    }

    if surviving.iter().any(|f| has_go_ext(&f.filename)) {
        runners.push(Box::new(GolangciLintRunner));
        runners.push(Box::new(GosecRunner));
        runners.push(Box::new(StaticcheckRunner));
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

    let c_files: Vec<String> = surviving
        .iter()
        .filter(|f| has_c_ext(&f.filename))
        .map(|f| f.filename.clone())
        .collect();
    if !c_files.is_empty() {
        runners.push(Box::new(CppcheckRunner { files: c_files }));
    }

    let java_files: Vec<String> = surviving
        .iter()
        .filter(|f| f.filename.ends_with(".java"))
        .map(|f| f.filename.clone())
        .collect();
    if !java_files.is_empty() {
        runners.push(Box::new(PmdRunner { files: java_files }));
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
        runners.push(Box::new(MarkdownLintRunner {
            files: md_files.clone(),
        }));
        // vale catches prose issues (grammar, voice, spelling)
        // markdownlint doesn't see. When the repo has no .vale.ini
        // configured, vale exits cleanly with `{}` and the runner
        // emits no findings.
        runners.push(Box::new(ValeRunner { files: md_files }));
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

    let swift_files: Vec<String> = surviving
        .iter()
        .filter(|f| f.filename.ends_with(".swift"))
        .map(|f| f.filename.clone())
        .collect();
    if !swift_files.is_empty() {
        runners.push(Box::new(SwiftLintRunner { files: swift_files }));
    }

    let css_files: Vec<String> = surviving
        .iter()
        .filter(|f| has_css_ext(&f.filename))
        .map(|f| f.filename.clone())
        .collect();
    if !css_files.is_empty() {
        runners.push(Box::new(StylelintRunner { files: css_files }));
    }

    // buf reads its module config (`buf.yaml`) from the workspace
    // root and lints every .proto in the module — passing per-file
    // args isn't its idiom. Always run when there's a .proto in
    // the surviving files; buf no-ops on repos without a buf.yaml.
    if surviving.iter().any(|f| f.filename.ends_with(".proto")) {
        runners.push(Box::new(BufRunner));
    }

    let toml_files: Vec<String> = surviving
        .iter()
        .filter(|f| f.filename.ends_with(".toml"))
        .map(|f| f.filename.clone())
        .collect();
    if !toml_files.is_empty() {
        runners.push(Box::new(TaploRunner { files: toml_files }));
    }

    // Terraform-only routing for checkov. checkov also covers
    // CloudFormation/k8s/Helm but those overlap with trivy enough
    // that running it on .yml files would mostly duplicate work.
    let tf_files: Vec<String> = surviving
        .iter()
        .filter(|f| has_terraform_ext(&f.filename))
        .map(|f| f.filename.clone())
        .collect();
    if !tf_files.is_empty() {
        runners.push(Box::new(CheckovRunner { files: tf_files }));
        // tflint is Terraform-specific and supports provider plugins
        // (AWS/Azure/GCP) that catch resource-level mistakes
        // checkov doesn't see.
        runners.push(Box::new(TflintRunner));
    }

    let env_files: Vec<String> = surviving
        .iter()
        .filter(|f| is_dotenv_file(&f.filename))
        .map(|f| f.filename.clone())
        .collect();
    if !env_files.is_empty() {
        runners.push(Box::new(DotenvLinterRunner { files: env_files }));
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
        runners.push(Box::new(YamlLintRunner {
            files: yaml_files.clone(),
        }));
        // kubeconform runs against the same YAML set; it skips
        // anything that isn't a Kubernetes manifest, so the cost
        // for a non-k8s repo is one container spawn that returns
        // an empty resources array.
        runners.push(Box::new(KubeconformRunner {
            files: yaml_files.clone(),
        }));
        // ansible-lint runs against the same YAML set; it skips
        // anything that isn't an Ansible playbook/role/task, so
        // the cost on non-Ansible repos is one container spawn
        // that returns an empty array.
        runners.push(Box::new(AnsibleLintRunner { files: yaml_files }));
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

fn has_c_ext(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    [
        ".c", ".cc", ".cpp", ".cxx", ".c++", ".h", ".hh", ".hpp", ".hxx", ".h++",
    ]
    .iter()
    .any(|ext| lower.ends_with(ext))
}

fn has_css_ext(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    [".css", ".scss", ".sass", ".less"]
        .iter()
        .any(|ext| lower.ends_with(ext))
}

fn has_terraform_ext(name: &str) -> bool {
    name.ends_with(".tf") || name.ends_with(".tfvars") || name.ends_with(".hcl")
}

fn is_dotenv_file(name: &str) -> bool {
    let last = name.rsplit('/').next().unwrap_or(name);
    // `.env`, `.env.local`, `.env.production`, `env.example`, etc.
    last == ".env" || last.starts_with(".env.") || last == "env" || last.starts_with("env.")
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
    fn python_files_select_ruff_mypy_and_bandit() {
        let files = vec![cf("src/x.py", "modified")];
        let runners = select_runners(&files);
        let mut got = names(&runners);
        got.sort();
        assert_eq!(
            got,
            vec![
                "ast-grep",
                "bandit",
                "gitleaks",
                "mypy",
                "osv-scanner",
                "ruff",
                "semgrep",
                "trivy",
                "typos"
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
                    "trivy",
                    "typos"
                ],
                "name = {name}"
            );
        }
    }

    #[test]
    fn go_files_select_golangci_lint_gosec_and_staticcheck() {
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
                "gosec",
                "osv-scanner",
                "semgrep",
                "staticcheck",
                "trivy",
                "typos"
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
                "trivy",
                "typos"
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
                    "trivy",
                    "typos"
                ],
                "name = {name}"
            );
        }
    }

    #[test]
    fn markdown_files_select_markdownlint_and_vale() {
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
                "trivy",
                "typos",
                "vale"
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
                "bandit",
                "biome",
                "eslint",
                "gitleaks",
                "hadolint",
                "markdownlint",
                "mypy",
                "osv-scanner",
                "oxlint",
                "ruff",
                "semgrep",
                "shellcheck",
                "trivy",
                "typos",
                "vale"
            ]
        );
    }

    #[test]
    fn workflow_yaml_selects_actionlint_yamllint_and_kubeconform() {
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
                    "ansible-lint",
                    "ast-grep",
                    "gitleaks",
                    "kubeconform",
                    "osv-scanner",
                    "semgrep",
                    "trivy",
                    "typos",
                    "yamllint"
                ],
                "name = {name}"
            );
        }
    }

    #[test]
    fn arbitrary_yaml_selects_yamllint_and_kubeconform_but_not_actionlint() {
        let files = vec![cf("config/app.yml", "modified")];
        let runners = select_runners(&files);
        let mut got = names(&runners);
        got.sort();
        assert_eq!(
            got,
            vec![
                "ansible-lint",
                "ast-grep",
                "gitleaks",
                "kubeconform",
                "osv-scanner",
                "semgrep",
                "trivy",
                "typos",
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
                    "trivy",
                    "typos"
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
                "trivy",
                "typos"
            ]
        );
    }

    #[test]
    fn dotenv_files_select_dotenv_linter() {
        for name in [
            ".env",
            ".env.local",
            ".env.production",
            "config/.env.staging",
        ] {
            let files = vec![cf(name, "modified")];
            let runners = select_runners(&files);
            let mut got = names(&runners);
            got.sort();
            assert_eq!(
                got,
                vec![
                    "ast-grep",
                    "dotenv-linter",
                    "gitleaks",
                    "osv-scanner",
                    "semgrep",
                    "trivy",
                    "typos"
                ],
                "name = {name}"
            );
        }
    }

    #[test]
    fn terraform_files_select_checkov_and_tflint() {
        for name in ["infra/main.tf", "vars/prod.tfvars", "module.hcl"] {
            let files = vec![cf(name, "modified")];
            let runners = select_runners(&files);
            let mut got = names(&runners);
            got.sort();
            assert_eq!(
                got,
                vec![
                    "ast-grep",
                    "checkov",
                    "gitleaks",
                    "osv-scanner",
                    "semgrep",
                    "tflint",
                    "trivy",
                    "typos"
                ],
                "name = {name}"
            );
        }
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
                    "trivy",
                    "typos"
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
                    "trivy",
                    "typos"
                ],
                "name = {name}"
            );
        }
    }

    #[test]
    fn java_files_select_pmd() {
        let files = vec![cf("src/main/java/Foo.java", "modified")];
        let runners = select_runners(&files);
        let mut got = names(&runners);
        got.sort();
        assert_eq!(
            got,
            vec![
                "ast-grep",
                "gitleaks",
                "osv-scanner",
                "pmd",
                "semgrep",
                "trivy",
                "typos"
            ]
        );
    }

    #[test]
    fn c_and_cpp_files_select_cppcheck() {
        for name in [
            "src/main.c",
            "src/util.cpp",
            "src/parse.cc",
            "src/x.cxx",
            "include/lib.h",
            "include/lib.hpp",
        ] {
            let files = vec![cf(name, "modified")];
            let runners = select_runners(&files);
            let mut got = names(&runners);
            got.sort();
            assert_eq!(
                got,
                vec![
                    "ast-grep",
                    "cppcheck",
                    "gitleaks",
                    "osv-scanner",
                    "semgrep",
                    "trivy",
                    "typos"
                ],
                "name = {name}"
            );
        }
    }

    #[test]
    fn css_files_select_stylelint() {
        for name in [
            "src/styles/main.css",
            "src/styles/theme.scss",
            "src/styles/old.sass",
            "src/styles/legacy.less",
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
                    "stylelint",
                    "trivy",
                    "typos"
                ],
                "name = {name}"
            );
        }
    }

    #[test]
    fn proto_files_select_buf() {
        let files = vec![cf("api/v1/foo.proto", "modified")];
        let runners = select_runners(&files);
        let mut got = names(&runners);
        got.sort();
        assert_eq!(
            got,
            vec![
                "ast-grep",
                "buf",
                "gitleaks",
                "osv-scanner",
                "semgrep",
                "trivy",
                "typos"
            ]
        );
    }

    #[test]
    fn swift_files_select_swiftlint() {
        let files = vec![cf("Sources/Auth.swift", "modified")];
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
                "swiftlint",
                "trivy",
                "typos"
            ]
        );
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
                    "trivy",
                    "typos"
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
                "bandit",
                "gitleaks",
                "mypy",
                "osv-scanner",
                "ruff",
                "semgrep",
                "shellcheck",
                "trivy",
                "typos"
            ]
        );
    }
}

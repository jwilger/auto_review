//! Legacy inventory of formerly bundled linters.
//!
//! The `name` field is the string each runner returns from
//! `LinterRunner::name()`. Normal review runtime no longer exposes this
//! catalogue or routes files to these tools; CI owns deterministic linters.

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub struct LinterInfo {
    pub name: &'static str,
    pub description: &'static str,
    pub languages: &'static [&'static str],
    pub homepage: &'static str,
}

/// Sorted alphabetically by `name` so output is stable.
pub fn linter_catalogue() -> &'static [LinterInfo] {
    CATALOGUE
}

const CATALOGUE: &[LinterInfo] = &[
    LinterInfo {
        name: "actionlint",
        description: "Linter for GitHub / Forgejo Actions workflow YAML.",
        languages: &["ci", "yaml"],
        homepage: "https://github.com/rhysd/actionlint",
    },
    LinterInfo {
        name: "ansible-lint",
        description: "Best-practice checker for Ansible playbooks and roles.",
        languages: &["ansible", "yaml"],
        homepage: "https://github.com/ansible/ansible-lint",
    },
    LinterInfo {
        name: "ast-grep",
        description: "Structural search/lint via tree-sitter AST patterns. Reads `sgconfig.yml` / `.ast-grep/` rules; no-op on repos without rules.",
        languages: &["multi"],
        homepage: "https://ast-grep.github.io/",
    },
    LinterInfo {
        name: "bandit",
        description: "Security-focused static analyzer for Python.",
        languages: &["python", "security"],
        homepage: "https://github.com/PyCQA/bandit",
    },
    LinterInfo {
        name: "biome",
        description: "Fast Rust-based formatter + linter for JS/TS/JSX/TSX. Complementary to ESLint for semantic checks.",
        languages: &["javascript", "typescript"],
        homepage: "https://biomejs.dev/",
    },
    LinterInfo {
        name: "buf",
        description: "Protocol Buffers linter. Reads `buf.yaml` from the repo root; no-op without one.",
        languages: &["proto"],
        homepage: "https://buf.build/",
    },
    LinterInfo {
        name: "checkov",
        description: "Static analyzer for Terraform IaC misconfigurations.",
        languages: &["terraform", "iac", "security"],
        homepage: "https://www.checkov.io/",
    },
    LinterInfo {
        name: "cppcheck",
        description: "Static analyzer for C and C++ catching memory and logic bugs.",
        languages: &["c", "cpp"],
        homepage: "https://cppcheck.sourceforge.io/",
    },
    LinterInfo {
        name: "dotenv-linter",
        description: "Lints `.env` files for duplicate keys, ordering, and quoting issues.",
        languages: &["dotenv"],
        homepage: "https://github.com/dotenv-linter/dotenv-linter",
    },
    LinterInfo {
        name: "eslint",
        description: "Pluggable JS/TS linter with project-config awareness.",
        languages: &["javascript", "typescript"],
        homepage: "https://eslint.org/",
    },
    LinterInfo {
        name: "gitleaks",
        description: "Detects committed secrets across all file types. Always runs.",
        languages: &["secrets", "security"],
        homepage: "https://github.com/gitleaks/gitleaks",
    },
    LinterInfo {
        name: "golangci-lint",
        description: "Go meta-linter aggregating ~50 individual analyzers.",
        languages: &["go"],
        homepage: "https://golangci-lint.run/",
    },
    LinterInfo {
        name: "gosec",
        description: "Security-focused static analyzer for Go.",
        languages: &["go", "security"],
        homepage: "https://github.com/securego/gosec",
    },
    LinterInfo {
        name: "hadolint",
        description: "Dockerfile linter checking ShellCheck-rule compliance and best practices.",
        languages: &["docker"],
        homepage: "https://github.com/hadolint/hadolint",
    },
    LinterInfo {
        name: "helm",
        description: "Helm chart linter (`helm lint`). Runs against any directory containing a Chart.yaml.",
        languages: &["helm", "kubernetes"],
        homepage: "https://helm.sh/",
    },
    LinterInfo {
        name: "htmlhint",
        description: "Static analyzer for HTML.",
        languages: &["html"],
        homepage: "https://htmlhint.com/",
    },
    LinterInfo {
        name: "jsonlint",
        description: "Validates JSON / JSONC syntax.",
        languages: &["json"],
        homepage: "https://github.com/zaach/jsonlint",
    },
    LinterInfo {
        name: "ktlint",
        description: "Kotlin linter and formatter (Kotlin coding-conventions enforced).",
        languages: &["kotlin"],
        homepage: "https://pinterest.github.io/ktlint/",
    },
    LinterInfo {
        name: "kubeconform",
        description: "Validates Kubernetes manifests against the upstream OpenAPI schema.",
        languages: &["kubernetes", "yaml"],
        homepage: "https://github.com/yannh/kubeconform",
    },
    LinterInfo {
        name: "languagetool",
        description: "Prose / grammar / style linter via LanguageTool HTTP API. Opt-in: requires LANGUAGETOOL_URL env var pointing at a self-hosted server or the public API.",
        languages: &["markdown", "prose"],
        homepage: "https://languagetool.org/",
    },
    LinterInfo {
        name: "markdownlint",
        description: "Style-rule linter for Markdown.",
        languages: &["markdown"],
        homepage: "https://github.com/DavidAnson/markdownlint",
    },
    LinterInfo {
        name: "mypy",
        description: "Static type checker for Python.",
        languages: &["python"],
        homepage: "https://mypy-lang.org/",
    },
    LinterInfo {
        name: "nilaway",
        description: "Go nil-pointer-dereference static analyzer.",
        languages: &["go"],
        homepage: "https://github.com/uber-go/nilaway",
    },
    LinterInfo {
        name: "osv-scanner",
        description: "Queries Google's OSV database for known CVEs in declared dependencies.",
        languages: &["dependencies", "security"],
        homepage: "https://google.github.io/osv-scanner/",
    },
    LinterInfo {
        name: "oxlint",
        description: "Rust-based JS/TS linter; ESLint-compatible rules without ESLint's startup cost.",
        languages: &["javascript", "typescript"],
        homepage: "https://oxc.rs/docs/guide/usage/linter.html",
    },
    LinterInfo {
        name: "phpstan",
        description: "Static analyzer for PHP.",
        languages: &["php"],
        homepage: "https://phpstan.org/",
    },
    LinterInfo {
        name: "pmd",
        description: "Static analyzer for Java (and Apex / Visualforce).",
        languages: &["java"],
        homepage: "https://pmd.github.io/",
    },
    LinterInfo {
        name: "prettier",
        description: "Multi-format opinionated formatter (JS/TS/CSS/JSON/YAML/Markdown). Catches formatting drift complementary to semantic linters.",
        languages: &["javascript", "typescript", "css", "json", "yaml", "markdown"],
        homepage: "https://prettier.io/",
    },
    LinterInfo {
        name: "pylint",
        description: "Comprehensive Python linter (style + semantic checks).",
        languages: &["python"],
        homepage: "https://pylint.pycqa.org/",
    },
    LinterInfo {
        name: "rubocop",
        description: "Ruby linter and formatter.",
        languages: &["ruby"],
        homepage: "https://rubocop.org/",
    },
    LinterInfo {
        name: "ruff",
        description: "Rust-based Python linter; superset of pyflakes/pycodestyle/isort/etc.",
        languages: &["python"],
        homepage: "https://docs.astral.sh/ruff/",
    },
    LinterInfo {
        name: "semgrep",
        description: "Pattern-based static analyzer with built-in rules for many languages and security frameworks. Always runs.",
        languages: &["multi", "security"],
        homepage: "https://semgrep.dev/",
    },
    LinterInfo {
        name: "shellcheck",
        description: "Bug-finder for sh/bash/dash/ksh shell scripts.",
        languages: &["shell"],
        homepage: "https://www.shellcheck.net/",
    },
    LinterInfo {
        name: "shfmt",
        description: "Formatter for shell scripts; complements ShellCheck (formatting drift vs. bug detection).",
        languages: &["shell"],
        homepage: "https://github.com/mvdan/sh",
    },
    LinterInfo {
        name: "sqlfluff",
        description: "Multi-dialect SQL linter and formatter.",
        languages: &["sql"],
        homepage: "https://sqlfluff.com/",
    },
    LinterInfo {
        name: "staticcheck",
        description: "State-of-the-art Go static analyzer; complements golangci-lint.",
        languages: &["go"],
        homepage: "https://staticcheck.dev/",
    },
    LinterInfo {
        name: "stylelint",
        description: "Static analyzer for CSS, SCSS, Sass, Less.",
        languages: &["css"],
        homepage: "https://stylelint.io/",
    },
    LinterInfo {
        name: "swiftlint",
        description: "Style and convention linter for Swift.",
        languages: &["swift"],
        homepage: "https://github.com/realm/SwiftLint",
    },
    LinterInfo {
        name: "taplo",
        description: "TOML formatter and linter.",
        languages: &["toml"],
        homepage: "https://taplo.tamasfe.dev/",
    },
    LinterInfo {
        name: "tflint",
        description: "Terraform linter with provider-plugin support (AWS / Azure / GCP).",
        languages: &["terraform", "iac"],
        homepage: "https://github.com/terraform-linters/tflint",
    },
    LinterInfo {
        name: "trivy",
        description: "CVE scanner for dependency manifests, Dockerfiles, and IaC; broader feed than osv-scanner. Always runs.",
        languages: &["dependencies", "iac", "docker", "security"],
        homepage: "https://trivy.dev/",
    },
    LinterInfo {
        name: "typos",
        description: "Identifier-aware spell-checker that runs on every file. Always runs.",
        languages: &["multi", "spelling"],
        homepage: "https://github.com/crate-ci/typos",
    },
    LinterInfo {
        name: "vale",
        description: "Prose linter for Markdown / docs (grammar, voice, vocabulary).",
        languages: &["markdown", "prose"],
        homepage: "https://vale.sh/",
    },
    LinterInfo {
        name: "vint",
        description: "Linter for Vimscript and `.vimrc`.",
        languages: &["vim"],
        homepage: "https://github.com/Vimjas/vint",
    },
    LinterInfo {
        name: "yamllint",
        description: "Linter for YAML files (syntax, indentation, key ordering).",
        languages: &["yaml"],
        homepage: "https://yamllint.readthedocs.io/",
    },
];

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn catalogue_is_alphabetised() {
        let names: Vec<&str> = linter_catalogue().iter().map(|l| l.name).collect();
        let mut sorted = names.clone();
        sorted.sort();
        assert_eq!(names, sorted, "catalogue must stay alphabetised");
    }

    #[test]
    fn catalogue_has_no_duplicate_names() {
        let mut seen = HashSet::new();
        for entry in linter_catalogue() {
            assert!(seen.insert(entry.name), "duplicate: {}", entry.name);
        }
    }

    #[test]
    fn catalogue_size_matches_bundled_count() {
        // Sanity guard: 45 linters are bundled. If you add or remove
        // one, update this number AND the catalogue itself.
        assert_eq!(linter_catalogue().len(), 45);
    }

    #[test]
    fn all_entries_carry_descriptions_and_languages() {
        for entry in linter_catalogue() {
            assert!(!entry.name.is_empty(), "name");
            assert!(!entry.description.is_empty(), "{}: description", entry.name);
            assert!(!entry.languages.is_empty(), "{}: languages", entry.name);
            assert!(
                entry.homepage.starts_with("https://"),
                "{}: homepage must be https",
                entry.name
            );
        }
    }
}

//! Static-analysis tool runners.
//!
//! Each runner exec's a bundled binary against a working tree and parses
//! output into a normalized [`Finding`]. Parsing is split from execution so
//! parsers can be tested directly against captured tool outputs without
//! invoking the binary.
//!
//! Milestone 1 ships 5 runners (ruff, eslint, shellcheck, hadolint,
//! markdownlint) running directly against the repo. Milestone 3 introduces
//! the OCI sandbox; runners are unchanged but execution moves into the jail.

pub mod actionlint;
pub mod ast_grep;
pub mod bandit;
pub mod biome;
pub mod checkov;
pub mod dotenv_linter;
pub mod eslint;
pub mod finding;
pub mod gitleaks;
pub mod golangci_lint;
pub mod hadolint;
pub mod kubeconform;
pub mod markdownlint;
pub mod mypy;
pub mod osv_scanner;
pub mod oxlint;
pub mod phpstan;
pub mod rubocop;
pub mod ruff;
pub mod runner;
pub mod semgrep;
pub mod shellcheck;
pub mod sqlfluff;
pub mod taplo;
pub mod trivy;
pub mod vale;
pub mod yamllint;

pub use finding::{Finding, Severity};
pub use runner::{run_all, run_in_sandbox, LinterRunner};

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

pub mod eslint;
pub mod finding;
pub mod hadolint;
pub mod markdownlint;
pub mod ruff;
pub mod runner;
pub mod shellcheck;

pub use finding::{Finding, Severity};
pub use runner::{run_all, LinterRunner};

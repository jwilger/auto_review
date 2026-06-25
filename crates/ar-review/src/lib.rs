//! Review pipeline activities.
//!
//! This crate owns workspace preparation, repository config loading, deterministic
//! triage, RAG context construction, prompt rendering, JSON self-heal, severity
//! filtering, verification, PR metadata checks, and Forgejo review mapping. The
//! orchestrator crate coordinates when these activities run for each PR.

pub mod agentic_verify;
pub mod config;
pub mod context_builder;
pub mod diff;
pub mod error;
pub mod heal;
pub mod host;
pub mod ignored;
pub mod llm_triage;
pub mod mapping;
pub mod override_marker;
pub mod pipeline;
pub mod rag_context;
pub mod triage;
pub mod verify;
pub mod workspace;
pub mod workspace_tools;

pub use agentic_verify::verify_findings_agentic;
pub use ar_prompts::ReviewSeverity;
pub use config::{
    load_repo_config, parse_repo_config, parse_repo_config_strict, RepoConfig,
    RepoConfigStrictError,
};
pub use context_builder::{
    build_review_context, build_review_context_with_store, ContextBuildError,
};
pub use diff::{cap_diff, DEFAULT_MAX_DIFF_BYTES};
pub use error::ReviewError;
pub use globset::GlobSet;
pub use heal::{generate_with_self_heal, HealConfig};
pub use host::ReviewHost;
pub use ignored::{build_glob_set, filter_changed_files, filter_diff_paths};
pub use llm_triage::{filter_reviewable, triage_files_with_llm};
pub use mapping::output_to_review_request;
pub use pipeline::{review_pull_request, ReviewArgs, ReviewOutcome, VerifyMode};
pub use rag_context::format_repo_context;
pub use triage::{classify, pr_is_skippable, FileClass};
pub use verify::verify_findings;
pub use workspace::{
    build_clone_url, prepare_workspace, prepare_workspace_from_clone_url, PreparedWorkspace,
    WorkspaceError,
};
pub use workspace_tools::{read_file, search, ReadResult, SearchHit, WorkspaceToolError};

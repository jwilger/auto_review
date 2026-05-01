//! Single-pass review pipeline.
//!
//! For Milestone 1 the activity is monolithic: fetch the PR diff, render the
//! prompt, call the LLM with JSON-schema response format, run the self-heal
//! loop on the output, map findings to a Forgejo review request, and post.
//!
//! Later milestones split this into discrete orchestrator activities
//! (triage → summarize → review → verify), with the pipeline becoming a thin
//! coordinator over them.

pub mod agentic_verify;
pub mod config;
pub mod context_builder;
pub mod diff;
pub mod error;
pub mod heal;
pub mod ignored;
pub mod llm_triage;
pub mod mapping;
pub mod pipeline;
pub mod rag_context;
pub mod routing;
pub mod triage;
pub mod verify;
pub mod workspace;
pub mod workspace_tools;

pub use agentic_verify::verify_findings_agentic;
pub use config::{load_repo_config, parse_repo_config, RepoConfig};
pub use context_builder::{build_review_context, ContextBuildError};
pub use diff::{cap_diff, DEFAULT_MAX_DIFF_BYTES};
pub use error::ReviewError;
pub use globset::GlobSet;
pub use heal::{generate_with_self_heal, HealConfig};
pub use ignored::{build_glob_set, filter_changed_files, filter_diff_paths};
pub use llm_triage::{filter_reviewable, triage_files_with_llm};
pub use mapping::output_to_review_request;
pub use pipeline::{review_pull_request, ReviewArgs, ReviewOutcome, VerifyMode};
pub use rag_context::format_repo_context;
pub use routing::{lint_workspace, lint_workspace_via, lint_workspace_with, select_runners};
pub use triage::{classify, pr_is_skippable, FileClass};
pub use verify::verify_findings;
pub use workspace::{build_clone_url, prepare_workspace, PreparedWorkspace, WorkspaceError};
pub use workspace_tools::{read_file, search, ReadResult, SearchHit, WorkspaceToolError};

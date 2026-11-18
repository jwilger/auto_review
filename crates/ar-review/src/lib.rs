//! Review pipeline activities.
//!
//! This crate owns workspace preparation, repository config loading, deterministic
//! triage, RAG context construction, prompt rendering, JSON self-heal, severity
//! filtering, verification, PR metadata checks, and Forgejo review mapping. The
//! orchestrator crate coordinates when these activities run for each PR.

extern crate alloc;

pub mod agent_loop;
pub mod agentic_verify;
pub mod config;
pub mod context_builder;
pub mod diff;
pub mod error;
pub mod heal;
pub mod host;
pub mod ignored;
pub mod mapping;
pub mod override_marker;
pub mod pipeline;
pub mod rag_context;
pub mod review_core;
pub mod triage;
pub mod trusted_files;
pub mod verify;
pub mod workspace;
pub mod workspace_tools;

#[expect(
    clippy::useless_attribute,
    reason = "clippy flags any lint attribute on a use item outside its fixed list, which omits \
              pub_use, yet the pub_use expectation below is what suppresses that lint here"
)]
#[expect(
    clippy::pub_use,
    reason = "ar-orchestrator (dispatcher), ar-cli (bench, commands), ar-chat and the red-team \
              tests import these items from the crate root (for example \
              ar_review::review_pull_request and ar_review::cap_diff); dropping the re-exports \
              breaks them"
)]
pub use crate::{
    agentic_verify::verify_findings_agentic,
    config::{parse_repo_config, parse_repo_config_strict, RepoConfig, RepoConfigStrictError},
    context_builder::{build_review_context, build_review_context_with_store, ContextBuildError},
    diff::{cap_diff, DEFAULT_MAX_DIFF_BYTES},
    error::ReviewError,
    heal::{generate_with_self_heal, HealConfig},
    host::ReviewHost,
    ignored::{build_glob_set, filter_changed_files, filter_diff_paths},
    mapping::output_to_review_request,
    pipeline::{review_pull_request, ReviewArgs, ReviewOutcome, VerifyMode},
    rag_context::format_repo_context,
    review_core::{run_review_core, ReviewCoreArgs, ReviewDraft},
    triage::{classify, pr_is_skippable, FileClass},
    trusted_files::{load_repo_config_from_base, BaseRef, LoadedRepoConfig},
    verify::verify_findings,
    workspace::{
        build_clone_url, is_valid_git_sha, local_diff_git_version_warning, prepare_workspace,
        prepare_workspace_from_clone_url, LocalDiff, MergeBaseUnavailable, PreparedWorkspace,
        WorkspaceError,
    },
    workspace_tools::{read_file, search, ReadResult, SearchHit, WorkspaceToolError},
};
#[expect(
    clippy::useless_attribute,
    reason = "clippy flags any lint attribute on a use item outside its fixed list, which omits \
              pub_use, yet the pub_use expectation below is what suppresses that lint here"
)]
#[expect(
    clippy::pub_use,
    reason = "ar-orchestrator (dispatcher) and the red-team tests name the severity and glob \
              types through ar_review, so callers need not depend on ar-prompts or globset \
              directly"
)]
pub use {ar_prompts::ReviewSeverity, globset::GlobSet};

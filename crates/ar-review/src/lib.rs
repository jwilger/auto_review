//! Single-pass review pipeline.
//!
//! For Milestone 1 the activity is monolithic: fetch the PR diff, render the
//! prompt, call the LLM with JSON-schema response format, run the self-heal
//! loop on the output, map findings to a Forgejo review request, and post.
//!
//! Later milestones split this into discrete orchestrator activities
//! (triage → summarize → review → verify), with the pipeline becoming a thin
//! coordinator over them.

pub mod error;
pub mod heal;
pub mod mapping;
pub mod pipeline;
pub mod routing;
pub mod triage;
pub mod workspace;

pub use error::ReviewError;
pub use heal::{generate_with_self_heal, HealConfig};
pub use mapping::output_to_review_request;
pub use pipeline::{review_pull_request, ReviewOutcome};
pub use routing::{lint_workspace, select_runners};
pub use triage::{classify, pr_is_skippable, FileClass};
pub use workspace::{build_clone_url, prepare_workspace, PreparedWorkspace, WorkspaceError};

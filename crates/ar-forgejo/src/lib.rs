//! Forgejo REST client (Gitea-compatible API).
//!
//! Endpoints used by the reviewer:
//! - `GET /repos/{owner}/{repo}/pulls/{n}.diff` → unified diff
//! - `GET /repos/{owner}/{repo}/pulls/{n}/files` → changed-files list
//! - `POST /repos/{owner}/{repo}/pulls/{n}/reviews` → post review with inline comments
//! - `POST /repos/{owner}/{repo}/statuses/{sha}` → commit status

pub mod client;
pub mod init;
pub mod types;
pub mod webhook;

pub use client::{Client, Error};
pub use init::InitClient;
pub use types::{
    ChangedFile, CommitStatus, CommitStatusState, CreateAccessTokenRequest, CreateReviewRequest,
    CreateWebhookRequest, CreatedAccessToken, CreatedWebhook, PrReviewComment, PrReviewCommentUser,
    PullRequestRefSummary, PullRequestSummary, ReviewComment, ReviewEvent, WebhookConfig,
};
pub use webhook::{
    Comment, Issue, IssueCommentAction, IssueCommentEvent, IssuePullRequestRef, PullRequestAction,
    PullRequestEvent,
};

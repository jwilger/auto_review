use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReviewComment {
    pub path: String,
    pub body: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub old_position: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub new_position: Option<u32>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ReviewEvent {
    Approved,
    RequestChanges,
    Comment,
    Pending,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CreateReviewRequest {
    pub body: String,
    pub commit_id: String,
    pub event: ReviewEvent,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub comments: Vec<ReviewComment>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ChangedFile {
    pub filename: String,
    pub status: String,
    #[serde(default)]
    pub additions: u32,
    #[serde(default)]
    pub deletions: u32,
    #[serde(default)]
    pub changes: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub patch: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum CommitStatusState {
    Pending,
    Success,
    Error,
    Failure,
    Warning,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CommitStatus {
    pub state: CommitStatusState,
    pub target_url: String,
    pub description: String,
    pub context: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CreateAccessTokenRequest {
    pub name: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub scopes: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct CreatedAccessToken {
    pub id: u64,
    pub name: String,
    /// The actual token secret. Forgejo returns it once at creation time;
    /// callers must save it immediately.
    pub sha1: String,
    #[serde(default)]
    pub scopes: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CreateWebhookRequest {
    #[serde(rename = "type")]
    pub kind: String,
    pub config: WebhookConfig,
    pub events: Vec<String>,
    pub active: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WebhookConfig {
    pub url: String,
    pub content_type: String,
    pub secret: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct CreatedWebhook {
    pub id: u64,
    #[serde(default)]
    pub active: bool,
    #[serde(default)]
    pub events: Vec<String>,
}

/// Compact view of a pull request, returned by `Client::get_pull_request`.
/// Mirrors the subset of Forgejo's PR-detail payload we actually need to
/// drive `run_review_job` from a CLI invocation (no webhook).
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct PullRequestSummary {
    pub number: u64,
    pub title: String,
    #[serde(default)]
    pub body: String,
    #[serde(default)]
    pub draft: bool,
    pub head: PullRequestRefSummary,
    pub base: PullRequestRefSummary,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct PullRequestRefSummary {
    #[serde(rename = "ref")]
    pub ref_name: String,
    pub sha: String,
}

/// One inline review-thread comment on a pull request — what
/// `Client::list_pr_review_comments` returns. The chat poller uses
/// `id` as a monotonic cursor (Forgejo issues ids from a single
/// sequence) and `body` to detect `@auto_review` mentions.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct PrReviewComment {
    pub id: u64,
    #[serde(default)]
    pub body: String,
    #[serde(default)]
    pub user: PrReviewCommentUser,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct PrReviewCommentUser {
    #[serde(default)]
    pub login: String,
}

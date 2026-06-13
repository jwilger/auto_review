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

/// Summary of an existing webhook returned by
/// `Client::list_webhooks`. Forgejo's `/repos/{owner}/{repo}/hooks`
/// endpoint returns the secret as `""` on read (it never emits the
/// configured secret), which is why this type only carries the URL
/// from the config map — the secret would always be empty anyway.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct WebhookSummary {
    pub id: u64,
    #[serde(rename = "type")]
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub active: bool,
    #[serde(default)]
    pub events: Vec<String>,
    /// The `config.url` field flattened up. Operators auditing
    /// installations care about which URL each webhook posts to,
    /// not the rest of the config map.
    #[serde(default)]
    pub url: String,
}

/// Forgejo's wire shape for the list-webhooks endpoint. Internal
/// to [`Client::list_webhooks`]; flattened to [`WebhookSummary`]
/// before returning so callers don't need to reach through nested
/// `config.url`.
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct WebhookListItem {
    pub id: u64,
    #[serde(rename = "type")]
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub active: bool,
    #[serde(default)]
    pub events: Vec<String>,
    #[serde(default)]
    pub config: std::collections::HashMap<String, String>,
}

impl From<WebhookListItem> for WebhookSummary {
    fn from(item: WebhookListItem) -> Self {
        Self {
            id: item.id,
            kind: item.kind,
            active: item.active,
            events: item.events,
            url: item.config.get("url").cloned().unwrap_or_default(),
        }
    }
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
    /// Forgejo's pull-request state: "open" or "closed". Populated
    /// for the chat handler's `re-review` command to skip closed
    /// PRs (running a review against a closed/merged PR's head SHA
    /// is wasted work — the user can't act on the findings).
    /// Defaults to "open" for older Forgejo versions or for
    /// payload variants that don't carry the field.
    #[serde(default = "default_pr_state")]
    pub state: String,
    pub head: PullRequestRefSummary,
    pub base: PullRequestRefSummary,
}

fn default_pr_state() -> String {
    "open".to_string()
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct PullRequestRefSummary {
    #[serde(rename = "ref")]
    pub ref_name: String,
    pub sha: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct PullReviewSummary {
    pub id: u64,
    #[serde(default)]
    pub state: String,
    /// Review author. Used to single out auto-review's own reviews when
    /// reconstructing outstanding findings for a human-override caveat.
    #[serde(default)]
    pub user: PrReviewCommentUser,
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

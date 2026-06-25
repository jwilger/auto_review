use serde::{Deserialize, Serialize};

pub use ar_forge::{
    ChangedFile, CommitStatus, CommitStatusState, CreateReviewRequest, PrReviewComment,
    PrReviewCommentUser, PullRequestRefSummary, PullRequestSummary, PullReviewSummary,
    ReviewComment, ReviewEvent,
};

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

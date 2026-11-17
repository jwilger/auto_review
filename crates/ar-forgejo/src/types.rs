use serde::{Deserialize, Serialize};

#[expect(
    clippy::useless_attribute,
    reason = "clippy flags any lint attribute on a use item outside its fixed list, which omits \
              pub_use, yet the pub_use expectation below is what suppresses that lint here"
)]
#[expect(
    clippy::pub_use,
    reason = "the crate root and ar_forgejo::types expose these ar-forge DTOs as the Forgejo \
              client's request and response types; dropping the re-export removes those paths"
)]
pub use ar_forge::{
    ChangedFile, CommitStatus, CommitStatusState, CreateReviewRequest, PrReviewComment,
    PrReviewCommentUser, PullRequestRefSummary, PullRequestSummary, PullReviewSummary,
    ReviewComment, ReviewEvent,
};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[expect(
    clippy::exhaustive_structs,
    reason = "ar-cli (commands) and this crate's InitClient tests build the token request by \
              struct literal; #[non_exhaustive] would break that construction"
)]
pub struct CreateAccessTokenRequest {
    pub name: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub scopes: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[non_exhaustive]
pub struct CreatedAccessToken {
    pub id: u64,
    pub name: String,
    #[serde(default)]
    pub scopes: Vec<String>,
    /// The actual token secret. Forgejo returns it once at creation time;
    /// callers must save it immediately.
    pub sha1: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[expect(
    clippy::exhaustive_structs,
    reason = "ar-cli (commands) and this crate's client tests build the webhook request by \
              struct literal; #[non_exhaustive] would break that construction"
)]
#[expect(
    clippy::arbitrary_source_item_ordering,
    reason = "field order sets the JSON key order of the POST /repos/{owner}/{repo}/hooks \
              request body, which stays byte-for-byte as Forgejo received it before"
)]
pub struct CreateWebhookRequest {
    #[serde(rename = "type")]
    pub kind: String,
    pub config: WebhookConfig,
    pub events: Vec<String>,
    pub active: bool,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[non_exhaustive]
pub struct CreatedWebhook {
    #[serde(default)]
    pub active: bool,
    #[serde(default)]
    pub events: Vec<String>,
    pub id: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[expect(
    clippy::exhaustive_structs,
    reason = "ar-cli (commands) and this crate's client tests build the webhook config by \
              struct literal; #[non_exhaustive] would break that construction"
)]
#[expect(
    clippy::arbitrary_source_item_ordering,
    reason = "field order sets the JSON key order of the webhook `config` object Forgejo \
              receives, which stays byte-for-byte as before"
)]
pub struct WebhookConfig {
    pub url: String,
    pub content_type: String,
    pub secret: String,
}

/// Summary of an existing webhook returned by
/// `Client::list_webhooks`. Forgejo's `/repos/{owner}/{repo}/hooks`
/// endpoint returns the secret as `""` on read (it never emits the
/// configured secret), which is why this type only carries the URL
/// from the config map — the secret would always be empty anyway.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[non_exhaustive]
#[expect(
    clippy::arbitrary_source_item_ordering,
    reason = "field order sets the JSON key order of each `auto-review webhook list --json` \
              line, which operators may parse positionally"
)]
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

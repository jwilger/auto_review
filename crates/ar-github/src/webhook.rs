//! GitHub webhook signature helpers.

use hmac::{Hmac, KeyInit as _, Mac as _};
use sha2::Sha256;
use thiserror::Error;

/// Prefix GitHub puts before the hex HMAC in `X-Hub-Signature-256`.
const SHA256_PREFIX: &str = "sha256=";

/// HMAC-SHA256, the algorithm behind `X-Hub-Signature-256`.
type HmacSha256 = Hmac<Sha256>;

#[expect(
    clippy::module_name_repetitions,
    reason = "ar-gateway (github_webhook) and this crate's webhook_signature tests import \
              WebhookSignatureError by name; renaming it breaks them"
)]
#[derive(Debug, Error, PartialEq, Eq)]
pub enum WebhookSignatureError {
    #[error("signature header must use sha256=<hex> format")]
    InvalidFormat,
    #[error("signature hex is invalid")]
    InvalidHex,
    #[error("webhook secret is invalid")]
    InvalidSecret,
    #[error("signature does not match")]
    Mismatch,
}

/// Installation reference carried by GitHub App webhook deliveries.
#[expect(
    clippy::exhaustive_structs,
    reason = "plain mirror of GitHub's webhook `installation` object; ar-github keeps its public \
              types exhaustive so workspace crates can build them by struct literal"
)]
#[derive(Debug, Clone, Copy, serde::Deserialize, PartialEq, Eq)]
pub struct InstallationRef {
    pub id: u64,
}

/// GitHub account (user, organization, or bot).
#[expect(
    clippy::exhaustive_structs,
    reason = "plain mirror of GitHub's webhook user object; ar-github keeps its public types \
              exhaustive so workspace crates can build them by struct literal"
)]
#[expect(
    clippy::module_name_repetitions,
    reason = "ar-gateway (github_webhook) imports WebhookUser by name; renaming it breaks it"
)]
#[derive(Debug, Clone, serde::Deserialize, PartialEq, Eq)]
pub struct WebhookUser {
    #[serde(rename = "type", default)]
    pub kind: String,
    pub login: String,
}

impl WebhookUser {
    /// GitHub Apps and other bot accounts report `type: "Bot"`.
    #[inline]
    #[must_use = "the check has no side effect"]
    pub fn is_bot(&self) -> bool {
        self.kind.eq_ignore_ascii_case("Bot")
    }
}

/// Repository a webhook delivery concerns.
#[expect(
    clippy::exhaustive_structs,
    reason = "plain mirror of GitHub's webhook repository object; ar-github keeps its public \
              types exhaustive so workspace crates can build them by struct literal"
)]
#[expect(
    clippy::module_name_repetitions,
    reason = "ar-gateway (github_webhook) imports WebhookRepository by name; renaming it breaks it"
)]
#[derive(Debug, Clone, serde::Deserialize, PartialEq, Eq)]
pub struct WebhookRepository {
    pub full_name: String,
    pub name: String,
    pub owner: WebhookUser,
}

/// Comment carried by `issue_comment` and `pull_request_review_comment`
/// deliveries.
#[expect(
    clippy::module_name_repetitions,
    reason = "the crate root re-exports WebhookComment by this name for dependents; renaming it \
              breaks that public path"
)]
#[expect(
    clippy::partial_pub_fields,
    reason = "body stays private so callers read it through body(), which maps GitHub's null \
              body to an empty string"
)]
#[derive(Debug, Clone, serde::Deserialize, PartialEq, Eq)]
pub struct WebhookComment {
    /// Comment text; GitHub sends `null` for an empty body.
    #[serde(default)]
    body: Option<String>,
    pub id: u64,
    pub user: WebhookUser,
}

impl WebhookComment {
    /// The comment text, or `""` when GitHub sent no body.
    #[inline]
    #[must_use = "reading the body has no side effect"]
    pub fn body(&self) -> &str {
        self.body.as_deref().unwrap_or_default()
    }
}

/// Issue (or pull request) an `issue_comment` delivery concerns.
#[expect(
    clippy::exhaustive_structs,
    reason = "plain mirror of GitHub's webhook issue object; ar-github keeps its public types \
              exhaustive so workspace crates can build them by struct literal"
)]
#[expect(
    clippy::module_name_repetitions,
    reason = "the crate root re-exports WebhookIssue by this name for dependents; renaming it \
              breaks that public path"
)]
#[derive(Debug, Clone, serde::Deserialize)]
pub struct WebhookIssue {
    pub number: u64,
    /// Present only when the issue is a pull request.
    #[serde(default)]
    pub pull_request: Option<serde_json::Value>,
}

/// `issue_comment` delivery. GitHub models pull request conversation
/// comments as issue comments.
#[expect(
    clippy::exhaustive_structs,
    reason = "plain mirror of GitHub's `issue_comment` payload; ar-github keeps its public types \
              exhaustive so workspace crates can build them by struct literal"
)]
#[derive(Debug, Clone, serde::Deserialize)]
pub struct IssueCommentEvent {
    pub action: String,
    pub comment: WebhookComment,
    #[serde(default)]
    pub installation: Option<InstallationRef>,
    pub issue: WebhookIssue,
    pub repository: WebhookRepository,
    pub sender: WebhookUser,
}

impl IssueCommentEvent {
    /// Whether the comment was made on a pull request rather than an issue.
    #[inline]
    #[must_use = "the check has no side effect"]
    pub fn is_pull_request_comment(&self) -> bool {
        self.issue.pull_request.is_some()
    }
}

/// Pull request a `pull_request_review_comment` delivery concerns.
#[expect(
    clippy::exhaustive_structs,
    reason = "plain mirror of GitHub's webhook pull request object; ar-github keeps its public \
              types exhaustive so workspace crates can build them by struct literal"
)]
#[expect(
    clippy::module_name_repetitions,
    reason = "the crate root re-exports WebhookPullRequestRef by this name for dependents; \
              renaming it breaks that public path"
)]
#[derive(Debug, Clone, Copy, serde::Deserialize, PartialEq, Eq)]
pub struct WebhookPullRequestRef {
    pub number: u64,
}

/// `pull_request_review_comment` delivery (inline diff comments and replies).
#[expect(
    clippy::exhaustive_structs,
    reason = "plain mirror of GitHub's `pull_request_review_comment` payload; ar-github keeps its \
              public types exhaustive so workspace crates can build them by struct literal"
)]
#[derive(Debug, Clone, serde::Deserialize)]
pub struct PullRequestReviewCommentEvent {
    pub action: String,
    pub comment: WebhookComment,
    #[serde(default)]
    pub installation: Option<InstallationRef>,
    pub pull_request: WebhookPullRequestRef,
    pub repository: WebhookRepository,
    pub sender: WebhookUser,
}

/// Verify GitHub's `X-Hub-Signature-256` header value.
///
/// GitHub signs the raw request body as `sha256=<hex hmac>`. The comparison is
/// delegated to the HMAC crate's constant-time verifier.
///
/// # Errors
///
/// Returns [`WebhookSignatureError::InvalidFormat`] when the header lacks the
/// `sha256=` prefix, [`WebhookSignatureError::InvalidHex`] when the digest is
/// not hex, [`WebhookSignatureError::InvalidSecret`] when the secret cannot
/// key the HMAC, and [`WebhookSignatureError::Mismatch`] when the signature
/// does not match the body.
#[expect(
    clippy::map_err_ignore,
    reason = "WebhookSignatureError is a unit-variant enum the gateway compares by value; the \
              hex and MAC errors add no detail and must not hint at how a forgery failed"
)]
#[inline]
pub fn verify_webhook_signature(
    secret: &str,
    body: &[u8],
    signature_header: &str,
) -> Result<(), WebhookSignatureError> {
    let signature_hex = signature_header
        .trim()
        .strip_prefix(SHA256_PREFIX)
        .ok_or(WebhookSignatureError::InvalidFormat)?;
    let provided = hex::decode(signature_hex).map_err(|_| WebhookSignatureError::InvalidHex)?;
    let mut mac = HmacSha256::new_from_slice(secret.as_bytes())
        .map_err(|_| WebhookSignatureError::InvalidSecret)?;
    mac.update(body);
    mac.verify_slice(&provided)
        .map_err(|_| WebhookSignatureError::Mismatch)
}

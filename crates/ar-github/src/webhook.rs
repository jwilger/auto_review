//! GitHub webhook signature helpers.

use hmac::{Hmac, KeyInit, Mac};
use sha2::Sha256;
use thiserror::Error;

type HmacSha256 = Hmac<Sha256>;

const SHA256_PREFIX: &str = "sha256=";

#[derive(Debug, Error, PartialEq, Eq)]
pub enum WebhookSignatureError {
    #[error("signature header must use sha256=<hex> format")]
    InvalidFormat,
    #[error("signature hex is invalid")]
    InvalidHex,
    #[error("signature does not match")]
    Mismatch,
    #[error("webhook secret is invalid")]
    InvalidSecret,
}

/// Verify GitHub's `X-Hub-Signature-256` header value.
///
/// GitHub signs the raw request body as `sha256=<hex hmac>`. The comparison is
/// delegated to the HMAC crate's constant-time verifier.
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

/// Installation reference carried by GitHub App webhook deliveries.
#[derive(Debug, Clone, Copy, serde::Deserialize, PartialEq, Eq)]
pub struct InstallationRef {
    pub id: u64,
}

/// GitHub account (user, organization, or bot).
#[derive(Debug, Clone, serde::Deserialize, PartialEq, Eq)]
pub struct WebhookUser {
    pub login: String,
    #[serde(rename = "type", default)]
    pub kind: String,
}

impl WebhookUser {
    /// GitHub Apps and other bot accounts report `type: "Bot"`.
    pub fn is_bot(&self) -> bool {
        self.kind.eq_ignore_ascii_case("Bot")
    }
}

#[derive(Debug, Clone, serde::Deserialize, PartialEq, Eq)]
pub struct WebhookRepository {
    pub name: String,
    pub full_name: String,
    pub owner: WebhookUser,
}

#[derive(Debug, Clone, serde::Deserialize, PartialEq, Eq)]
pub struct WebhookComment {
    pub id: u64,
    #[serde(default)]
    body: Option<String>,
    pub user: WebhookUser,
}

impl WebhookComment {
    pub fn body(&self) -> &str {
        self.body.as_deref().unwrap_or_default()
    }
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct WebhookIssue {
    pub number: u64,
    /// Present only when the issue is a pull request.
    #[serde(default)]
    pub pull_request: Option<serde_json::Value>,
}

/// `issue_comment` delivery. GitHub models pull request conversation
/// comments as issue comments.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct IssueCommentEvent {
    pub action: String,
    pub issue: WebhookIssue,
    pub comment: WebhookComment,
    pub repository: WebhookRepository,
    pub sender: WebhookUser,
    #[serde(default)]
    pub installation: Option<InstallationRef>,
}

impl IssueCommentEvent {
    pub fn is_pull_request_comment(&self) -> bool {
        self.issue.pull_request.is_some()
    }
}

#[derive(Debug, Clone, Copy, serde::Deserialize, PartialEq, Eq)]
pub struct WebhookPullRequestRef {
    pub number: u64,
}

/// `pull_request_review_comment` delivery (inline diff comments and replies).
#[derive(Debug, Clone, serde::Deserialize)]
pub struct PullRequestReviewCommentEvent {
    pub action: String,
    pub comment: WebhookComment,
    pub pull_request: WebhookPullRequestRef,
    pub repository: WebhookRepository,
    pub sender: WebhookUser,
    #[serde(default)]
    pub installation: Option<InstallationRef>,
}

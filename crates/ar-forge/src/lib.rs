//! Provider-neutral repository host DTOs.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, thiserror::Error, PartialEq, Eq)]
#[error("repository host error: {message}")]
pub struct HostError {
    message: String,
}

impl HostError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }

    pub fn message(&self) -> &str {
        &self.message
    }
}

#[async_trait]
pub trait ReviewHost: Send + Sync {
    async fn clone_url(&self, owner: &str, repo: &str) -> Result<String, HostError> {
        Err(HostError::new(format!(
            "clone URL not available for {owner}/{repo}"
        )))
    }

    async fn get_pull_request(
        &self,
        owner: &str,
        repo: &str,
        pr_number: u64,
    ) -> Result<PullRequestSummary, HostError>;

    async fn get_pr_diff(
        &self,
        owner: &str,
        repo: &str,
        pr_number: u64,
    ) -> Result<String, HostError>;

    async fn get_compare_diff(
        &self,
        owner: &str,
        repo: &str,
        base: &str,
        head: &str,
    ) -> Result<String, HostError>;

    async fn list_changed_files(
        &self,
        owner: &str,
        repo: &str,
        pr_number: u64,
    ) -> Result<Vec<ChangedFile>, HostError>;

    async fn list_pr_review_comments(
        &self,
        owner: &str,
        repo: &str,
        pr_number: u64,
    ) -> Result<Vec<PrReviewComment>, HostError>;

    async fn list_pull_reviews(
        &self,
        owner: &str,
        repo: &str,
        pr_number: u64,
    ) -> Result<Vec<PullReviewSummary>, HostError>;

    async fn list_pull_review_comments(
        &self,
        owner: &str,
        repo: &str,
        pr_number: u64,
        review_id: u64,
    ) -> Result<Vec<PrReviewComment>, HostError>;

    async fn get_file_content(
        &self,
        owner: &str,
        repo: &str,
        file_path: &str,
        ref_: &str,
    ) -> Result<Option<String>, HostError> {
        Err(HostError::new(format!(
            "file content not available for {owner}/{repo}:{ref_}:{file_path}"
        )))
    }

    async fn update_pull_request(
        &self,
        owner: &str,
        repo: &str,
        pr_number: u64,
        title: Option<&str>,
        body: Option<&str>,
    ) -> Result<(), HostError>;

    async fn post_commit_status(
        &self,
        owner: &str,
        repo: &str,
        sha: &str,
        status: &CommitStatus,
    ) -> Result<(), HostError>;

    async fn create_review(
        &self,
        owner: &str,
        repo: &str,
        pr_number: u64,
        request: &CreateReviewRequest,
    ) -> Result<CreatedReview, HostError>;

    async fn post_issue_comment(
        &self,
        owner: &str,
        repo: &str,
        issue_number: u64,
        body: &str,
    ) -> Result<u64, HostError> {
        let _ = body;
        Err(HostError::new(format!(
            "issue comments not available for {owner}/{repo}#{issue_number}"
        )))
    }
}

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
pub struct CreatedReview {
    pub id: u64,
    #[serde(default)]
    pub state: String,
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

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct PullRequestSummary {
    pub number: u64,
    pub title: String,
    #[serde(default, deserialize_with = "empty_string_when_null")]
    pub body: String,
    #[serde(default)]
    pub draft: bool,
    #[serde(default = "default_pr_state")]
    pub state: String,
    pub head: PullRequestRefSummary,
    pub base: PullRequestRefSummary,
}

fn default_pr_state() -> String {
    "open".to_string()
}

fn empty_string_when_null<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(Option::<String>::deserialize(deserializer)?.unwrap_or_default())
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
    #[serde(default)]
    pub user: PrReviewCommentUser,
    /// Number of inline (code) comments the review holds, when the host
    /// reports it. `None` means unknown, not zero.
    #[serde(default)]
    pub comments_count: Option<u64>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct PrReviewComment {
    pub id: u64,
    #[serde(default)]
    pub body: String,
    #[serde(default)]
    pub user: PrReviewCommentUser,
    /// File path an inline review comment is anchored to. Empty for
    /// top-level conversation comments.
    #[serde(default, deserialize_with = "empty_string_when_null")]
    pub path: String,
    /// New-side (head) line the inline comment is anchored to. Forgejo
    /// sets exactly one of `position` and `original_position`, depending
    /// on which side of the diff the comment is on, and leaves the other
    /// at zero. Replies in the same thread share `path`, `position` and
    /// `original_position`. Zero for top-level conversation comments.
    #[serde(default, deserialize_with = "zero_when_null")]
    pub position: u64,
    /// Old-side (base) line the inline comment is anchored to; zero for
    /// comments on the new side of the diff. See `position`.
    #[serde(default, deserialize_with = "zero_when_null")]
    pub original_position: u64,
}

fn zero_when_null<'de, D>(deserializer: D) -> Result<u64, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(Option::<u64>::deserialize(deserializer)?.unwrap_or_default())
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct PrReviewCommentUser {
    #[serde(default)]
    pub login: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pull_request_summary_treats_null_body_as_empty() {
        let summary: PullRequestSummary = serde_json::from_value(serde_json::json!({
            "number": 7,
            "title": "fix: thing",
            "body": null,
            "head": {"ref": "feature", "sha": "abc"},
            "base": {"ref": "main", "sha": "def"}
        }))
        .expect("summary");

        assert_eq!(summary.body, "");
        assert_eq!(summary.state, "open");
    }

    #[test]
    fn pr_review_comment_reads_inline_thread_location_when_present() {
        let comment: PrReviewComment = serde_json::from_value(serde_json::json!({
            "id": 12,
            "body": "@auto-review help",
            "user": {"login": "bob"},
            "path": "src/lib.rs",
            "position": 4,
            "original_position": 0
        }))
        .expect("comment");

        assert_eq!(comment.path, "src/lib.rs");
        assert_eq!(comment.position, 4);
        assert_eq!(comment.original_position, 0);
    }

    #[test]
    fn pr_review_comment_without_inline_location_still_deserializes() {
        let comment: PrReviewComment = serde_json::from_value(serde_json::json!({
            "id": 3,
            "body": "top-level",
            "user": {"login": "carol"}
        }))
        .expect("comment");

        assert_eq!(comment.path, "");
        assert_eq!(comment.position, 0);
        assert_eq!(comment.original_position, 0);
    }
}

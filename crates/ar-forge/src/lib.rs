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

    /// Fetch an issue (or a pull request, via the shared issues endpoint) by
    /// number. Returns `Ok(None)` when the host answers 404; any other
    /// failure is an error. `is_pull_request` tells issues and PRs apart.
    async fn get_issue(
        &self,
        owner: &str,
        repo: &str,
        issue_number: u64,
    ) -> Result<Option<IssueSummary>, HostError> {
        Err(HostError::new(format!(
            "issue lookup not available for {owner}/{repo}#{issue_number}"
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
}

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

/// Host-neutral issue summary returned by [`ReviewHost::get_issue`].
/// `is_pull_request` is true when the number names a pull request rather
/// than a plain issue (both hosts serve PRs on the issues endpoint).
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct IssueSummary {
    pub number: u64,
    pub title: String,
    #[serde(default, deserialize_with = "empty_string_when_null")]
    pub body: String,
    pub state: String,
    pub is_pull_request: bool,
    pub html_url: String,
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
    fn issue_summary_treats_null_or_missing_body_as_empty() {
        let null_body: IssueSummary = serde_json::from_value(serde_json::json!({
            "number": 12,
            "title": "bug: thing",
            "body": null,
            "state": "open",
            "is_pull_request": false,
            "html_url": "https://forge.example/o/r/issues/12"
        }))
        .expect("null body");
        let missing_body: IssueSummary = serde_json::from_value(serde_json::json!({
            "number": 12,
            "title": "bug: thing",
            "state": "open",
            "is_pull_request": false,
            "html_url": "https://forge.example/o/r/issues/12"
        }))
        .expect("missing body");

        assert_eq!(null_body.body, "");
        assert_eq!(missing_body, null_body);
    }

    struct RequiredOnlyHost;

    #[async_trait]
    impl ReviewHost for RequiredOnlyHost {
        async fn get_pull_request(
            &self,
            _owner: &str,
            _repo: &str,
            _pr_number: u64,
        ) -> Result<PullRequestSummary, HostError> {
            Err(HostError::new("unused"))
        }

        async fn get_pr_diff(
            &self,
            _owner: &str,
            _repo: &str,
            _pr_number: u64,
        ) -> Result<String, HostError> {
            Err(HostError::new("unused"))
        }

        async fn get_compare_diff(
            &self,
            _owner: &str,
            _repo: &str,
            _base: &str,
            _head: &str,
        ) -> Result<String, HostError> {
            Err(HostError::new("unused"))
        }

        async fn list_changed_files(
            &self,
            _owner: &str,
            _repo: &str,
            _pr_number: u64,
        ) -> Result<Vec<ChangedFile>, HostError> {
            Err(HostError::new("unused"))
        }

        async fn list_pr_review_comments(
            &self,
            _owner: &str,
            _repo: &str,
            _pr_number: u64,
        ) -> Result<Vec<PrReviewComment>, HostError> {
            Err(HostError::new("unused"))
        }

        async fn list_pull_reviews(
            &self,
            _owner: &str,
            _repo: &str,
            _pr_number: u64,
        ) -> Result<Vec<PullReviewSummary>, HostError> {
            Err(HostError::new("unused"))
        }

        async fn list_pull_review_comments(
            &self,
            _owner: &str,
            _repo: &str,
            _pr_number: u64,
            _review_id: u64,
        ) -> Result<Vec<PrReviewComment>, HostError> {
            Err(HostError::new("unused"))
        }

        async fn update_pull_request(
            &self,
            _owner: &str,
            _repo: &str,
            _pr_number: u64,
            _title: Option<&str>,
            _body: Option<&str>,
        ) -> Result<(), HostError> {
            Err(HostError::new("unused"))
        }

        async fn post_commit_status(
            &self,
            _owner: &str,
            _repo: &str,
            _sha: &str,
            _status: &CommitStatus,
        ) -> Result<(), HostError> {
            Err(HostError::new("unused"))
        }

        async fn create_review(
            &self,
            _owner: &str,
            _repo: &str,
            _pr_number: u64,
            _request: &CreateReviewRequest,
        ) -> Result<CreatedReview, HostError> {
            Err(HostError::new("unused"))
        }
    }

    #[tokio::test]
    async fn default_get_issue_returns_host_error_naming_the_issue() {
        let err = RequiredOnlyHost
            .get_issue("o", "r", 12)
            .await
            .expect_err("default get_issue must fail");

        assert!(err.message().contains("o/r#12"), "{}", err.message());
    }
}

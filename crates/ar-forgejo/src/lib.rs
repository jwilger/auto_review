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

use async_trait::async_trait;

pub use client::{Client, Error};
pub use init::InitClient;
pub use types::{
    ChangedFile, CommitStatus, CommitStatusState, CreateAccessTokenRequest, CreateReviewRequest,
    CreateWebhookRequest, CreatedAccessToken, CreatedWebhook, PrReviewComment, PrReviewCommentUser,
    PullRequestRefSummary, PullRequestSummary, PullReviewSummary, ReviewComment, ReviewEvent,
    WebhookConfig, WebhookSummary,
};
pub use webhook::{
    Comment, Issue, IssueCommentAction, IssueCommentEvent, IssuePullRequestRef, PullRequestAction,
    PullRequestEvent,
};

#[async_trait]
impl ar_forge::ReviewHost for Client {
    async fn clone_url(&self, owner: &str, repo: &str) -> Result<String, ar_forge::HostError> {
        self.clone_url(owner, repo).map_err(host_error)
    }

    async fn get_pull_request(
        &self,
        owner: &str,
        repo: &str,
        pr_number: u64,
    ) -> Result<ar_forge::PullRequestSummary, ar_forge::HostError> {
        self.get_pull_request(owner, repo, pr_number)
            .await
            .map_err(host_error)
    }

    async fn get_pr_diff(
        &self,
        owner: &str,
        repo: &str,
        pr_number: u64,
    ) -> Result<String, ar_forge::HostError> {
        self.get_pr_diff(owner, repo, pr_number)
            .await
            .map_err(host_error)
    }

    async fn get_compare_diff(
        &self,
        owner: &str,
        repo: &str,
        base: &str,
        head: &str,
    ) -> Result<String, ar_forge::HostError> {
        self.get_compare_diff(owner, repo, base, head)
            .await
            .map_err(host_error)
    }

    async fn list_changed_files(
        &self,
        owner: &str,
        repo: &str,
        pr_number: u64,
    ) -> Result<Vec<ar_forge::ChangedFile>, ar_forge::HostError> {
        self.list_changed_files(owner, repo, pr_number)
            .await
            .map_err(host_error)
    }

    async fn list_pr_review_comments(
        &self,
        owner: &str,
        repo: &str,
        pr_number: u64,
    ) -> Result<Vec<ar_forge::PrReviewComment>, ar_forge::HostError> {
        self.list_pr_review_comments(owner, repo, pr_number)
            .await
            .map_err(host_error)
    }

    async fn list_pull_reviews(
        &self,
        owner: &str,
        repo: &str,
        pr_number: u64,
    ) -> Result<Vec<ar_forge::PullReviewSummary>, ar_forge::HostError> {
        self.list_pull_reviews(owner, repo, pr_number)
            .await
            .map_err(host_error)
    }

    async fn list_pull_review_comments(
        &self,
        owner: &str,
        repo: &str,
        pr_number: u64,
        review_id: u64,
    ) -> Result<Vec<ar_forge::PrReviewComment>, ar_forge::HostError> {
        self.list_pull_review_comments(owner, repo, pr_number, review_id)
            .await
            .map_err(host_error)
    }

    async fn get_file_content(
        &self,
        owner: &str,
        repo: &str,
        file_path: &str,
        ref_: &str,
    ) -> Result<Option<String>, ar_forge::HostError> {
        self.get_file_content(owner, repo, file_path, ref_)
            .await
            .map_err(host_error)
    }

    async fn update_pull_request(
        &self,
        owner: &str,
        repo: &str,
        pr_number: u64,
        title: Option<&str>,
        body: Option<&str>,
    ) -> Result<(), ar_forge::HostError> {
        self.update_pull_request(owner, repo, pr_number, title, body)
            .await
            .map_err(host_error)
    }

    async fn post_commit_status(
        &self,
        owner: &str,
        repo: &str,
        sha: &str,
        status: &ar_forge::CommitStatus,
    ) -> Result<(), ar_forge::HostError> {
        self.post_commit_status(owner, repo, sha, status)
            .await
            .map_err(host_error)
    }

    async fn create_review(
        &self,
        owner: &str,
        repo: &str,
        pr_number: u64,
        request: &ar_forge::CreateReviewRequest,
    ) -> Result<ar_forge::CreatedReview, ar_forge::HostError> {
        self.create_review(owner, repo, pr_number, request)
            .await
            .map_err(host_error)
    }

    async fn post_issue_comment(
        &self,
        owner: &str,
        repo: &str,
        issue_number: u64,
        body: &str,
    ) -> Result<u64, ar_forge::HostError> {
        self.post_issue_comment(owner, repo, issue_number, body)
            .await
            .map_err(host_error)
    }
}

fn host_error(error: Error) -> ar_forge::HostError {
    ar_forge::HostError::new(error.to_string())
}

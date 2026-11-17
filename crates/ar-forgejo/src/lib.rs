//! Forgejo REST client (Gitea-compatible API).
//!
//! Endpoints used by the reviewer:
//! - `GET /repos/{owner}/{repo}/pulls/{n}.diff` → unified diff
//! - `GET /repos/{owner}/{repo}/pulls/{n}/files` → changed-files list
//! - `POST /repos/{owner}/{repo}/pulls/{n}/reviews` → post review with inline comments
//! - `POST /repos/{owner}/{repo}/statuses/{sha}` → commit status

pub mod capabilities;
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
    async fn capabilities(&self) -> ar_forge::HostCapabilities {
        // Transport, HTTP and timeout failures leave the cell empty so the
        // next call probes again; an unparseable version is deterministic
        // and is cached.
        let probed = self
            .capabilities
            .get_or_try_init(|| async {
                let version = probe(self.get_server_version()).await?;
                Ok::<_, Error>(
                    capabilities::capabilities_for_version(&version).unwrap_or_else(|| {
                        tracing::warn!(
                            version = %client::cap_for_error(&version),
                            "unparseable Forgejo version; treating the server as \
                             supporting no optional review features"
                        );
                        ar_forge::HostCapabilities::default()
                    }),
                )
            })
            .await;
        match probed {
            Ok(capabilities) => *capabilities,
            Err(error) => {
                tracing::warn!(
                    error = %error,
                    "Forgejo version probe failed; treating the server as \
                     supporting no optional review features until a later probe succeeds"
                );
                ar_forge::HostCapabilities::default()
            }
        }
    }

    async fn bot_login(&self) -> Result<String, ar_forge::HostError> {
        self.bot_login
            .get_or_try_init(|| probe(self.get_authenticated_user_login()))
            .await
            .cloned()
            .map_err(host_error)
    }

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

    async fn get_issue(
        &self,
        owner: &str,
        repo: &str,
        issue_number: u64,
    ) -> Result<Option<ar_forge::IssueSummary>, ar_forge::HostError> {
        self.get_issue(owner, repo, issue_number)
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

    async fn edit_issue_comment(
        &self,
        owner: &str,
        repo: &str,
        comment_id: u64,
        body: &str,
    ) -> Result<(), ar_forge::HostError> {
        self.edit_issue_comment(owner, repo, comment_id, body)
            .await
            .map_err(host_error)
    }

    /// Forgejo v11+ edits inline code comments through the
    /// issue-comment endpoint; there is no `pulls/comments/{id}` PATCH.
    async fn edit_review_comment(
        &self,
        owner: &str,
        repo: &str,
        comment_id: u64,
        body: &str,
    ) -> Result<(), ar_forge::HostError> {
        self.edit_issue_comment(owner, repo, comment_id, body)
            .await
            .map_err(host_error)
    }
}

/// Upper bound on a host-identity request (`GET /version`, `GET /user`).
/// Concurrent callers wait on the same cached initialiser, so a server that
/// accepts the connection but never answers must not block them forever.
const PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

async fn probe<T>(
    request: impl std::future::Future<Output = Result<T, Error>>,
) -> Result<T, Error> {
    tokio::time::timeout(PROBE_TIMEOUT, request)
        .await
        .unwrap_or(Err(Error::Timeout(PROBE_TIMEOUT)))
}

fn host_error(error: Error) -> ar_forge::HostError {
    ar_forge::HostError::new(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(start_paused = true)]
    async fn probe_times_out_a_request_that_never_answers() {
        let result = probe(std::future::pending::<Result<(), Error>>()).await;

        assert!(matches!(result, Err(Error::Timeout(_))), "{result:?}");
    }
}

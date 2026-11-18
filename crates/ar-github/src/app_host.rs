//! Long-lived GitHub App [`ar_forge::ReviewHost`] for the always-on gateway.
//!
//! [`crate::InstallationReviewHost`] wraps one already-minted installation
//! token, which suits a single CI invocation. A running service instead needs
//! to serve many repositories over many hours, so [`AppReviewHost`] resolves
//! the installation covering each repository, mints a repository-scoped
//! installation token on demand, and relies on [`Client`]'s token cache to
//! reuse it until shortly before expiry.

use crate::{
    host_error, resolve_bot_login, with_clone_credentials, Client, Error, InstallationTokenRequest,
    Permission, IDENTITY_TIMEOUT,
};
use async_trait::async_trait;
use core::time::Duration;
use std::collections::HashMap;
use tokio::sync::{Mutex, OnceCell};

/// A [`ar_forge::ReviewHost`] that mints repository-scoped installation
/// tokens on demand for every repository the app is installed on.
#[derive(Debug)]
pub struct AppReviewHost {
    /// The bot login, resolved once from `GET /app` or seeded by the caller.
    bot_login: OnceCell<String>,
    /// REST client; built with an app signer so JWTs stay fresh.
    client: Client,
    /// Bound on the `GET /app` identity request.
    identity_timeout: Duration,
    /// Installation id per lowercased `owner/repo`.
    installations: Mutex<HashMap<String, u64>>,
}

impl AppReviewHost {
    /// The underlying REST client.
    #[inline]
    #[must_use = "returning the client has no side effect"]
    pub fn client(&self) -> &Client {
        &self.client
    }

    /// The installation covering `owner/repo`, from the cache or a lookup.
    async fn installation_id(&self, owner: &str, repo: &str) -> Result<u64, Error> {
        let key = installation_key(owner, repo);
        if let Some(id) = self.installations.lock().await.get(&key).copied() {
            return Ok(id);
        }
        let id = self
            .client
            .get_repository_installation_id(owner, repo)
            .await?;
        self.installations.lock().await.insert(key, id);
        Ok(id)
    }

    /// Mint (or reuse) an installation token scoped to `owner/repo`.
    ///
    /// # Errors
    ///
    /// Returns an error when the installation lookup or the token request
    /// fails. An authorization or not-found answer also drops the cached
    /// installation id so the next call looks it up again.
    #[inline]
    pub async fn installation_token_for(&self, owner: &str, repo: &str) -> Result<String, Error> {
        let installation_id = self.installation_id(owner, repo).await?;
        match self
            .client
            .installation_token(installation_id, review_installation_token_request(repo))
            .await
        {
            Ok(token) => Ok(token.token),
            Err(error) => {
                // The app may have been uninstalled or the repository removed
                // from the installation; re-resolve on the next call.
                #[expect(
                    clippy::rest_pattern_accessible_field,
                    reason = "only the status decides whether the installation is stale; the \
                              error body is irrelevant, and naming it as `body: _` trips \
                              unneeded_field_pattern instead"
                )]
                let installation_gone = matches!(
                    error,
                    Error::Api {
                        status: 401 | 403 | 404 | 422,
                        ..
                    }
                );
                if installation_gone {
                    self.installations
                        .lock()
                        .await
                        .remove(&installation_key(owner, repo));
                }
                Err(error)
            }
        }
    }

    /// `client` should be built with [`Client::with_app_signer`] so app JWTs
    /// stay fresh for the life of the process.
    #[inline]
    pub fn new(client: Client) -> Self {
        Self {
            client,
            installations: Mutex::new(HashMap::new()),
            bot_login: OnceCell::new(),
            identity_timeout: IDENTITY_TIMEOUT,
        }
    }

    /// Record the installation id a webhook delivery reported for a
    /// repository, saving an installation lookup on the next API call.
    #[inline]
    pub async fn remember_installation(&self, owner: &str, repo: &str, installation_id: u64) {
        self.installations
            .lock()
            .await
            .insert(installation_key(owner, repo), installation_id);
    }

    /// An installation token for `owner/repo`, as a host error on failure.
    async fn token(&self, owner: &str, repo: &str) -> Result<String, ar_forge::HostError> {
        self.installation_token_for(owner, repo)
            .await
            .map_err(|error| host_error(&error))
    }

    /// Seed the bot login resolved elsewhere (the gateway's startup
    /// identity, which honours `GITHUB_BOT_LOGIN`), so
    /// [`ar_forge::ReviewHost::bot_login`] never calls `GET /app`.
    #[inline]
    #[must_use = "the seeded login lives only on the returned host"]
    pub fn with_bot_login(mut self, login: impl Into<String>) -> Self {
        self.bot_login = OnceCell::new_with(Some(login.into()));
        self
    }

    /// Override the bound on the `GET /app` identity request
    /// (default [`IDENTITY_TIMEOUT`]).
    #[inline]
    #[must_use = "the timeout applies only to the returned host"]
    pub fn with_identity_timeout(mut self, timeout: Duration) -> Self {
        self.identity_timeout = timeout;
        self
    }
}

#[async_trait]
impl ar_forge::ReviewHost for AppReviewHost {
    #[inline]
    async fn bot_login(&self) -> Result<String, ar_forge::HostError> {
        self.bot_login
            .get_or_try_init(|| resolve_bot_login(&self.client, self.identity_timeout))
            .await
            .cloned()
    }

    #[inline]
    async fn capabilities(&self) -> ar_forge::HostCapabilities {
        ar_forge::HostCapabilities::all()
    }

    #[inline]
    async fn clone_url(&self, owner: &str, repo: &str) -> Result<String, ar_forge::HostError> {
        let token = self.token(owner, repo).await?;
        let clone_url = self
            .client
            .get_repository_clone_url(owner, repo, &token)
            .await
            .map_err(|error| host_error(&error))?;
        with_clone_credentials(&clone_url, &token).map_err(|error| host_error(&error))
    }

    #[inline]
    async fn create_review(
        &self,
        owner: &str,
        repo: &str,
        pr_number: u64,
        request: &ar_forge::CreateReviewRequest,
    ) -> Result<ar_forge::CreatedReview, ar_forge::HostError> {
        let token = self.token(owner, repo).await?;
        self.client
            .create_review(owner, repo, pr_number, request, &token)
            .await
            .map_err(|error| host_error(&error))
    }

    #[inline]
    async fn edit_issue_comment(
        &self,
        owner: &str,
        repo: &str,
        comment_id: u64,
        body: &str,
    ) -> Result<(), ar_forge::HostError> {
        let token = self.token(owner, repo).await?;
        self.client
            .edit_issue_comment(owner, repo, comment_id, body, &token)
            .await
            .map_err(|error| host_error(&error))
    }

    #[inline]
    async fn edit_review_comment(
        &self,
        owner: &str,
        repo: &str,
        comment_id: u64,
        body: &str,
    ) -> Result<(), ar_forge::HostError> {
        let token = self.token(owner, repo).await?;
        self.client
            .edit_review_comment(owner, repo, comment_id, body, &token)
            .await
            .map_err(|error| host_error(&error))
    }

    #[inline]
    async fn get_compare_diff(
        &self,
        owner: &str,
        repo: &str,
        base: &str,
        head: &str,
    ) -> Result<String, ar_forge::HostError> {
        let token = self.token(owner, repo).await?;
        self.client
            .get_compare_diff(owner, repo, base, head, &token)
            .await
            .map_err(|error| host_error(&error))
    }

    #[inline]
    async fn get_file_content(
        &self,
        owner: &str,
        repo: &str,
        file_path: &str,
        ref_: &str,
    ) -> Result<Option<String>, ar_forge::HostError> {
        let token = self.token(owner, repo).await?;
        self.client
            .get_file_content(owner, repo, file_path, ref_, &token)
            .await
            .map_err(|error| host_error(&error))
    }

    #[inline]
    async fn get_issue(
        &self,
        owner: &str,
        repo: &str,
        issue_number: u64,
    ) -> Result<Option<ar_forge::IssueSummary>, ar_forge::HostError> {
        let token = self.token(owner, repo).await?;
        self.client
            .get_issue(owner, repo, issue_number, &token)
            .await
            .map_err(|error| host_error(&error))
    }

    #[inline]
    async fn get_pr_diff(
        &self,
        owner: &str,
        repo: &str,
        pr_number: u64,
    ) -> Result<String, ar_forge::HostError> {
        let token = self.token(owner, repo).await?;
        self.client
            .get_pull_request_diff(owner, repo, pr_number, &token)
            .await
            .map_err(|error| host_error(&error))
    }

    #[inline]
    async fn get_pull_request(
        &self,
        owner: &str,
        repo: &str,
        pr_number: u64,
    ) -> Result<ar_forge::PullRequestSummary, ar_forge::HostError> {
        let token = self.token(owner, repo).await?;
        self.client
            .get_pull_request(owner, repo, pr_number, &token)
            .await
            .map_err(|error| host_error(&error))
    }

    #[inline]
    async fn list_changed_files(
        &self,
        owner: &str,
        repo: &str,
        pr_number: u64,
    ) -> Result<Vec<ar_forge::ChangedFile>, ar_forge::HostError> {
        let token = self.token(owner, repo).await?;
        self.client
            .list_changed_files(owner, repo, pr_number, &token)
            .await
            .map_err(|error| host_error(&error))
    }

    #[inline]
    async fn list_pr_review_comments(
        &self,
        owner: &str,
        repo: &str,
        pr_number: u64,
    ) -> Result<Vec<ar_forge::PrReviewComment>, ar_forge::HostError> {
        let token = self.token(owner, repo).await?;
        self.client
            .list_pr_review_comments(owner, repo, pr_number, &token)
            .await
            .map_err(|error| host_error(&error))
    }

    #[inline]
    async fn list_pull_review_comments(
        &self,
        owner: &str,
        repo: &str,
        pr_number: u64,
        review_id: u64,
    ) -> Result<Vec<ar_forge::PrReviewComment>, ar_forge::HostError> {
        let token = self.token(owner, repo).await?;
        self.client
            .list_pull_review_comments(owner, repo, pr_number, review_id, &token)
            .await
            .map_err(|error| host_error(&error))
    }

    #[inline]
    async fn list_pull_reviews(
        &self,
        owner: &str,
        repo: &str,
        pr_number: u64,
    ) -> Result<Vec<ar_forge::PullReviewSummary>, ar_forge::HostError> {
        let token = self.token(owner, repo).await?;
        self.client
            .list_pull_reviews(owner, repo, pr_number, &token)
            .await
            .map_err(|error| host_error(&error))
    }

    #[inline]
    async fn post_commit_status(
        &self,
        owner: &str,
        repo: &str,
        sha: &str,
        status: &ar_forge::CommitStatus,
    ) -> Result<(), ar_forge::HostError> {
        let token = self.token(owner, repo).await?;
        self.client
            .post_commit_status(owner, repo, sha, status, &token)
            .await
            .map_err(|error| host_error(&error))
    }

    #[inline]
    async fn post_issue_comment(
        &self,
        owner: &str,
        repo: &str,
        issue_number: u64,
        body: &str,
    ) -> Result<u64, ar_forge::HostError> {
        let token = self.token(owner, repo).await?;
        self.client
            .post_issue_comment(owner, repo, issue_number, body, &token)
            .await
            .map_err(|error| host_error(&error))
    }

    #[inline]
    async fn update_pull_request(
        &self,
        owner: &str,
        repo: &str,
        pr_number: u64,
        title: Option<&str>,
        body: Option<&str>,
    ) -> Result<(), ar_forge::HostError> {
        let token = self.token(owner, repo).await?;
        self.client
            .update_pull_request(owner, repo, pr_number, title, body, &token)
            .await
            .map_err(|error| host_error(&error))
    }
}

/// Cache key for `owner/repo`; GitHub owner and repository names are
/// case-insensitive.
fn installation_key(owner: &str, repo: &str) -> String {
    format!(
        "{}/{}",
        owner.to_ascii_lowercase(),
        repo.to_ascii_lowercase()
    )
}

/// Permissions requested for every review-host installation token. The token
/// is also restricted to the single repository being reviewed.
#[inline]
#[must_use = "building the request has no side effect"]
pub fn review_installation_token_request(repo: &str) -> InstallationTokenRequest {
    InstallationTokenRequest::for_repository(repo)
        .with_permission("contents", Permission::Read)
        .with_permission("issues", Permission::Write)
        .with_permission("pull_requests", Permission::Write)
        .with_permission("statuses", Permission::Write)
}

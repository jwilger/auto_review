//! GitHub App REST client.

pub mod webhook;

use async_trait::async_trait;
use reqwest::header::{HeaderMap, HeaderValue, ACCEPT, AUTHORIZATION, CONTENT_TYPE, USER_AGENT};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use thiserror::Error;
use time::format_description::well_known::Rfc3339;
use time::{Duration, OffsetDateTime};
use tokio::sync::Mutex;
use url::Url;

pub use webhook::{verify_webhook_signature, WebhookSignatureError};

const GITHUB_API_VERSION: &str = "2022-11-28";
const TOKEN_REFRESH_MARGIN: Duration = Duration::minutes(5);

#[derive(Debug, Error)]
pub enum Error {
    #[error("invalid base URL: {0}")]
    InvalidBaseUrl(String),
    #[error("failed to set clone credentials on URL")]
    CloneCredentialEncoding,
    #[error("invalid header value: {0}")]
    InvalidHeader(String),
    #[error("HTTP error: {0}")]
    Http(#[from] reqwest::Error),
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("JWT error: {0}")]
    Jwt(#[from] jsonwebtoken::errors::Error),
    #[error("API error {status}: {body}")]
    Api { status: u16, body: String },
    #[error("URL build error: {0}")]
    UrlBuild(#[from] url::ParseError),
    #[error("invalid expires_at timestamp: {0}")]
    InvalidExpiresAt(#[from] time::error::Parse),
}

pub fn build_clone_url(
    base_url: &str,
    owner: &str,
    repo: &str,
    installation_token: &str,
) -> Result<String, Error> {
    let mut url = Url::parse(base_url).map_err(|_| Error::InvalidBaseUrl(base_url.to_string()))?;
    let path = {
        let trimmed = url.path().trim_end_matches('/');
        format!("{trimmed}/{owner}/{repo}.git")
    };
    url.set_path(&path);
    url.set_username("x-access-token")
        .map_err(|_| Error::CloneCredentialEncoding)?;
    url.set_password(Some(installation_token))
        .map_err(|_| Error::CloneCredentialEncoding)?;
    Ok(url.to_string())
}

/// Add installation-token userinfo to a repository's HTTPS clone URL.
pub fn with_clone_credentials(clone_url: &str, installation_token: &str) -> Result<String, Error> {
    let mut url =
        Url::parse(clone_url).map_err(|_| Error::InvalidBaseUrl(clone_url.to_string()))?;
    url.set_username("x-access-token")
        .map_err(|_| Error::CloneCredentialEncoding)?;
    url.set_password(Some(installation_token))
        .map_err(|_| Error::CloneCredentialEncoding)?;
    Ok(url.to_string())
}

#[derive(Debug)]
pub struct InstallationReviewHost {
    client: Client,
    installation_token: String,
}

impl InstallationReviewHost {
    pub fn new(client: Client, installation_token: impl Into<String>) -> Self {
        Self {
            client,
            installation_token: installation_token.into(),
        }
    }
}

#[async_trait]
impl ar_forge::ReviewHost for InstallationReviewHost {
    async fn clone_url(&self, owner: &str, repo: &str) -> Result<String, ar_forge::HostError> {
        let clone_url = self
            .client
            .get_repository_clone_url(owner, repo, &self.installation_token)
            .await
            .map_err(host_error)?;
        with_clone_credentials(&clone_url, &self.installation_token).map_err(host_error)
    }

    async fn get_pull_request(
        &self,
        owner: &str,
        repo: &str,
        pr_number: u64,
    ) -> Result<ar_forge::PullRequestSummary, ar_forge::HostError> {
        self.client
            .get_pull_request(owner, repo, pr_number, &self.installation_token)
            .await
            .map_err(host_error)
    }

    async fn get_pr_diff(
        &self,
        owner: &str,
        repo: &str,
        pr_number: u64,
    ) -> Result<String, ar_forge::HostError> {
        self.client
            .get_pull_request_diff(owner, repo, pr_number, &self.installation_token)
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
        self.client
            .get_compare_diff(owner, repo, base, head, &self.installation_token)
            .await
            .map_err(host_error)
    }

    async fn list_changed_files(
        &self,
        owner: &str,
        repo: &str,
        pr_number: u64,
    ) -> Result<Vec<ar_forge::ChangedFile>, ar_forge::HostError> {
        self.client
            .list_changed_files(owner, repo, pr_number, &self.installation_token)
            .await
            .map_err(host_error)
    }

    async fn list_pr_review_comments(
        &self,
        owner: &str,
        repo: &str,
        pr_number: u64,
    ) -> Result<Vec<ar_forge::PrReviewComment>, ar_forge::HostError> {
        self.client
            .list_pr_review_comments(owner, repo, pr_number, &self.installation_token)
            .await
            .map_err(host_error)
    }

    async fn list_pull_reviews(
        &self,
        owner: &str,
        repo: &str,
        pr_number: u64,
    ) -> Result<Vec<ar_forge::PullReviewSummary>, ar_forge::HostError> {
        self.client
            .list_pull_reviews(owner, repo, pr_number, &self.installation_token)
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
        self.client
            .list_pull_review_comments(owner, repo, pr_number, review_id, &self.installation_token)
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
        self.client
            .get_file_content(owner, repo, file_path, ref_, &self.installation_token)
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
        self.client
            .update_pull_request(
                owner,
                repo,
                pr_number,
                title,
                body,
                &self.installation_token,
            )
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
        self.client
            .post_commit_status(owner, repo, sha, status, &self.installation_token)
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
        self.client
            .create_review(owner, repo, pr_number, request, &self.installation_token)
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
        self.client
            .post_issue_comment(owner, repo, issue_number, body, &self.installation_token)
            .await
            .map_err(host_error)
    }
}

fn host_error(error: Error) -> ar_forge::HostError {
    ar_forge::HostError::new(error.to_string())
}

#[derive(Debug, Clone)]
pub struct GitHubAppJwt {
    app_id: u64,
    key: jsonwebtoken::EncodingKey,
}

impl GitHubAppJwt {
    pub fn from_rsa_pem(app_id: u64, private_key_pem: &[u8]) -> Result<Self, Error> {
        let key = jsonwebtoken::EncodingKey::from_rsa_pem(private_key_pem)?;
        Ok(Self { app_id, key })
    }

    pub fn jwt_at_unix(&self, issued_at: i64) -> Result<String, Error> {
        #[derive(Serialize)]
        struct Claims {
            iss: String,
            iat: i64,
            exp: i64,
        }

        let claims = Claims {
            iss: self.app_id.to_string(),
            iat: issued_at,
            exp: issued_at + 600,
        };
        let header = jsonwebtoken::Header::new(jsonwebtoken::Algorithm::RS256);
        Ok(jsonwebtoken::encode(&header, &claims, &self.key)?)
    }

    pub fn jwt_now(&self) -> Result<String, Error> {
        self.jwt_at_unix(OffsetDateTime::now_utc().unix_timestamp())
    }
}

#[derive(Debug)]
pub struct Client {
    http: reqwest::Client,
    base: Url,
    installation_tokens: Mutex<HashMap<InstallationTokenCacheKey, CachedInstallationToken>>,
}

impl Client {
    pub fn new(base_url: &str, app_jwt: &str) -> Result<Self, Error> {
        let normalized = if base_url.ends_with('/') {
            base_url.to_string()
        } else {
            format!("{base_url}/")
        };
        let base =
            Url::parse(&normalized).map_err(|_| Error::InvalidBaseUrl(normalized.clone()))?;

        let mut headers = HeaderMap::new();
        let mut auth_value = HeaderValue::from_str(&format!("Bearer {app_jwt}"))
            .map_err(|_| Error::InvalidHeader("app JWT must be valid header text".to_string()))?;
        auth_value.set_sensitive(true);
        headers.insert(AUTHORIZATION, auth_value);
        headers.insert(
            ACCEPT,
            HeaderValue::from_static("application/vnd.github+json"),
        );
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        headers.insert(
            "X-GitHub-Api-Version",
            HeaderValue::from_static(GITHUB_API_VERSION),
        );
        headers.insert(
            USER_AGENT,
            HeaderValue::from_static(concat!("auto_review/", env!("CARGO_PKG_VERSION"))),
        );
        let http = reqwest::Client::builder()
            .default_headers(headers)
            .build()?;

        Ok(Self {
            http,
            base,
            installation_tokens: Mutex::new(HashMap::new()),
        })
    }

    pub async fn installation_token(
        &self,
        installation_id: u64,
        request: InstallationTokenRequest,
    ) -> Result<InstallationToken, Error> {
        let key = InstallationTokenCacheKey {
            installation_id,
            request: request.clone(),
        };
        if let Some(cached) = self.fresh_cached_token(&key).await {
            return Ok(cached);
        }

        let url = self.url(&format!(
            "app/installations/{installation_id}/access_tokens"
        ))?;
        let response = self.http.post(url).json(&request).send().await?;
        let status = response.status();
        let body = response.text().await?;
        if !status.is_success() {
            return Err(Error::Api {
                status: status.as_u16(),
                body: cap_for_error(&body),
            });
        }

        let token: InstallationToken = serde_json::from_str(&body)?;
        let expires_at = OffsetDateTime::parse(&token.expires_at, &Rfc3339)?;
        let cached = CachedInstallationToken {
            token: token.clone(),
            expires_at,
        };
        self.installation_tokens.lock().await.insert(key, cached);
        Ok(token)
    }

    pub async fn get_pull_request(
        &self,
        owner: &str,
        repo: &str,
        pull_number: u64,
        installation_token: &str,
    ) -> Result<ar_forge::PullRequestSummary, Error> {
        let url = self.url(&format!("repos/{owner}/{repo}/pulls/{pull_number}"))?;
        self.get_with_installation_token(url, installation_token)
            .await
    }

    /// Fetch the repository's HTTPS `clone_url`. The REST API host is not
    /// the git host on github.com or GitHub Enterprise Server.
    pub async fn get_repository_clone_url(
        &self,
        owner: &str,
        repo: &str,
        installation_token: &str,
    ) -> Result<String, Error> {
        #[derive(Deserialize)]
        struct Repository {
            clone_url: String,
        }
        let url = self.url(&format!("repos/{owner}/{repo}"))?;
        let repository: Repository = self
            .get_with_installation_token(url, installation_token)
            .await?;
        Ok(repository.clone_url)
    }

    pub async fn list_changed_files(
        &self,
        owner: &str,
        repo: &str,
        pull_number: u64,
        installation_token: &str,
    ) -> Result<Vec<ar_forge::ChangedFile>, Error> {
        let mut all = Vec::new();
        for page in 1..=30 {
            let url = self.url(&format!(
                "repos/{owner}/{repo}/pulls/{pull_number}/files?page={page}&per_page=100"
            ))?;
            let chunk: Vec<ar_forge::ChangedFile> = self
                .get_with_installation_token(url, installation_token)
                .await?;
            let chunk_len = chunk.len();
            all.extend(chunk);
            if chunk_len < 100 {
                break;
            }
        }
        Ok(all)
    }

    pub async fn get_pull_request_diff(
        &self,
        owner: &str,
        repo: &str,
        pull_number: u64,
        installation_token: &str,
    ) -> Result<String, Error> {
        let url = self.url(&format!("repos/{owner}/{repo}/pulls/{pull_number}"))?;
        self.get_text_with_installation_token(
            url,
            installation_token,
            HeaderValue::from_static("application/vnd.github.v3.diff"),
        )
        .await
    }

    pub async fn get_compare_diff(
        &self,
        owner: &str,
        repo: &str,
        base: &str,
        head: &str,
        installation_token: &str,
    ) -> Result<String, Error> {
        let url = self.url(&format!("repos/{owner}/{repo}/compare/{base}...{head}"))?;
        self.get_text_with_installation_token(
            url,
            installation_token,
            HeaderValue::from_static("application/vnd.github.v3.diff"),
        )
        .await
    }

    pub async fn create_review(
        &self,
        owner: &str,
        repo: &str,
        pull_number: u64,
        request: &ar_forge::CreateReviewRequest,
        installation_token: &str,
    ) -> Result<ar_forge::CreatedReview, Error> {
        let url = self.url(&format!("repos/{owner}/{repo}/pulls/{pull_number}/reviews"))?;
        let github_request = GitHubCreateReviewRequest::from(request);
        match self
            .post_with_installation_token(url.clone(), &github_request, installation_token)
            .await
        {
            // GitHub rejects the whole review when any inline comment targets a
            // line outside the diff. Keep the findings visible by retrying with
            // the inline comments folded into the review body.
            Err(Error::Api { status: 422, .. }) if !github_request.comments.is_empty() => {
                let folded_body = fold_inline_comments_into_body(request);
                let fallback = GitHubCreateReviewRequest {
                    body: folded_body.as_str(),
                    commit_id: github_request.commit_id,
                    event: github_request.event,
                    comments: Vec::new(),
                };
                self.post_with_installation_token(url, &fallback, installation_token)
                    .await
            }
            other => other,
        }
    }

    pub async fn post_commit_status(
        &self,
        owner: &str,
        repo: &str,
        sha: &str,
        status: &ar_forge::CommitStatus,
        installation_token: &str,
    ) -> Result<(), Error> {
        let url = self.url(&format!("repos/{owner}/{repo}/statuses/{sha}"))?;
        let status = GitHubCommitStatus::from(status);
        let _: serde_json::Value = self
            .post_with_installation_token(url, &status, installation_token)
            .await?;
        Ok(())
    }

    pub async fn list_pr_review_comments(
        &self,
        owner: &str,
        repo: &str,
        pull_number: u64,
        installation_token: &str,
    ) -> Result<Vec<ar_forge::PrReviewComment>, Error> {
        let mut all = Vec::new();
        for page in 1..=30 {
            let url = self.url(&format!(
                "repos/{owner}/{repo}/issues/{pull_number}/comments?page={page}&per_page=100"
            ))?;
            let chunk: Vec<ar_forge::PrReviewComment> = self
                .get_with_installation_token(url, installation_token)
                .await?;
            let chunk_len = chunk.len();
            all.extend(chunk);
            if chunk_len < 100 {
                break;
            }
        }
        Ok(all)
    }

    pub async fn list_pull_reviews(
        &self,
        owner: &str,
        repo: &str,
        pull_number: u64,
        installation_token: &str,
    ) -> Result<Vec<ar_forge::PullReviewSummary>, Error> {
        let mut all = Vec::new();
        for page in 1..=30 {
            let url = self.url(&format!(
                "repos/{owner}/{repo}/pulls/{pull_number}/reviews?page={page}&per_page=100"
            ))?;
            let chunk: Vec<ar_forge::PullReviewSummary> = self
                .get_with_installation_token(url, installation_token)
                .await?;
            let chunk_len = chunk.len();
            all.extend(chunk.into_iter().map(|mut review| {
                review.state = normalize_review_state(&review.state).to_string();
                review
            }));
            if chunk_len < 100 {
                break;
            }
        }
        Ok(all)
    }

    pub async fn list_pull_review_comments(
        &self,
        owner: &str,
        repo: &str,
        pull_number: u64,
        review_id: u64,
        installation_token: &str,
    ) -> Result<Vec<ar_forge::PrReviewComment>, Error> {
        let mut all = Vec::new();
        for page in 1..=30 {
            let url = self.url(&format!(
                "repos/{owner}/{repo}/pulls/{pull_number}/reviews/{review_id}/comments?page={page}&per_page=100"
            ))?;
            let chunk: Vec<ar_forge::PrReviewComment> = self
                .get_with_installation_token(url, installation_token)
                .await?;
            let chunk_len = chunk.len();
            all.extend(chunk);
            if chunk_len < 100 {
                break;
            }
        }
        Ok(all)
    }

    pub async fn post_issue_comment(
        &self,
        owner: &str,
        repo: &str,
        issue_number: u64,
        body: &str,
        installation_token: &str,
    ) -> Result<u64, Error> {
        let url = self.url(&format!(
            "repos/{owner}/{repo}/issues/{issue_number}/comments"
        ))?;
        let created: CreatedIssueComment = self
            .post_with_installation_token(url, &CreateIssueComment { body }, installation_token)
            .await?;
        Ok(created.id)
    }

    pub async fn update_pull_request(
        &self,
        owner: &str,
        repo: &str,
        pull_number: u64,
        title: Option<&str>,
        body: Option<&str>,
        installation_token: &str,
    ) -> Result<(), Error> {
        let url = self.url(&format!("repos/{owner}/{repo}/pulls/{pull_number}"))?;
        self.patch_with_installation_token(
            url,
            &UpdatePullRequest { title, body },
            installation_token,
        )
        .await
    }

    pub async fn get_file_content(
        &self,
        owner: &str,
        repo: &str,
        file_path: &str,
        ref_: &str,
        installation_token: &str,
    ) -> Result<Option<String>, Error> {
        let url = self.url(&format!(
            "repos/{owner}/{repo}/contents/{file_path}?ref={ref_}"
        ))?;
        self.get_optional_text_with_installation_token(
            url,
            installation_token,
            HeaderValue::from_static("application/vnd.github.raw+json"),
        )
        .await
    }

    async fn get_with_installation_token<T>(
        &self,
        url: Url,
        installation_token: &str,
    ) -> Result<T, Error>
    where
        T: for<'de> Deserialize<'de>,
    {
        let auth_value = Self::installation_auth_header(installation_token)?;
        let response = self
            .http
            .get(url)
            .header(AUTHORIZATION, auth_value)
            .send()
            .await?;
        let status = response.status();
        let body = response.text().await?;
        if !status.is_success() {
            return Err(Error::Api {
                status: status.as_u16(),
                body: cap_for_error(&body),
            });
        }

        Ok(serde_json::from_str(&body)?)
    }

    async fn get_text_with_installation_token(
        &self,
        url: Url,
        installation_token: &str,
        accept: HeaderValue,
    ) -> Result<String, Error> {
        let auth_value = Self::installation_auth_header(installation_token)?;
        let response = self
            .http
            .get(url)
            .header(AUTHORIZATION, auth_value)
            .header(ACCEPT, accept)
            .send()
            .await?;
        let status = response.status();
        let body = response.text().await?;
        if !status.is_success() {
            return Err(Error::Api {
                status: status.as_u16(),
                body: cap_for_error(&body),
            });
        }

        Ok(body)
    }

    async fn get_optional_text_with_installation_token(
        &self,
        url: Url,
        installation_token: &str,
        accept: HeaderValue,
    ) -> Result<Option<String>, Error> {
        let auth_value = Self::installation_auth_header(installation_token)?;
        let response = self
            .http
            .get(url)
            .header(AUTHORIZATION, auth_value)
            .header(ACCEPT, accept)
            .send()
            .await?;
        let status = response.status();
        if status.as_u16() == 404 {
            return Ok(None);
        }
        let body = response.text().await?;
        if !status.is_success() {
            return Err(Error::Api {
                status: status.as_u16(),
                body: cap_for_error(&body),
            });
        }

        Ok(Some(body))
    }

    async fn post_with_installation_token<T, U>(
        &self,
        url: Url,
        body: &T,
        installation_token: &str,
    ) -> Result<U, Error>
    where
        T: Serialize,
        U: for<'de> Deserialize<'de>,
    {
        let auth_value = Self::installation_auth_header(installation_token)?;
        let response = self
            .http
            .post(url)
            .header(AUTHORIZATION, auth_value)
            .json(body)
            .send()
            .await?;
        let status = response.status();
        let body = response.text().await?;
        if !status.is_success() {
            return Err(Error::Api {
                status: status.as_u16(),
                body: cap_for_error(&body),
            });
        }

        Ok(serde_json::from_str(&body)?)
    }

    async fn patch_with_installation_token<T>(
        &self,
        url: Url,
        body: &T,
        installation_token: &str,
    ) -> Result<(), Error>
    where
        T: Serialize,
    {
        let auth_value = Self::installation_auth_header(installation_token)?;
        let response = self
            .http
            .patch(url)
            .header(AUTHORIZATION, auth_value)
            .json(body)
            .send()
            .await?;
        let status = response.status();
        let body = response.text().await?;
        if !status.is_success() {
            return Err(Error::Api {
                status: status.as_u16(),
                body: cap_for_error(&body),
            });
        }

        Ok(())
    }

    fn installation_auth_header(installation_token: &str) -> Result<HeaderValue, Error> {
        let mut auth_value = HeaderValue::from_str(&format!("Bearer {installation_token}"))
            .map_err(|_| {
                Error::InvalidHeader("installation token must be valid header text".to_string())
            })?;
        auth_value.set_sensitive(true);
        Ok(auth_value)
    }

    async fn fresh_cached_token(
        &self,
        key: &InstallationTokenCacheKey,
    ) -> Option<InstallationToken> {
        let now = OffsetDateTime::now_utc();
        self.installation_tokens
            .lock()
            .await
            .get(key)
            .filter(|cached| cached.expires_at - TOKEN_REFRESH_MARGIN > now)
            .map(|cached| cached.token.clone())
    }

    fn url(&self, path: &str) -> Result<Url, Error> {
        Ok(self.base.join(path.trim_start_matches('/'))?)
    }
}

#[derive(Debug, Serialize)]
struct GitHubCreateReviewRequest<'a> {
    body: &'a str,
    commit_id: &'a str,
    /// `None` leaves the review pending, which is how GitHub models a
    /// draft review (there is no `PENDING` event value).
    #[serde(skip_serializing_if = "Option::is_none")]
    event: Option<GitHubReviewEvent>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    comments: Vec<GitHubReviewComment<'a>>,
}

impl<'a> From<&'a ar_forge::CreateReviewRequest> for GitHubCreateReviewRequest<'a> {
    fn from(request: &'a ar_forge::CreateReviewRequest) -> Self {
        let comments = request
            .comments
            .iter()
            .filter_map(|comment| {
                // The provider-neutral DTO carries file line numbers, not
                // diff-relative offsets, so GitHub needs `line` + `side`
                // rather than the legacy `position` field.
                let (line, side) = match (comment.new_position, comment.old_position) {
                    (Some(line), _) => (line, DiffSide::Right),
                    (None, Some(line)) => (line, DiffSide::Left),
                    (None, None) => return None,
                };
                Some(GitHubReviewComment {
                    path: comment.path.as_str(),
                    line,
                    side,
                    body: comment.body.as_str(),
                })
            })
            .collect();
        Self {
            body: request.body.as_str(),
            commit_id: request.commit_id.as_str(),
            event: GitHubReviewEvent::from_review_event(request.event),
            comments,
        }
    }
}

#[derive(Debug, Serialize)]
struct GitHubReviewComment<'a> {
    path: &'a str,
    line: u32,
    side: DiffSide,
    body: &'a str,
}

/// GitHub's review `event` vocabulary differs from Forgejo's: approving is
/// `APPROVE` (not `APPROVED`) and pending reviews omit the field entirely.
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
enum GitHubReviewEvent {
    Approve,
    RequestChanges,
    Comment,
}

impl GitHubReviewEvent {
    fn from_review_event(event: ar_forge::ReviewEvent) -> Option<Self> {
        match event {
            ar_forge::ReviewEvent::Approved => Some(Self::Approve),
            ar_forge::ReviewEvent::RequestChanges => Some(Self::RequestChanges),
            ar_forge::ReviewEvent::Comment => Some(Self::Comment),
            ar_forge::ReviewEvent::Pending => None,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "UPPERCASE")]
enum DiffSide {
    Left,
    Right,
}

fn fold_inline_comments_into_body(request: &ar_forge::CreateReviewRequest) -> String {
    let mut body = request.body.clone();
    body.push_str("\n\n---\n\n**Inline comments that GitHub could not anchor to the diff:**");
    for comment in &request.comments {
        let location = match (comment.new_position, comment.old_position) {
            (Some(line), _) => format!(" line {line}"),
            (None, Some(line)) => format!(" old line {line}"),
            (None, None) => String::new(),
        };
        body.push_str(&format!(
            "\n\n#### `{}`{location}\n\n{}",
            comment.path, comment.body
        ));
    }
    body
}

/// GitHub's commit-status limit for `description`, in characters.
const MAX_GITHUB_STATUS_DESCRIPTION_CHARS: usize = 140;

#[derive(Debug, Serialize)]
struct GitHubCommitStatus<'a> {
    state: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    target_url: Option<&'a str>,
    description: String,
    context: &'a str,
}

impl<'a> From<&'a ar_forge::CommitStatus> for GitHubCommitStatus<'a> {
    fn from(status: &'a ar_forge::CommitStatus) -> Self {
        let state = match status.state {
            ar_forge::CommitStatusState::Pending => "pending",
            ar_forge::CommitStatusState::Success => "success",
            ar_forge::CommitStatusState::Error => "error",
            // GitHub has no `warning` state; keep it non-green.
            ar_forge::CommitStatusState::Failure | ar_forge::CommitStatusState::Warning => {
                "failure"
            }
        };
        let target_url = Some(status.target_url.as_str()).filter(|url| !url.trim().is_empty());
        Self {
            state,
            target_url,
            description: truncate_chars(&status.description, MAX_GITHUB_STATUS_DESCRIPTION_CHARS),
            context: status.context.as_str(),
        }
    }
}

fn truncate_chars(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let mut truncated: String = text.chars().take(max_chars.saturating_sub(1)).collect();
    truncated.push('…');
    truncated
}

#[derive(Debug, Serialize)]
struct CreateIssueComment<'a> {
    body: &'a str,
}

#[derive(Debug, Deserialize)]
struct CreatedIssueComment {
    id: u64,
}

#[derive(Debug, Serialize)]
struct UpdatePullRequest<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    title: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    body: Option<&'a str>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct InstallationTokenRequest {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    repositories: Vec<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    permissions: BTreeMap<String, Permission>,
}

impl InstallationTokenRequest {
    pub fn for_repository(repo: impl Into<String>) -> Self {
        Self {
            repositories: vec![repo.into()],
            permissions: BTreeMap::new(),
        }
    }

    pub fn with_permission(mut self, name: impl Into<String>, permission: Permission) -> Self {
        self.permissions.insert(name.into(), permission);
        self
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "lowercase")]
pub enum Permission {
    Read,
    Write,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct InstallationToken {
    pub token: String,
    pub expires_at: String,
}

#[derive(Debug, Clone)]
struct CachedInstallationToken {
    token: InstallationToken,
    expires_at: OffsetDateTime,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct InstallationTokenCacheKey {
    installation_id: u64,
    request: InstallationTokenRequest,
}

/// Map GitHub review states onto the provider-neutral vocabulary
/// (`REQUEST_CHANGES`, `COMMENT`, `APPROVED`, ...) shared with Forgejo.
fn normalize_review_state(state: &str) -> &str {
    match state {
        "CHANGES_REQUESTED" => "REQUEST_CHANGES",
        "COMMENTED" => "COMMENT",
        other => other,
    }
}

fn cap_for_error(body: &str) -> String {
    const MAX: usize = 1024;
    if body.len() <= MAX {
        body.to_string()
    } else {
        format!("{}...[truncated]", &body[..MAX])
    }
}

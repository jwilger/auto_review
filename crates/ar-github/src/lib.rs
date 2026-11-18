//! GitHub App REST client.

extern crate alloc;

mod app_host;
pub mod webhook;

#[expect(
    clippy::useless_attribute,
    reason = "clippy flags any lint attribute on a use item outside its fixed list, which omits \
              pub_use, yet the pub_use expectation below is what suppresses that lint here"
)]
#[expect(
    clippy::pub_use,
    reason = "ar-gateway (lib, startup) imports AppReviewHost from the crate root, and \
              app_host is a private module; dropping the re-export removes the type from the API"
)]
pub use app_host::{review_installation_token_request, AppReviewHost};

use alloc::collections::BTreeMap;
use async_trait::async_trait;
use core::time::Duration;
use jsonwebtoken::errors::Error as JwtError;
use jsonwebtoken::{Algorithm, EncodingKey, Header};
use reqwest::header::{HeaderMap, HeaderValue, ACCEPT, AUTHORIZATION, CONTENT_TYPE, USER_AGENT};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use thiserror::Error;
use time::error::Parse as TimestampParseError;
use time::format_description::well_known::Rfc3339;
use time::{Duration as TimeDuration, OffsetDateTime};
use tokio::sync::{Mutex, OnceCell};
use tokio::time::timeout;
use url::Url;

#[expect(
    clippy::useless_attribute,
    reason = "clippy flags any lint attribute on a use item outside its fixed list, which omits \
              pub_use, yet the pub_use expectation below is what suppresses that lint here"
)]
#[expect(
    clippy::pub_use,
    reason = "ar-gateway (github_webhook) and this crate's webhook_signature tests import these \
              webhook items from the crate root; dropping the re-exports breaks them"
)]
pub use webhook::{
    verify_webhook_signature, InstallationRef, IssueCommentEvent, PullRequestReviewCommentEvent,
    WebhookComment, WebhookIssue, WebhookPullRequestRef, WebhookRepository, WebhookSignatureError,
    WebhookUser,
};

/// Value of the `X-GitHub-Api-Version` header sent on every request.
const GITHUB_API_VERSION: &str = "2022-11-28";

/// Default bound on the `GET /app` request behind
/// [`ar_forge::ReviewHost::bot_login`]. Callers share one cached result, so a
/// server that accepts the connection but never answers must not block them
/// all indefinitely.
pub const IDENTITY_TIMEOUT: Duration = Duration::from_secs(10);

/// GitHub's commit-status limit for `description`, in characters.
const MAX_GITHUB_STATUS_DESCRIPTION_CHARS: usize = 140;

/// Most pages (of 100 rows) a paginated list call fetches.
const MAX_LIST_PAGES: u32 = 30;

/// Cached installation tokens are refreshed this long before they expire.
const TOKEN_REFRESH_MARGIN: TimeDuration = TimeDuration::minutes(5);

#[expect(
    clippy::error_impl_error,
    reason = "ar-cli (commands), ar-gateway and this crate's integration tests name this type \
              ar_github::Error; renaming it breaks them"
)]
#[derive(Debug, Error)]
pub enum Error {
    #[error("API error {status}: {body}")]
    Api { status: u16, body: String },
    #[error("failed to set clone credentials on URL")]
    CloneCredentialEncoding,
    #[error("HTTP error: {0}")]
    Http(#[from] reqwest::Error),
    #[error("invalid base URL: {0}")]
    InvalidBaseUrl(String),
    #[error("invalid expires_at timestamp: {0}")]
    InvalidExpiresAt(#[from] TimestampParseError),
    #[error("invalid header value: {0}")]
    InvalidHeader(String),
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("JWT error: {0}")]
    Jwt(#[from] JwtError),
    #[error("request timed out after {0:?}")]
    Timeout(Duration),
    #[error("URL build error: {0}")]
    UrlBuild(#[from] url::ParseError),
}

/// A [`ar_forge::ReviewHost`] bound to one already-minted installation token,
/// suited to a single CI invocation.
#[derive(Debug)]
pub struct InstallationReviewHost {
    /// The bot login, resolved once from `GET /app` or seeded by the caller.
    bot_login: OnceCell<String>,
    /// REST client used for every call.
    client: Client,
    /// Bound on the `GET /app` identity request.
    identity_timeout: Duration,
    /// Installation token authenticating every repository call.
    installation_token: String,
}

impl InstallationReviewHost {
    /// Wrap `client` with an installation token for repository calls.
    #[inline]
    pub fn new(client: Client, installation_token: impl Into<String>) -> Self {
        Self {
            client,
            installation_token: installation_token.into(),
            bot_login: OnceCell::new(),
            identity_timeout: IDENTITY_TIMEOUT,
        }
    }

    /// Seed a bot login already known to the caller, so
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
impl ar_forge::ReviewHost for InstallationReviewHost {
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
        let clone_url = self
            .client
            .get_repository_clone_url(owner, repo, &self.installation_token)
            .await
            .map_err(|error| host_error(&error))?;
        with_clone_credentials(&clone_url, &self.installation_token)
            .map_err(|error| host_error(&error))
    }

    #[inline]
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
        self.client
            .edit_issue_comment(owner, repo, comment_id, body, &self.installation_token)
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
        self.client
            .edit_review_comment(owner, repo, comment_id, body, &self.installation_token)
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
        self.client
            .get_compare_diff(owner, repo, base, head, &self.installation_token)
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
        self.client
            .get_file_content(owner, repo, file_path, ref_, &self.installation_token)
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
        self.client
            .get_issue(owner, repo, issue_number, &self.installation_token)
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
        self.client
            .get_pull_request_diff(owner, repo, pr_number, &self.installation_token)
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
        self.client
            .get_pull_request(owner, repo, pr_number, &self.installation_token)
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
        self.client
            .list_changed_files(owner, repo, pr_number, &self.installation_token)
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
        self.client
            .list_pr_review_comments(owner, repo, pr_number, &self.installation_token)
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
        self.client
            .list_pull_review_comments(owner, repo, pr_number, review_id, &self.installation_token)
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
        self.client
            .list_pull_reviews(owner, repo, pr_number, &self.installation_token)
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
        self.client
            .post_commit_status(owner, repo, sha, status, &self.installation_token)
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
        self.client
            .post_issue_comment(owner, repo, issue_number, body, &self.installation_token)
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
            .map_err(|error| host_error(&error))
    }
}

/// Signs GitHub App JWTs with the app's RSA private key.
#[derive(Debug, Clone)]
pub struct GitHubAppJwt {
    /// The GitHub App id, used as the JWT issuer.
    app_id: u64,
    /// The app's RSA private key.
    key: EncodingKey,
}

impl GitHubAppJwt {
    /// Load the signer from the app id and its PEM-encoded RSA private key.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Jwt`] when the PEM is not a valid RSA private key.
    #[inline]
    pub fn from_rsa_pem(app_id: u64, private_key_pem: &[u8]) -> Result<Self, Error> {
        let key = EncodingKey::from_rsa_pem(private_key_pem)?;
        Ok(Self { app_id, key })
    }

    /// Sign an app JWT issued at `issued_at` (Unix seconds) and valid for
    /// ten minutes.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Jwt`] when signing fails.
    #[inline]
    pub fn jwt_at_unix(&self, issued_at: i64) -> Result<String, Error> {
        /// JWT claims GitHub requires for app authentication.
        #[expect(
            clippy::arbitrary_source_item_ordering,
            reason = "field order sets the JSON key order of the signed claims, so reordering \
                      changes the issued JWT"
        )]
        #[derive(Serialize)]
        struct Claims {
            /// Issuer: the app id.
            iss: String,
            /// Issued-at time, Unix seconds.
            iat: i64,
            /// Expiry time, Unix seconds.
            exp: i64,
        }

        let claims = Claims {
            iss: self.app_id.to_string(),
            iat: issued_at,
            exp: issued_at.saturating_add(600),
        };
        let header = Header::new(Algorithm::RS256);
        Ok(jsonwebtoken::encode(&header, &claims, &self.key)?)
    }

    /// Sign an app JWT issued now.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Jwt`] when signing fails.
    #[inline]
    pub fn jwt_now(&self) -> Result<String, Error> {
        self.jwt_at_unix(OffsetDateTime::now_utc().unix_timestamp())
    }
}

/// How app-level (JWT-authenticated) calls authenticate.
#[derive(Debug, Clone)]
enum AppAuth {
    /// Mint a fresh JWT per app-level call. Required by long-running
    /// processes such as the gateway.
    Signer(GitHubAppJwt),
    /// A pre-minted JWT. Valid for at most ten minutes, so only suitable
    /// for short-lived clients such as a single CI invocation.
    Static(String),
}

impl AppAuth {
    /// The `Authorization` header value for an app-level call, marked
    /// sensitive.
    fn bearer(&self) -> Result<HeaderValue, Error> {
        #[expect(
            clippy::pattern_type_mismatch,
            reason = "matching the borrowed enum binds its fields by reference; the alternative \
                      `match *self` with `ref` bindings trips the ref_patterns restriction lint"
        )]
        let jwt = match self {
            Self::Signer(signer) => signer.jwt_now()?,
            Self::Static(jwt) => jwt.clone(),
        };
        #[expect(
            clippy::map_err_ignore,
            reason = "InvalidHeaderValue carries no detail, and the error must not echo the JWT"
        )]
        let mut value = HeaderValue::from_str(&format!("Bearer {jwt}"))
            .map_err(|_| Error::InvalidHeader("app JWT must be valid header text".to_owned()))?;
        value.set_sensitive(true);
        Ok(value)
    }
}

/// GitHub REST client authenticating as a GitHub App.
#[derive(Debug)]
pub struct Client {
    /// How app-level calls authenticate.
    app_auth: AppAuth,
    /// API base URL, always ending in `/`.
    base: Url,
    /// HTTP client carrying the GitHub default headers.
    http: reqwest::Client,
    /// Installation tokens reused until shortly before they expire.
    installation_tokens: Mutex<HashMap<InstallationTokenCacheKey, CachedInstallationToken>>,
}

/// The authenticated GitHub App, as returned by `GET /app`.
#[expect(
    clippy::exhaustive_structs,
    reason = "ar-gateway's startup tests build AppInfo by struct literal, which \
              #[non_exhaustive] would forbid"
)]
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct AppInfo {
    pub id: u64,
    #[serde(default)]
    pub name: String,
    pub slug: String,
}

impl AppInfo {
    /// Login GitHub shows for comments and reviews authored by this app.
    #[inline]
    #[must_use = "computing the login has no side effect"]
    pub fn bot_login(&self) -> String {
        format!("{}[bot]", self.slug)
    }
}

impl Client {
    /// Build a client with the GitHub default headers for `base_url`.
    fn build(base_url: &str, app_auth: AppAuth) -> Result<Self, Error> {
        let normalized = if base_url.ends_with('/') {
            base_url.to_owned()
        } else {
            format!("{base_url}/")
        };
        #[expect(
            clippy::map_err_ignore,
            reason = "InvalidBaseUrl reports the rejected URL itself; its Display text stays as \
                      callers and logs already see it"
        )]
        let base =
            Url::parse(&normalized).map_err(|_| Error::InvalidBaseUrl(normalized.clone()))?;

        let mut headers = HeaderMap::new();
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
            app_auth,
            installation_tokens: Mutex::new(HashMap::new()),
        })
    }

    /// Builds the `contents` API URL with every path segment and the `ref`
    /// query value percent-encoded, so branch names such as `feature/a&b`
    /// or file paths with reserved characters cannot alter the request.
    fn contents_url(
        &self,
        owner: &str,
        repo: &str,
        file_path: &str,
        ref_: &str,
    ) -> Result<Url, Error> {
        let mut url = self.base.clone();
        url.path_segments_mut()
            .map_err(|()| Error::InvalidBaseUrl(self.base.to_string()))?
            .pop_if_empty()
            .extend(["repos", owner, repo, "contents"])
            .extend(file_path.split('/').filter(|segment| !segment.is_empty()));
        url.query_pairs_mut().append_pair("ref", ref_);
        Ok(url)
    }

    /// Create a pull request review. When GitHub rejects an inline comment
    /// outside the diff (422), the review is retried with the inline
    /// comments folded into its body.
    ///
    /// # Errors
    ///
    /// Returns an error when the request fails, GitHub answers with a
    /// non-success status, or the response does not decode.
    #[inline]
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
            #[expect(
                clippy::rest_pattern_accessible_field,
                reason = "only the 422 status decides the retry; the error body is irrelevant, \
                          and naming it as `body: _` trips unneeded_field_pattern instead"
            )]
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

    /// Replace the body of a top-level issue/PR comment. Callers must
    /// pass only ids of comments the bot authored (THREAT-MODEL.md T9).
    ///
    /// # Errors
    ///
    /// Returns an error when the request fails or GitHub answers with a
    /// non-success status.
    #[inline]
    pub async fn edit_issue_comment(
        &self,
        owner: &str,
        repo: &str,
        comment_id: u64,
        body: &str,
        installation_token: &str,
    ) -> Result<(), Error> {
        let url = self.url(&format!(
            "repos/{owner}/{repo}/issues/comments/{comment_id}"
        ))?;
        self.patch_with_installation_token(url, &CreateIssueComment { body }, installation_token)
            .await
    }

    /// Replace the body of an inline pull request review comment.
    /// Callers must pass only ids of comments the bot authored
    /// (THREAT-MODEL.md T9).
    ///
    /// # Errors
    ///
    /// Returns an error when the request fails or GitHub answers with a
    /// non-success status.
    #[inline]
    pub async fn edit_review_comment(
        &self,
        owner: &str,
        repo: &str,
        comment_id: u64,
        body: &str,
        installation_token: &str,
    ) -> Result<(), Error> {
        let url = self.url(&format!("repos/{owner}/{repo}/pulls/comments/{comment_id}"))?;
        self.patch_with_installation_token(url, &CreateIssueComment { body }, installation_token)
            .await
    }

    /// The cached token for `key`, when it is not within
    /// [`TOKEN_REFRESH_MARGIN`] of expiring.
    async fn fresh_cached_token(
        &self,
        key: &InstallationTokenCacheKey,
    ) -> Option<InstallationToken> {
        let now = OffsetDateTime::now_utc();
        self.installation_tokens
            .lock()
            .await
            .get(key)
            .filter(|cached| {
                cached
                    .expires_at
                    .checked_sub(TOKEN_REFRESH_MARGIN)
                    .is_some_and(|refresh_at| refresh_at > now)
            })
            .map(|cached| cached.token.clone())
    }

    /// `GET /app`: identify the authenticated GitHub App.
    ///
    /// # Errors
    ///
    /// Returns an error when the app JWT cannot be produced, the request
    /// fails, GitHub answers with a non-success status, or the response does
    /// not decode.
    #[inline]
    pub async fn get_authenticated_app(&self) -> Result<AppInfo, Error> {
        let url = self.url("app")?;
        self.get_with_app_auth(url).await
    }

    /// Fetch the unified diff between two commits.
    ///
    /// # Errors
    ///
    /// Returns an error when the request fails or GitHub answers with a
    /// non-success status.
    #[inline]
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

    /// Fetch a file's raw contents from the repo at `ref_` (a branch, tag, or
    /// commit SHA). Returns `Ok(None)` when the file does not exist (404).
    ///
    /// This is the transport for the trusted base-branch loader
    /// (`ar_review::trusted_files`): both the review dispatcher and the chat
    /// handler read `.auto_review.yaml` through it at the PR's base branch,
    /// never from the head checkout. The whole body is read into memory; the
    /// loader's size cap applies after the read.
    ///
    /// # Errors
    ///
    /// Returns an error when the request fails or GitHub answers with a
    /// non-success status other than 404.
    #[inline]
    pub async fn get_file_content(
        &self,
        owner: &str,
        repo: &str,
        file_path: &str,
        ref_: &str,
        installation_token: &str,
    ) -> Result<Option<String>, Error> {
        let url = self.contents_url(owner, repo, file_path, ref_)?;
        self.get_optional_text_with_installation_token(
            url,
            installation_token,
            HeaderValue::from_static("application/vnd.github.raw+json"),
        )
        .await
    }

    /// Fetch an issue by number with the installation token. GitHub serves
    /// pull requests on the issues endpoint and marks them with a
    /// `pull_request` object, which becomes `is_pull_request`. Returns
    /// `Ok(None)` on 404; every other non-success status is an error whose
    /// body is capped on a UTF-8 char boundary.
    ///
    /// # Errors
    ///
    /// Returns an error when the request fails, GitHub answers with a
    /// non-success status other than 404, or the response does not decode.
    #[inline]
    pub async fn get_issue(
        &self,
        owner: &str,
        repo: &str,
        issue_number: u64,
        installation_token: &str,
    ) -> Result<Option<ar_forge::IssueSummary>, Error> {
        /// The fields of GitHub's issue response that the summary keeps.
        #[derive(Deserialize)]
        struct RawIssue {
            /// Issue body; GitHub sends `null` for an empty body.
            #[serde(default)]
            body: Option<String>,
            /// Browser URL of the issue.
            #[serde(default)]
            html_url: String,
            /// Issue number.
            number: u64,
            /// Present (and non-null) when the issue is a pull request.
            #[serde(default)]
            pull_request: Option<serde_json::Value>,
            /// `open` or `closed`.
            state: String,
            /// Issue title.
            title: String,
        }
        let url = self.url(&format!("repos/{owner}/{repo}/issues/{issue_number}"))?;
        let auth_value = Self::installation_auth_header(installation_token)?;
        let response = self
            .http
            .get(url)
            .header(AUTHORIZATION, auth_value)
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
        let raw: RawIssue = serde_json::from_str(&body)?;
        Ok(Some(ar_forge::IssueSummary {
            number: raw.number,
            title: raw.title,
            body: raw.body.unwrap_or_default(),
            state: raw.state,
            is_pull_request: raw.pull_request.is_some_and(|value| !value.is_null()),
            html_url: raw.html_url,
        }))
    }

    /// `GET` `url` with the installation token and `accept`, returning the
    /// body as text, or `None` on 404.
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

    /// Fetch a pull request.
    ///
    /// # Errors
    ///
    /// Returns an error when the request fails, GitHub answers with a
    /// non-success status, or the response does not decode.
    #[inline]
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

    /// Fetch a pull request's unified diff.
    ///
    /// GitHub refuses the diff media type with `406 too_large` for a pull
    /// request over its diff limits (300 files, or too many lines or
    /// bytes). The diff is then assembled from the per-file `patch` fields
    /// of `pulls/{n}/files` (see [`Self::list_pull_request_files`]); a file
    /// GitHub sends no patch for (binary, or too large) appears as a
    /// header-only entry, so it still counts as changed but carries no
    /// content.
    ///
    /// # Errors
    ///
    /// Returns an error when the request fails or GitHub answers with a
    /// non-success status (other than the `too_large` refusal, when the
    /// file list is fetched instead and its errors are returned).
    #[inline]
    pub async fn get_pull_request_diff(
        &self,
        owner: &str,
        repo: &str,
        pull_number: u64,
        installation_token: &str,
    ) -> Result<String, Error> {
        let url = self.url(&format!("repos/{owner}/{repo}/pulls/{pull_number}"))?;
        match self
            .get_text_with_installation_token(
                url,
                installation_token,
                HeaderValue::from_static("application/vnd.github.v3.diff"),
            )
            .await
        {
            Err(error) if is_diff_too_large(&error) => {
                let files = self
                    .list_pull_request_files(owner, repo, pull_number, installation_token)
                    .await?;
                Ok(diff_from_file_patches(&files))
            }
            other => other,
        }
    }

    /// Fetch the repository's HTTPS `clone_url`. The REST API host is not
    /// the git host on github.com or GitHub Enterprise Server.
    ///
    /// # Errors
    ///
    /// Returns an error when the request fails, GitHub answers with a
    /// non-success status, or the response does not decode.
    #[inline]
    pub async fn get_repository_clone_url(
        &self,
        owner: &str,
        repo: &str,
        installation_token: &str,
    ) -> Result<String, Error> {
        /// The field of GitHub's repository response that is kept.
        #[derive(Deserialize)]
        struct Repository {
            /// HTTPS clone URL.
            clone_url: String,
        }
        let url = self.url(&format!("repos/{owner}/{repo}"))?;
        let repository: Repository = self
            .get_with_installation_token(url, installation_token)
            .await?;
        Ok(repository.clone_url)
    }

    /// `GET /repos/{owner}/{repo}/installation`: find the app installation
    /// that covers a repository.
    ///
    /// # Errors
    ///
    /// Returns an error when the app JWT cannot be produced, the request
    /// fails, GitHub answers with a non-success status, or the response does
    /// not decode.
    #[inline]
    pub async fn get_repository_installation_id(
        &self,
        owner: &str,
        repo: &str,
    ) -> Result<u64, Error> {
        /// The field of GitHub's installation response that is kept.
        #[derive(Deserialize)]
        struct Installation {
            /// Installation id.
            id: u64,
        }
        let url = self.url(&format!("repos/{owner}/{repo}/installation"))?;
        let installation: Installation = self.get_with_app_auth(url).await?;
        Ok(installation.id)
    }

    /// `GET` `url` with the installation token and `accept`, returning the
    /// body as text.
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

    /// `GET` `url` with an app JWT and decode the JSON response.
    async fn get_with_app_auth<T>(&self, url: Url) -> Result<T, Error>
    where
        T: for<'de> Deserialize<'de>,
    {
        let response = self
            .http
            .get(url)
            .header(AUTHORIZATION, self.app_auth.bearer()?)
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

    /// `GET` `url` with the installation token and decode the JSON response.
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

    /// The `Authorization` header value for an installation token, marked
    /// sensitive.
    fn installation_auth_header(installation_token: &str) -> Result<HeaderValue, Error> {
        #[expect(
            clippy::map_err_ignore,
            reason = "InvalidHeaderValue carries no detail, and the error must not echo the token"
        )]
        let mut auth_value = HeaderValue::from_str(&format!("Bearer {installation_token}"))
            .map_err(|_| {
                Error::InvalidHeader("installation token must be valid header text".to_owned())
            })?;
        auth_value.set_sensitive(true);
        Ok(auth_value)
    }

    /// Mint (or reuse a cached) installation access token.
    ///
    /// # Errors
    ///
    /// Returns an error when the app JWT cannot be produced, the request
    /// fails, GitHub answers with a non-success status, the response does
    /// not decode, or its `expires_at` is not RFC 3339.
    #[inline]
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
        let response = self
            .http
            .post(url)
            .header(AUTHORIZATION, self.app_auth.bearer()?)
            .json(&request)
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

        let token: InstallationToken = serde_json::from_str(&body)?;
        let expires_at = OffsetDateTime::parse(&token.expires_at, &Rfc3339)?;
        let cached = CachedInstallationToken {
            token: token.clone(),
            expires_at,
        };
        self.installation_tokens.lock().await.insert(key, cached);
        Ok(token)
    }

    /// List every file a pull request changes, up to 30 pages of 100.
    ///
    /// # Errors
    ///
    /// Returns an error when a request fails, GitHub answers with a
    /// non-success status, or a page does not decode.
    #[inline]
    pub async fn list_changed_files(
        &self,
        owner: &str,
        repo: &str,
        pull_number: u64,
        installation_token: &str,
    ) -> Result<Vec<ar_forge::ChangedFile>, Error> {
        let mut all = Vec::new();
        for page in 1..=MAX_LIST_PAGES {
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

    /// List a pull request's conversation comments, up to 30 pages of 100.
    ///
    /// # Errors
    ///
    /// Returns an error when a request fails, GitHub answers with a
    /// non-success status, or a page does not decode.
    #[inline]
    pub async fn list_pr_review_comments(
        &self,
        owner: &str,
        repo: &str,
        pull_number: u64,
        installation_token: &str,
    ) -> Result<Vec<ar_forge::PrReviewComment>, Error> {
        let mut all = Vec::new();
        for page in 1..=MAX_LIST_PAGES {
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

    /// List every file a pull request changes with its `patch` and, for a
    /// rename, its previous path, up to 30 pages of 100 (GitHub's own cap is
    /// 3000 files).
    ///
    /// # Errors
    ///
    /// Returns an error when a request fails, GitHub answers with a
    /// non-success status, or a page does not decode.
    async fn list_pull_request_files(
        &self,
        owner: &str,
        repo: &str,
        pull_number: u64,
        installation_token: &str,
    ) -> Result<Vec<PullRequestFile>, Error> {
        let mut all = Vec::new();
        for page in 1..=MAX_LIST_PAGES {
            let url = self.url(&format!(
                "repos/{owner}/{repo}/pulls/{pull_number}/files?page={page}&per_page=100"
            ))?;
            let chunk: Vec<PullRequestFile> = self
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

    /// List the inline comments of one pull request review, up to 30 pages
    /// of 100.
    ///
    /// # Errors
    ///
    /// Returns an error when a request fails, GitHub answers with a
    /// non-success status, or a page does not decode.
    #[inline]
    pub async fn list_pull_review_comments(
        &self,
        owner: &str,
        repo: &str,
        pull_number: u64,
        review_id: u64,
        installation_token: &str,
    ) -> Result<Vec<ar_forge::PrReviewComment>, Error> {
        let mut all = Vec::new();
        for page in 1..=MAX_LIST_PAGES {
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

    /// List a pull request's reviews, up to 30 pages of 100, with review
    /// states mapped onto the provider-neutral vocabulary.
    ///
    /// # Errors
    ///
    /// Returns an error when a request fails, GitHub answers with a
    /// non-success status, or a page does not decode.
    #[inline]
    pub async fn list_pull_reviews(
        &self,
        owner: &str,
        repo: &str,
        pull_number: u64,
        installation_token: &str,
    ) -> Result<Vec<ar_forge::PullReviewSummary>, Error> {
        let mut all = Vec::new();
        for page in 1..=MAX_LIST_PAGES {
            let url = self.url(&format!(
                "repos/{owner}/{repo}/pulls/{pull_number}/reviews?page={page}&per_page=100"
            ))?;
            let chunk: Vec<ar_forge::PullReviewSummary> = self
                .get_with_installation_token(url, installation_token)
                .await?;
            let chunk_len = chunk.len();
            all.extend(chunk.into_iter().map(|mut review| {
                review.state = normalize_review_state(&review.state).to_owned();
                review
            }));
            if chunk_len < 100 {
                break;
            }
        }
        Ok(all)
    }

    /// Build a client that authenticates app-level calls with a pre-minted
    /// JWT, suitable for a short-lived process.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidHeader`] when the JWT is not valid header
    /// text, and [`Error::InvalidBaseUrl`] or [`Error::Http`] when the client
    /// cannot be built.
    #[inline]
    pub fn new(base_url: &str, app_jwt: &str) -> Result<Self, Error> {
        let app_auth = AppAuth::Static(app_jwt.to_owned());
        app_auth.bearer()?;
        Self::build(base_url, app_auth)
    }

    /// `PATCH` `url` with a JSON body and the installation token, discarding
    /// the response body.
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
        let response_body = response.text().await?;
        if !status.is_success() {
            return Err(Error::Api {
                status: status.as_u16(),
                body: cap_for_error(&response_body),
            });
        }

        Ok(())
    }

    /// Post a commit status on `sha`.
    ///
    /// # Errors
    ///
    /// Returns an error when the request fails, GitHub answers with a
    /// non-success status, or the response is not JSON.
    #[inline]
    pub async fn post_commit_status(
        &self,
        owner: &str,
        repo: &str,
        sha: &str,
        status: &ar_forge::CommitStatus,
        installation_token: &str,
    ) -> Result<(), Error> {
        let url = self.url(&format!("repos/{owner}/{repo}/statuses/{sha}"))?;
        let github_status = GitHubCommitStatus::from(status);
        let _: serde_json::Value = self
            .post_with_installation_token(url, &github_status, installation_token)
            .await?;
        Ok(())
    }

    /// Post a top-level comment on an issue or pull request, returning the
    /// new comment's id.
    ///
    /// # Errors
    ///
    /// Returns an error when the request fails, GitHub answers with a
    /// non-success status, or the response does not decode.
    #[inline]
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

    /// `POST` `url` with a JSON body and the installation token, decoding the
    /// JSON response.
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
        let response_body = response.text().await?;
        if !status.is_success() {
            return Err(Error::Api {
                status: status.as_u16(),
                body: cap_for_error(&response_body),
            });
        }

        Ok(serde_json::from_str(&response_body)?)
    }

    /// Update a pull request's title and/or body; `None` leaves a field
    /// unchanged.
    ///
    /// # Errors
    ///
    /// Returns an error when the request fails or GitHub answers with a
    /// non-success status.
    #[inline]
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

    /// Resolve an API path against the base URL.
    fn url(&self, path: &str) -> Result<Url, Error> {
        Ok(self.base.join(path.trim_start_matches('/'))?)
    }

    /// Build a client that signs a fresh app JWT for every app-level call,
    /// so it stays usable for the lifetime of a long-running service.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidBaseUrl`] or [`Error::Http`] when the client
    /// cannot be built.
    #[inline]
    pub fn with_app_signer(base_url: &str, signer: GitHubAppJwt) -> Result<Self, Error> {
        Self::build(base_url, AppAuth::Signer(signer))
    }
}

/// GitHub's create-review request body.
#[expect(
    clippy::arbitrary_source_item_ordering,
    reason = "field order sets the JSON key order of the create-review request body"
)]
#[derive(Debug, Serialize)]
struct GitHubCreateReviewRequest<'req> {
    /// Review summary body.
    body: &'req str,
    /// Commit the review applies to.
    commit_id: &'req str,
    /// `None` leaves the review pending, which is how GitHub models a
    /// draft review (there is no `PENDING` event value).
    #[serde(skip_serializing_if = "Option::is_none")]
    event: Option<GitHubReviewEvent>,
    /// Inline comments anchored by line and side.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    comments: Vec<GitHubReviewComment<'req>>,
}

impl<'req> From<&'req ar_forge::CreateReviewRequest> for GitHubCreateReviewRequest<'req> {
    #[inline]
    fn from(request: &'req ar_forge::CreateReviewRequest) -> Self {
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

/// One inline comment in GitHub's create-review request body.
#[expect(
    clippy::arbitrary_source_item_ordering,
    reason = "field order sets the JSON key order of each inline review comment"
)]
#[derive(Debug, Serialize)]
struct GitHubReviewComment<'req> {
    /// File path the comment is anchored to.
    path: &'req str,
    /// File line number the comment is anchored to.
    line: u32,
    /// Which side of the diff `line` refers to.
    side: DiffSide,
    /// Comment body.
    body: &'req str,
}

/// GitHub's review `event` vocabulary differs from Forgejo's: approving is
/// `APPROVE` (not `APPROVED`) and pending reviews omit the field entirely.
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
enum GitHubReviewEvent {
    /// Approve the pull request.
    Approve,
    /// Comment without approving or requesting changes.
    Comment,
    /// Request changes.
    RequestChanges,
}

impl GitHubReviewEvent {
    /// GitHub's event for a provider-neutral review event; `None` for a
    /// pending review.
    const fn from_review_event(event: ar_forge::ReviewEvent) -> Option<Self> {
        match event {
            ar_forge::ReviewEvent::Approved => Some(Self::Approve),
            ar_forge::ReviewEvent::RequestChanges => Some(Self::RequestChanges),
            ar_forge::ReviewEvent::Comment => Some(Self::Comment),
            ar_forge::ReviewEvent::Pending => None,
        }
    }
}

/// Side of the diff an inline comment's line refers to.
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "UPPERCASE")]
enum DiffSide {
    /// The base (old) side.
    Left,
    /// The head (new) side.
    Right,
}

/// GitHub's create-commit-status request body.
#[expect(
    clippy::arbitrary_source_item_ordering,
    reason = "field order sets the JSON key order of the commit-status request body"
)]
#[derive(Debug, Serialize)]
struct GitHubCommitStatus<'status> {
    /// `pending`, `success`, `error` or `failure`.
    state: &'static str,
    /// Details link; omitted when blank.
    #[serde(skip_serializing_if = "Option::is_none")]
    target_url: Option<&'status str>,
    /// Description, truncated to GitHub's limit.
    description: String,
    /// Status context label.
    context: &'status str,
}

impl<'status> From<&'status ar_forge::CommitStatus> for GitHubCommitStatus<'status> {
    #[inline]
    fn from(status: &'status ar_forge::CommitStatus) -> Self {
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

/// Request body for creating or editing an issue comment.
#[derive(Debug, Serialize)]
struct CreateIssueComment<'body> {
    /// Comment body.
    body: &'body str,
}

/// The field of GitHub's created-comment response that is kept.
#[derive(Debug, Deserialize)]
struct CreatedIssueComment {
    /// Id of the created comment.
    id: u64,
}

/// Request body for updating a pull request.
#[expect(
    clippy::arbitrary_source_item_ordering,
    reason = "field order sets the JSON key order of the update-pull-request request body"
)]
#[derive(Debug, Serialize)]
struct UpdatePullRequest<'body> {
    /// New title; omitted to leave it unchanged.
    #[serde(skip_serializing_if = "Option::is_none")]
    title: Option<&'body str>,
    /// New body; omitted to leave it unchanged.
    #[serde(skip_serializing_if = "Option::is_none")]
    body: Option<&'body str>,
}

/// Scope of an installation access token request.
#[expect(
    clippy::arbitrary_source_item_ordering,
    reason = "field order sets the JSON key order of the access-token request body"
)]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct InstallationTokenRequest {
    /// Repository names the token is restricted to; empty means all.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    repositories: Vec<String>,
    /// Permission levels requested, by permission name.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    permissions: BTreeMap<String, Permission>,
}

impl InstallationTokenRequest {
    /// A request restricted to the repository named `repo`.
    #[inline]
    pub fn for_repository(repo: impl Into<String>) -> Self {
        Self {
            repositories: vec![repo.into()],
            permissions: BTreeMap::new(),
        }
    }

    /// Request `permission` for the permission named `name`.
    #[inline]
    #[must_use = "the permission is added only to the returned request"]
    pub fn with_permission(mut self, name: impl Into<String>, permission: Permission) -> Self {
        self.permissions.insert(name.into(), permission);
        self
    }
}

/// Access level requested for an installation token permission.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "lowercase")]
pub enum Permission {
    Read,
    Write,
}

/// An installation access token and its expiry.
#[expect(
    clippy::exhaustive_structs,
    reason = "a token/expiry pair whose public fields are its whole contract; ar-github keeps \
              its public types exhaustive so workspace crates can build them by struct literal"
)]
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct InstallationToken {
    pub expires_at: String,
    pub token: String,
}

/// A cached installation token with its parsed expiry.
#[derive(Debug, Clone)]
struct CachedInstallationToken {
    /// When the token expires.
    expires_at: OffsetDateTime,
    /// The token as GitHub returned it.
    token: InstallationToken,
}

/// Cache key: a token is reused only for the same installation and scope.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct InstallationTokenCacheKey {
    /// Installation the token belongs to.
    installation_id: u64,
    /// Repository and permission scope of the token.
    request: InstallationTokenRequest,
}

/// The fields of one `pulls/{n}/files` entry that the diff fallback needs.
#[derive(Debug, Deserialize)]
struct PullRequestFile {
    /// Path on the head side.
    filename: String,
    /// Unified-diff hunks; absent for binary files and files too large for
    /// GitHub to diff.
    #[serde(default)]
    patch: Option<String>,
    /// Path on the base side, for a renamed file.
    #[serde(default)]
    previous_filename: Option<String>,
    /// `added`, `removed`, `modified`, `renamed`, `copied`, `changed` or
    /// `unchanged`.
    status: String,
}

/// Build an authenticated HTTPS clone URL for `owner/repo` under `base_url`.
///
/// # Errors
///
/// Returns [`Error::InvalidBaseUrl`] when `base_url` does not parse and
/// [`Error::CloneCredentialEncoding`] when it cannot carry credentials.
#[inline]
pub fn build_clone_url(
    base_url: &str,
    owner: &str,
    repo: &str,
    installation_token: &str,
) -> Result<String, Error> {
    #[expect(
        clippy::map_err_ignore,
        reason = "InvalidBaseUrl reports the rejected URL itself; its Display text stays as \
                  callers and logs already see it"
    )]
    let mut url = Url::parse(base_url).map_err(|_| Error::InvalidBaseUrl(base_url.to_owned()))?;
    let path = {
        let trimmed = url.path().trim_end_matches('/');
        format!("{trimmed}/{owner}/{repo}.git")
    };
    url.set_path(&path);
    url.set_username("x-access-token")
        .map_err(|()| Error::CloneCredentialEncoding)?;
    url.set_password(Some(installation_token))
        .map_err(|()| Error::CloneCredentialEncoding)?;
    Ok(url.to_string())
}

/// Add installation-token userinfo to a repository's HTTPS clone URL.
///
/// # Errors
///
/// Returns [`Error::InvalidBaseUrl`] when `clone_url` does not parse and
/// [`Error::CloneCredentialEncoding`] when it cannot carry credentials.
#[inline]
pub fn with_clone_credentials(clone_url: &str, installation_token: &str) -> Result<String, Error> {
    #[expect(
        clippy::map_err_ignore,
        reason = "InvalidBaseUrl reports the rejected URL itself; its Display text stays as \
                  callers and logs already see it"
    )]
    let mut url = Url::parse(clone_url).map_err(|_| Error::InvalidBaseUrl(clone_url.to_owned()))?;
    url.set_username("x-access-token")
        .map_err(|()| Error::CloneCredentialEncoding)?;
    url.set_password(Some(installation_token))
        .map_err(|()| Error::CloneCredentialEncoding)?;
    Ok(url.to_string())
}

/// Cap a response body for inclusion in [`Error::Api`], cutting on a UTF-8
/// char boundary so a multi-byte character straddling the cap cannot panic
/// the slice.
fn cap_for_error(body: &str) -> String {
    /// Largest body prefix, in bytes, kept in an error message.
    const MAX: usize = 1024;
    if body.len() <= MAX {
        return body.to_owned();
    }
    let mut cut = MAX;
    while cut > 0 && !body.is_char_boundary(cut) {
        cut = cut.saturating_sub(1);
    }
    let kept = body.get(..cut).unwrap_or_default();
    format!("{kept}...[truncated]")
}

/// Assemble a unified diff from `pulls/{n}/files` entries: a
/// `diff --git` header per file, rename lines for a renamed file, and the
/// file's hunks when GitHub sent a patch. A file without a patch keeps only
/// its header, so it still counts as changed without any content.
fn diff_from_file_patches(files: &[PullRequestFile]) -> String {
    let mut diff = String::new();
    for file in files {
        let old_path = file.previous_filename.as_deref().unwrap_or(&file.filename);
        diff.push_str("diff --git a/");
        diff.push_str(old_path);
        diff.push_str(" b/");
        diff.push_str(&file.filename);
        diff.push('\n');
        if file.previous_filename.is_some() {
            diff.push_str("rename from ");
            diff.push_str(old_path);
            diff.push_str("\nrename to ");
            diff.push_str(&file.filename);
            diff.push('\n');
        }
        let Some(patch) = file.patch.as_deref().filter(|patch| !patch.is_empty()) else {
            continue;
        };
        if file.status == "added" {
            diff.push_str("--- /dev/null\n");
        } else {
            diff.push_str("--- a/");
            diff.push_str(old_path);
            diff.push('\n');
        }
        if file.status == "removed" {
            diff.push_str("+++ /dev/null\n");
        } else {
            diff.push_str("+++ b/");
            diff.push_str(&file.filename);
            diff.push('\n');
        }
        diff.push_str(patch);
        if !patch.ends_with('\n') {
            diff.push('\n');
        }
    }
    diff
}

/// The review body with every inline comment appended, for the retry after
/// GitHub refuses to anchor an inline comment.
fn fold_inline_comments_into_body(request: &ar_forge::CreateReviewRequest) -> String {
    let mut body = request.body.clone();
    body.push_str("\n\n---\n\n**Inline comments that GitHub could not anchor to the diff:**");
    for comment in &request.comments {
        let location = match (comment.new_position, comment.old_position) {
            (Some(line), _) => format!(" line {line}"),
            (None, Some(line)) => format!(" old line {line}"),
            (None, None) => String::new(),
        };
        body.push_str("\n\n#### `");
        body.push_str(&comment.path);
        body.push('`');
        body.push_str(&location);
        body.push_str("\n\n");
        body.push_str(&comment.body);
    }
    body
}

/// Convert a GitHub client error into the provider-neutral host error,
/// keeping its `Display` text as the message.
fn host_error(error: &Error) -> ar_forge::HostError {
    ar_forge::HostError::new(error.to_string())
}

/// Whether `error` is GitHub refusing a diff media type because the diff is
/// over its size limits: `406` with the `too_large` error code, for too
/// many files, lines or bytes alike.
fn is_diff_too_large(error: &Error) -> bool {
    matches!(error, Error::Api { status: 406, body } if body.contains("too_large"))
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

/// Resolve the app's bot login from `GET /app`, bounded by `limit`.
async fn resolve_bot_login(
    client: &Client,
    limit: Duration,
) -> Result<String, ar_forge::HostError> {
    timeout(limit, client.get_authenticated_app())
        .await
        .unwrap_or(Err(Error::Timeout(limit)))
        .map(|app| app.bot_login())
        .map_err(|error| host_error(&error))
}

/// `text` cut to at most `max_chars` characters, ending in an ellipsis when
/// shortened.
fn truncate_chars(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_owned();
    }
    let mut truncated: String = text.chars().take(max_chars.saturating_sub(1)).collect();
    truncated.push('\u{2026}');
    truncated
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(filename: &str, status: &str, patch: Option<&str>) -> PullRequestFile {
        PullRequestFile {
            filename: filename.to_owned(),
            patch: patch.map(str::to_owned),
            previous_filename: None,
            status: status.to_owned(),
        }
    }

    #[test]
    fn diff_from_file_patches_marks_added_and_removed_sides_as_dev_null() {
        let diff = diff_from_file_patches(&[
            file("new.rs", "added", Some("@@ -0,0 +1 @@\n+fresh")),
            file("gone.rs", "removed", Some("@@ -1 +0,0 @@\n-stale")),
        ]);

        assert_eq!(
            diff,
            "diff --git a/new.rs b/new.rs\n--- /dev/null\n+++ b/new.rs\n@@ -0,0 +1 @@\n+fresh\n\
             diff --git a/gone.rs b/gone.rs\n--- a/gone.rs\n+++ /dev/null\n@@ -1 +0,0 @@\n-stale\n"
        );
    }

    #[test]
    fn diff_from_file_patches_keeps_only_the_header_for_a_file_without_a_patch() {
        let diff = diff_from_file_patches(&[file("logo.png", "added", None)]);

        assert_eq!(diff, "diff --git a/logo.png b/logo.png\n");
    }

    #[test]
    fn is_diff_too_large_needs_both_the_406_status_and_the_too_large_code() {
        let api = |status: u16, body: &str| Error::Api {
            status,
            body: body.to_owned(),
        };

        assert!(is_diff_too_large(&api(
            406,
            r#"{"errors":[{"code":"too_large"}]}"#
        )));
        assert!(!is_diff_too_large(&api(406, "Not Acceptable")));
        assert!(!is_diff_too_large(&api(422, "too_large")));
    }
}

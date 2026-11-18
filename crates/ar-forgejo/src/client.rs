use crate::capabilities::capabilities_for_version;
use crate::types::{
    ChangedFile, CommitStatus, CreateReviewRequest, CreateWebhookRequest, CreatedWebhook,
    PrReviewComment, PullRequestSummary, PullReviewSummary, WebhookSummary,
};
use crate::{cap_for_error, host_error, probe};
use alloc::sync::Arc;
use async_trait::async_trait;
use core::time::Duration;
use reqwest::header::{HeaderMap, HeaderValue, ACCEPT, AUTHORIZATION, CONTENT_TYPE, USER_AGENT};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use thiserror::Error;
use tokio::sync::OnceCell;
use url::Url;

#[expect(
    clippy::useless_attribute,
    reason = "clippy flags any lint attribute on a use item outside its fixed list, which omits \
              pub_use, yet the pub_use expectation below is what suppresses that lint here"
)]
#[expect(
    clippy::pub_use,
    reason = "ar_forgejo::client::CreatedReview is the public path for the create_review return \
              type; dropping the re-export removes that path from the crate API"
)]
pub use ar_forge::CreatedReview;

/// Defensive cap on the number of pages a paginated list call
/// will fetch. 100 pages × 50 rows = 5,000 rows. Hitting this
/// limit means either an accidental everything-is-renamed PR or
/// a bug in the loop's termination condition; better to stop
/// here than OOM on serialised JSON.
const PAGINATION_MAX_PAGES: u32 = 100;

/// Default Forgejo page size for paginated list endpoints.
/// Forgejo's actual default is 50; this matches it explicitly so
/// behaviour is deterministic regardless of server-side defaults
/// (which can be tuned per-instance via `app.ini`).
const PAGINATION_PAGE_SIZE: usize = 50;

/// Forgejo REST client.
///
/// Wraps a `reqwest::Client` with auth + base URL + JSON helpers. Constructed
/// from a base URL like `https://forgejo.example.com` and an API token issued
/// to the bot user.
#[derive(Debug, Clone)]
pub struct Client {
    /// Normalised server base URL, always ending in `/`.
    base: Url,
    /// The bot's own login, resolved once (or seeded) and shared by
    /// every clone.
    bot_login: Arc<OnceCell<String>>,
    /// Server capabilities, probed once and shared by every clone.
    capabilities: Arc<OnceCell<ar_forge::HostCapabilities>>,
    /// HTTP client carrying the token auth and JSON default headers.
    http: reqwest::Client,
    /// The bot's API token, also embedded in clone URLs.
    token: String,
}

#[expect(
    clippy::error_impl_error,
    reason = "ar-gateway (startup), ar-review (error) and this crate's integration tests name \
              this type ar_forgejo::Error; renaming it breaks them"
)]
#[derive(Debug, Error)]
pub enum Error {
    #[error("API error {status}: {body}")]
    Api { status: u16, body: String },
    #[error("HTTP error: {0}")]
    Http(#[from] reqwest::Error),
    #[error("invalid base URL: {0}")]
    InvalidBaseUrl(String),
    #[error("request timed out after {0:?}")]
    Timeout(Duration),
    #[error("URL build error: {0}")]
    UrlBuild(#[from] url::ParseError),
}

/// Forgejo's wire shape for the list-webhooks endpoint. Internal
/// to [`Client::list_webhooks`]; flattened to [`WebhookSummary`]
/// before returning so callers don't need to reach through nested
/// `config.url`.
#[derive(Debug, Clone, Deserialize)]
struct WebhookListItem {
    /// Whether the webhook fires.
    #[serde(default)]
    active: bool,
    /// The webhook's config map; only `url` is kept.
    #[serde(default)]
    config: HashMap<String, String>,
    /// Event names the webhook subscribes to.
    #[serde(default)]
    events: Vec<String>,
    /// Forgejo's webhook id.
    id: u64,
    /// Webhook type, such as `forgejo` or `gitea`.
    #[serde(rename = "type")]
    #[serde(default)]
    kind: String,
}

#[expect(
    clippy::same_name_method,
    reason = "ar-cli (commands) and ar-gateway call these inherent methods on Client by the same \
              names the ReviewHost impl below delegates to; renaming either side breaks callers"
)]
impl Client {
    /// Build an authenticated `git clone` URL for `owner/repo`, keeping a
    /// subpath deploy's base path and carrying the token as the password.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidBaseUrl`] when the base URL cannot carry
    /// credentials.
    #[inline]
    pub fn clone_url(&self, owner: &str, repo: &str) -> Result<String, Error> {
        let mut url = self.base.clone();
        let path = {
            let trimmed = url.path().trim_end_matches('/');
            format!("{trimmed}/{owner}/{repo}.git")
        };
        url.set_path(&path);
        url.set_username("oauth2")
            .map_err(|()| Error::InvalidBaseUrl("failed to set clone username".into()))?;
        url.set_password(Some(&self.token))
            .map_err(|()| Error::InvalidBaseUrl("failed to set clone password".into()))?;
        Ok(url.to_string())
    }

    /// Create a review on a pull request, optionally with inline line comments.
    ///
    /// # Errors
    ///
    /// Returns an error when the request fails, Forgejo answers with a
    /// non-success status, or the response does not decode.
    #[inline]
    pub async fn create_review(
        &self,
        owner: &str,
        repo: &str,
        n: u64,
        req: &CreateReviewRequest,
    ) -> Result<CreatedReview, Error> {
        let url = self.url(&format!("repos/{owner}/{repo}/pulls/{n}/reviews"))?;
        json_post(&self.http, url, req).await
    }

    /// Register a webhook on a repository so it POSTs PR events to
    /// `request.config.url`. `secret` is what the gateway HMAC-verifies.
    ///
    /// # Errors
    ///
    /// Returns an error when the request fails, Forgejo answers with a
    /// non-success status, or the response does not decode.
    #[inline]
    pub async fn create_webhook(
        &self,
        owner: &str,
        repo: &str,
        request: &CreateWebhookRequest,
    ) -> Result<CreatedWebhook, Error> {
        let url = self.url(&format!("repos/{owner}/{repo}/hooks"))?;
        json_post(&self.http, url, request).await
    }

    /// Delete a webhook by id. The id comes from `list_webhooks` or
    /// from the operator's records when they ran `register-webhook`
    /// (it printed the id at creation).
    ///
    /// # Errors
    ///
    /// Returns an error when the request fails or Forgejo answers with a
    /// non-success status.
    #[inline]
    pub async fn delete_webhook(&self, owner: &str, repo: &str, id: u64) -> Result<(), Error> {
        let url = self.url(&format!("repos/{owner}/{repo}/hooks/{id}"))?;
        let resp = self.http.delete(url).send().await?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(Error::Api {
                status: status.as_u16(),
                body: cap_for_error(&body),
            });
        }
        Ok(())
    }

    /// Replace the body of an existing comment. Forgejo v11+ edits
    /// both top-level issue/PR comments and inline code comments
    /// through this endpoint. Callers must pass only ids of comments
    /// the bot authored (THREAT-MODEL.md T9).
    ///
    /// Forgejo answers `204 No Content` without editing when the
    /// comment type does not support content, so a 204 is an error.
    ///
    /// # Errors
    ///
    /// Returns an error when the request fails, Forgejo answers with a
    /// non-success status, or Forgejo answers `204 No Content`.
    #[inline]
    pub async fn edit_issue_comment(
        &self,
        owner: &str,
        repo: &str,
        comment_id: u64,
        body: &str,
    ) -> Result<(), Error> {
        #[derive(serde::Serialize)]
        struct Req<'text> {
            body: &'text str,
        }
        let url = self.url(&format!(
            "repos/{owner}/{repo}/issues/comments/{comment_id}"
        ))?;
        let status = json_patch(&self.http, url, &Req { body }).await?;
        if status == reqwest::StatusCode::NO_CONTENT {
            return Err(Error::Api {
                status: status.as_u16(),
                body: format!(
                    "comment {comment_id} was not edited (Forgejo returned 204 No Content)"
                ),
            });
        }
        Ok(())
    }

    /// `GET /user`: login of the account the token belongs to.
    ///
    /// # Errors
    ///
    /// Returns an error when the request fails, Forgejo answers with a
    /// non-success status, or the response does not decode.
    #[inline]
    pub async fn get_authenticated_user_login(&self) -> Result<String, Error> {
        #[derive(serde::Deserialize)]
        struct UserResponse {
            login: String,
        }
        let url = self.url("user")?;
        let resp: UserResponse = json_get(&self.http, url).await?;
        Ok(resp.login)
    }

    /// Fetch the unified diff between two commit SHAs (or branches).
    /// Used for incremental review: when a PR gets new commits, the
    /// orchestrator can ask for `previous_head..current_head` instead
    /// of re-reviewing the whole PR.
    ///
    /// Forgejo accepts the standard `base...head` triple-dot syntax
    /// for range diffs.
    ///
    /// # Errors
    ///
    /// Returns an error when the URL cannot be built, the request fails, or
    /// Forgejo answers with a non-success status.
    #[inline]
    pub async fn get_compare_diff(
        &self,
        owner: &str,
        repo: &str,
        base: &str,
        head: &str,
    ) -> Result<String, Error> {
        let url = self
            .base
            .join(&format!("{owner}/{repo}/compare/{base}...{head}.diff"))?;
        let resp = self
            .http
            .get(url)
            .header(ACCEPT, "text/plain")
            .send()
            .await?;
        let status = resp.status();
        let body = resp.text().await?;
        if !status.is_success() {
            return Err(Error::Api {
                status: status.as_u16(),
                body: cap_for_error(&body),
            });
        }
        Ok(body)
    }

    /// Fetch a file's raw contents from the repo at `ref_` (a branch, tag, or
    /// commit SHA). Returns `Ok(None)` when the file does not exist (404) so
    /// callers can treat "no config" the same as "default config".
    ///
    /// This is the transport for the trusted base-branch loader
    /// (`ar_review::trusted_files`): both the review dispatcher and the chat
    /// handler read `.auto_review.yaml` through it at the PR's base branch,
    /// never from the head checkout. The whole body is read into memory; the
    /// loader's size cap applies after the read.
    ///
    /// # Errors
    ///
    /// Returns an error when the URL cannot be built, the request fails, or
    /// Forgejo answers with a non-success status other than 404.
    #[inline]
    pub async fn get_file_content(
        &self,
        owner: &str,
        repo: &str,
        file_path: &str,
        ref_: &str,
    ) -> Result<Option<String>, Error> {
        // Build the path segment-by-segment and the query through
        // `query_pairs_mut` so branch names such as `feature/a&b` and file
        // names containing `#` or `?` reach Forgejo intact instead of
        // splitting into stray query params or a fragment.
        let mut url = self.url(&format!("repos/{owner}/{repo}/raw/"))?;
        url.path_segments_mut()
            .map_err(|()| Error::InvalidBaseUrl("base URL cannot carry path segments".into()))?
            .pop_if_empty()
            .extend(file_path.split('/').filter(|segment| !segment.is_empty()));
        url.query_pairs_mut().append_pair("ref", ref_);
        let resp = self
            .http
            .get(url)
            .header(ACCEPT, "text/plain")
            .send()
            .await?;
        let status = resp.status();
        if status.as_u16() == 404 {
            return Ok(None);
        }
        let body = resp.text().await?;
        if !status.is_success() {
            return Err(Error::Api {
                status: status.as_u16(),
                body: cap_for_error(&body),
            });
        }
        Ok(Some(body))
    }

    /// Fetch an issue by number. Forgejo serves pull requests on the same
    /// endpoint and marks them with a non-null `pull_request` object, which
    /// becomes `is_pull_request`. Returns `Ok(None)` on 404; every other
    /// non-success status is an error with a capped body.
    ///
    /// # Errors
    ///
    /// Returns an error when the request fails, Forgejo answers with a
    /// non-success status other than 404, or the response does not decode.
    #[inline]
    pub async fn get_issue(
        &self,
        owner: &str,
        repo: &str,
        n: u64,
    ) -> Result<Option<ar_forge::IssueSummary>, Error> {
        #[derive(Deserialize)]
        struct RawIssue {
            #[serde(default)]
            body: Option<String>,
            #[serde(default)]
            html_url: String,
            number: u64,
            #[serde(default)]
            pull_request: Option<serde_json::Value>,
            state: String,
            title: String,
        }
        let url = self.url(&format!("repos/{owner}/{repo}/issues/{n}"))?;
        let resp = self.http.get(url).send().await?;
        let status = resp.status();
        if status.as_u16() == 404 {
            return Ok(None);
        }
        let body = resp.text().await?;
        if !status.is_success() {
            return Err(Error::Api {
                status: status.as_u16(),
                body: cap_for_error(&body),
            });
        }
        let raw: RawIssue = serde_json::from_str(&body).map_err(|error| Error::Api {
            status: status.as_u16(),
            body: format!("decode error: {error}: {}", cap_for_error(&body)),
        })?;
        Ok(Some(ar_forge::IssueSummary {
            number: raw.number,
            title: raw.title,
            body: raw.body.unwrap_or_default(),
            state: raw.state,
            is_pull_request: raw.pull_request.is_some_and(|value| !value.is_null()),
            html_url: raw.html_url,
        }))
    }

    /// Fetch the unified diff for a pull request.
    ///
    /// # Errors
    ///
    /// Returns an error when the request fails or Forgejo answers with a
    /// non-success status.
    #[inline]
    pub async fn get_pr_diff(&self, owner: &str, repo: &str, n: u64) -> Result<String, Error> {
        let url = self.url(&format!("repos/{owner}/{repo}/pulls/{n}.diff"))?;
        let resp = self
            .http
            .get(url)
            .header(ACCEPT, "text/plain")
            .send()
            .await?;
        let status = resp.status();
        let body = resp.text().await?;
        if !status.is_success() {
            return Err(Error::Api {
                status: status.as_u16(),
                body: cap_for_error(&body),
            });
        }
        Ok(body)
    }

    /// Fetch a compact summary of a pull request — used by ad-hoc CLI
    /// invocations (e.g. `auto_review review-once`) to drive the same
    /// pipeline the webhook flow uses, without needing the webhook
    /// payload.
    ///
    /// # Errors
    ///
    /// Returns an error when the request fails, Forgejo answers with a
    /// non-success status, or the response does not decode.
    #[inline]
    pub async fn get_pull_request(
        &self,
        owner: &str,
        repo: &str,
        n: u64,
    ) -> Result<PullRequestSummary, Error> {
        let url = self.url(&format!("repos/{owner}/{repo}/pulls/{n}"))?;
        json_get(&self.http, url).await
    }

    /// Fetch the Forgejo server's reported version string. Used as a
    /// cheap connectivity probe by readiness checks at gateway startup.
    ///
    /// # Errors
    ///
    /// Returns an error when the request fails, Forgejo answers with a
    /// non-success status, or the response does not decode.
    #[inline]
    pub async fn get_server_version(&self) -> Result<String, Error> {
        #[derive(serde::Deserialize)]
        struct VersionResponse {
            version: String,
        }
        let url = self.url("version")?;
        let resp: VersionResponse = json_get(&self.http, url).await?;
        Ok(resp.version)
    }

    /// List changed files for a pull request, including patch
    /// hunks. Paginates internally — see `PAGINATION_PAGE_SIZE`
    /// and `PAGINATION_MAX_PAGES`. Large PRs return the full
    /// file set instead of the first page only.
    ///
    /// # Errors
    ///
    /// Returns an error when any page request fails, Forgejo answers with a
    /// non-success status, or a page does not decode.
    #[inline]
    pub async fn list_changed_files(
        &self,
        owner: &str,
        repo: &str,
        n: u64,
    ) -> Result<Vec<ChangedFile>, Error> {
        let mut all = Vec::new();
        for page in 1..=PAGINATION_MAX_PAGES {
            let url = self.url(&format!(
                "repos/{owner}/{repo}/pulls/{n}/files?page={page}&limit={PAGINATION_PAGE_SIZE}"
            ))?;
            let chunk: Vec<ChangedFile> = json_get(&self.http, url).await?;
            let chunk_len = chunk.len();
            all.extend(chunk);
            if chunk_len < PAGINATION_PAGE_SIZE {
                break;
            }
        }
        Ok(all)
    }

    /// List the top-level conversation-tab comments on a PR.
    ///
    /// Forgejo treats PRs as issues for the conversation thread, so this
    /// hits `issues/{n}/comments` (`pulls/{n}/comments` does not exist in
    /// Forgejo; verified against 15.0.0, returns 404). Forgejo filters
    /// this endpoint to plain comments, so inline review-thread comments
    /// are never included; list those per review with
    /// [`Self::list_pull_review_comments`]. Paginates internally.
    ///
    /// # Errors
    ///
    /// Returns an error when any page request fails, Forgejo answers with a
    /// non-success status, or a page does not decode.
    #[inline]
    pub async fn list_pr_review_comments(
        &self,
        owner: &str,
        repo: &str,
        n: u64,
    ) -> Result<Vec<PrReviewComment>, Error> {
        let mut all = Vec::new();
        for page in 1..=PAGINATION_MAX_PAGES {
            let url = self.url(&format!(
                "repos/{owner}/{repo}/issues/{n}/comments?page={page}&limit={PAGINATION_PAGE_SIZE}"
            ))?;
            let chunk: Vec<PrReviewComment> = json_get(&self.http, url).await?;
            let chunk_len = chunk.len();
            all.extend(chunk);
            if chunk_len < PAGINATION_PAGE_SIZE {
                break;
            }
        }
        Ok(all)
    }

    /// List comments attached to one pull request review.
    ///
    /// Forgejo's `pulls/{n}/reviews/{id}/comments` endpoint takes no
    /// `page`/`limit` parameters and returns the review's whole comment
    /// list, so this is a single request. Paging it would fetch the same
    /// full list repeatedly until `PAGINATION_MAX_PAGES` for any review
    /// with at least `PAGINATION_PAGE_SIZE` comments.
    ///
    /// # Errors
    ///
    /// Returns an error when the request fails, Forgejo answers with a
    /// non-success status, or the response does not decode.
    #[inline]
    pub async fn list_pull_review_comments(
        &self,
        owner: &str,
        repo: &str,
        n: u64,
        review_id: u64,
    ) -> Result<Vec<PrReviewComment>, Error> {
        let url = self.url(&format!(
            "repos/{owner}/{repo}/pulls/{n}/reviews/{review_id}/comments"
        ))?;
        json_get(&self.http, url).await
    }

    /// List pull request review records.
    ///
    /// # Errors
    ///
    /// Returns an error when any page request fails, Forgejo answers with a
    /// non-success status, or a page does not decode.
    #[inline]
    pub async fn list_pull_reviews(
        &self,
        owner: &str,
        repo: &str,
        n: u64,
    ) -> Result<Vec<PullReviewSummary>, Error> {
        let mut all = Vec::new();
        for page in 1..=PAGINATION_MAX_PAGES {
            let url = self.url(&format!(
                "repos/{owner}/{repo}/pulls/{n}/reviews?page={page}&limit={PAGINATION_PAGE_SIZE}"
            ))?;
            let chunk: Vec<PullReviewSummary> = json_get(&self.http, url).await?;
            let chunk_len = chunk.len();
            all.extend(chunk);
            if chunk_len < PAGINATION_PAGE_SIZE {
                break;
            }
        }
        Ok(all)
    }

    /// List webhooks installed on a repository. Operators use this
    /// to audit which webhook(s) point at the gateway and to find
    /// the `id` needed for `delete_webhook`. Paginates internally
    /// (50/page) so a repo with many hooks returns the full set.
    ///
    /// # Errors
    ///
    /// Returns an error when any page request fails, Forgejo answers with a
    /// non-success status, or a page does not decode.
    #[inline]
    pub async fn list_webhooks(
        &self,
        owner: &str,
        repo: &str,
    ) -> Result<Vec<WebhookSummary>, Error> {
        let mut all = Vec::new();
        for page in 1..=PAGINATION_MAX_PAGES {
            let url = self.url(&format!(
                "repos/{owner}/{repo}/hooks?page={page}&limit={PAGINATION_PAGE_SIZE}"
            ))?;
            let chunk: Vec<WebhookListItem> = json_get(&self.http, url).await?;
            let chunk_len = chunk.len();
            all.extend(chunk.into_iter().map(Into::into));
            if chunk_len < PAGINATION_PAGE_SIZE {
                break;
            }
        }
        Ok(all)
    }

    /// Build a client for the Forgejo server at `base_url` that
    /// authenticates as the bot with `token`.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidBaseUrl`] when `base_url` is not a URL or
    /// `token` is not a valid header value, and [`Error::Http`] when the
    /// HTTP client cannot be built.
    #[inline]
    pub fn new(base_url: &str, token: &str) -> Result<Self, Error> {
        // Forgejo can be deployed under a subpath (e.g.
        // `https://example.com/forgejo`). `Url::join` resolves
        // relative to the *last path component*, so a base without
        // a trailing slash silently drops the subpath when joining
        // `api/v1/`. Append `/` defensively so subpath deploys work
        // without requiring operators to remember the convention.
        let normalized = if base_url.ends_with('/') {
            base_url.to_owned()
        } else {
            format!("{base_url}/")
        };
        #[expect(
            clippy::map_err_ignore,
            reason = "InvalidBaseUrl reports the rejected URL itself; the url::ParseError \
                      detail is not part of the message callers and operators see"
        )]
        let base =
            Url::parse(&normalized).map_err(|_| Error::InvalidBaseUrl(normalized.clone()))?;
        let mut headers = HeaderMap::new();
        #[expect(
            clippy::map_err_ignore,
            reason = "InvalidHeaderValue carries no detail beyond its kind, and the error \
                      text must never echo the token it rejected"
        )]
        let mut auth_value = HeaderValue::from_str(&format!("token {token}"))
            .map_err(|_| Error::InvalidBaseUrl("non-ascii token".into()))?;
        // Mark the auth header as sensitive so reqwest's Debug
        // impl and any HTTP/2 frame logging redacts it. The
        // InitClient already does this; the main Client should
        // too — without it, a stray `tracing::debug!(?client)`
        // anywhere in the stack would leak the bot's PAT into
        // operator logs.
        auth_value.set_sensitive(true);
        headers.insert(AUTHORIZATION, auth_value);
        headers.insert(ACCEPT, HeaderValue::from_static("application/json"));
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
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
            token: token.to_owned(),
            capabilities: Arc::new(OnceCell::new()),
            bot_login: Arc::new(OnceCell::new()),
        })
    }

    /// Post a commit status (the aggregate pass/fail badge on the PR).
    ///
    /// # Errors
    ///
    /// Returns an error when the request fails or Forgejo answers with a
    /// non-success status.
    #[inline]
    pub async fn post_commit_status(
        &self,
        owner: &str,
        repo: &str,
        sha: &str,
        status: &CommitStatus,
    ) -> Result<(), Error> {
        let url = self.url(&format!("repos/{owner}/{repo}/statuses/{sha}"))?;
        let resp = self.http.post(url).json(status).send().await?;
        let response_status = resp.status();
        if !response_status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(Error::Api {
                status: response_status.as_u16(),
                body: cap_for_error(&body),
            });
        }
        Ok(())
    }

    /// Post a top-level comment on an issue or pull request. Used by
    /// the agentic chat handler to reply to `@auto_review` mentions.
    /// Returns the comment id on success.
    ///
    /// # Errors
    ///
    /// Returns an error when the request fails, Forgejo answers with a
    /// non-success status, or the response does not decode.
    #[inline]
    pub async fn post_issue_comment(
        &self,
        owner: &str,
        repo: &str,
        issue_number: u64,
        body: &str,
    ) -> Result<u64, Error> {
        #[derive(serde::Serialize)]
        struct Req<'text> {
            body: &'text str,
        }
        #[derive(serde::Deserialize)]
        struct Resp {
            id: u64,
        }
        let url = self.url(&format!(
            "repos/{owner}/{repo}/issues/{issue_number}/comments"
        ))?;
        let resp: Resp = json_post(&self.http, url, &Req { body }).await?;
        Ok(resp.id)
    }

    /// Edit a pull request's title and/or body. Only the provided fields are
    /// sent (Forgejo leaves omitted fields unchanged). Used to stamp — and
    /// later strip — the human-override marker on the PR so it rides into the
    /// squash/merge commit the maintainer creates from the PR title/body.
    ///
    /// # Errors
    ///
    /// Returns an error when the request fails or Forgejo answers with a
    /// non-success status.
    #[inline]
    pub async fn update_pull_request(
        &self,
        owner: &str,
        repo: &str,
        n: u64,
        title: Option<&str>,
        body: Option<&str>,
    ) -> Result<(), Error> {
        #[derive(Serialize)]
        #[expect(
            clippy::arbitrary_source_item_ordering,
            reason = "field order sets the JSON key order of the PATCH pulls/{n} body, which \
                      stays byte-for-byte as Forgejo received it before"
        )]
        struct Req<'text> {
            #[serde(skip_serializing_if = "Option::is_none")]
            title: Option<&'text str>,
            #[serde(skip_serializing_if = "Option::is_none")]
            body: Option<&'text str>,
        }
        let url = self.url(&format!("repos/{owner}/{repo}/pulls/{n}"))?;
        let resp = self
            .http
            .patch(url)
            .json(&Req { title, body })
            .send()
            .await?;
        let status = resp.status();
        if !status.is_success() {
            let error_body = resp.text().await.unwrap_or_default();
            return Err(Error::Api {
                status: status.as_u16(),
                body: cap_for_error(&error_body),
            });
        }
        Ok(())
    }

    /// Resolve an API path such as `repos/o/r/pulls/1` against
    /// `{base}api/v1/`.
    fn url(&self, path: &str) -> Result<Url, Error> {
        // Strip leading slash so `Url::join` doesn't drop the base path.
        let trimmed = path.trim_start_matches('/');
        let joined = self.base.join("api/v1/")?.join(trimmed)?;
        Ok(joined)
    }

    /// Seed the bot login resolved elsewhere (the gateway's startup
    /// identity, so `AR_BOT_LOGIN` wins) so
    /// [`ar_forge::ReviewHost::bot_login`] never calls `GET /user`.
    #[must_use = "the seeded login lives only on the returned client"]
    #[inline]
    pub fn with_bot_login(mut self, login: impl Into<String>) -> Self {
        self.bot_login = Arc::new(OnceCell::new_with(Some(login.into())));
        self
    }
}

#[async_trait]
impl ar_forge::ReviewHost for Client {
    #[inline]
    async fn bot_login(&self) -> Result<String, ar_forge::HostError> {
        self.bot_login
            .get_or_try_init(|| probe(self.get_authenticated_user_login()))
            .await
            .cloned()
            .map_err(|error| host_error(&error))
    }

    #[inline]
    async fn capabilities(&self) -> ar_forge::HostCapabilities {
        // Transport, HTTP and timeout failures leave the cell empty so the
        // next call probes again; an unparseable version is deterministic
        // and is cached.
        let probed = self
            .capabilities
            .get_or_try_init(|| async {
                let version = probe(self.get_server_version()).await?;
                Ok::<_, Error>(capabilities_for_version(&version).unwrap_or_else(|| {
                    tracing::warn!(
                        version = %cap_for_error(&version),
                        "unparseable Forgejo version; treating the server as \
                         supporting no optional review features"
                    );
                    ar_forge::HostCapabilities::default()
                }))
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

    #[inline]
    async fn clone_url(&self, owner: &str, repo: &str) -> Result<String, ar_forge::HostError> {
        self.clone_url(owner, repo)
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
        self.create_review(owner, repo, pr_number, request)
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
        self.edit_issue_comment(owner, repo, comment_id, body)
            .await
            .map_err(|error| host_error(&error))
    }

    /// Forgejo v11+ edits inline code comments through the
    /// issue-comment endpoint; there is no `pulls/comments/{id}` PATCH.
    #[inline]
    async fn edit_review_comment(
        &self,
        owner: &str,
        repo: &str,
        comment_id: u64,
        body: &str,
    ) -> Result<(), ar_forge::HostError> {
        self.edit_issue_comment(owner, repo, comment_id, body)
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
        self.get_compare_diff(owner, repo, base, head)
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
        self.get_file_content(owner, repo, file_path, ref_)
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
        self.get_issue(owner, repo, issue_number)
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
        self.get_pr_diff(owner, repo, pr_number)
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
        self.get_pull_request(owner, repo, pr_number)
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
        self.list_changed_files(owner, repo, pr_number)
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
        self.list_pr_review_comments(owner, repo, pr_number)
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
        self.list_pull_review_comments(owner, repo, pr_number, review_id)
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
        self.list_pull_reviews(owner, repo, pr_number)
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
        self.post_commit_status(owner, repo, sha, status)
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
        self.post_issue_comment(owner, repo, issue_number, body)
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
        self.update_pull_request(owner, repo, pr_number, title, body)
            .await
            .map_err(|error| host_error(&error))
    }
}

impl From<WebhookListItem> for WebhookSummary {
    #[inline]
    fn from(item: WebhookListItem) -> Self {
        Self {
            id: item.id,
            kind: item.kind,
            active: item.active,
            events: item.events,
            url: item.config.get("url").cloned().unwrap_or_default(),
        }
    }
}

/// GET `url` and decode the JSON body, mapping a non-success status to
/// [`Error::Api`] with a capped body.
async fn json_get<T>(http: &reqwest::Client, url: Url) -> Result<T, Error>
where
    T: DeserializeOwned,
{
    let resp = http.get(url).send().await?;
    let status = resp.status();
    let body = resp.text().await?;
    if !status.is_success() {
        return Err(Error::Api {
            status: status.as_u16(),
            body: cap_for_error(&body),
        });
    }
    serde_json::from_str(&body).map_err(|error| Error::Api {
        status: 200,
        body: format!("decode error: {error}: {}", cap_for_error(&body)),
    })
}

/// PATCH a JSON body and return the 2xx status without decoding the
/// response, which callers may not need.
async fn json_patch<I>(
    http: &reqwest::Client,
    url: Url,
    input: &I,
) -> Result<reqwest::StatusCode, Error>
where
    I: Serialize,
{
    let resp = http.patch(url).json(input).send().await?;
    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await?;
        return Err(Error::Api {
            status: status.as_u16(),
            body: cap_for_error(&body),
        });
    }
    Ok(status)
}

/// POST a JSON body and decode the JSON response, mapping a non-success
/// status to [`Error::Api`] with a capped body.
async fn json_post<I, T>(http: &reqwest::Client, url: Url, input: &I) -> Result<T, Error>
where
    I: Serialize,
    T: DeserializeOwned,
{
    let resp = http.post(url).json(input).send().await?;
    let status = resp.status();
    let body = resp.text().await?;
    if !status.is_success() {
        return Err(Error::Api {
            status: status.as_u16(),
            body: cap_for_error(&body),
        });
    }
    serde_json::from_str(&body).map_err(|error| Error::Api {
        status: 200,
        body: format!("decode error: {error}: {}", cap_for_error(&body)),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{CommitStatus, CommitStatusState, ReviewComment, ReviewEvent};
    use wiremock::matchers::{body_json, header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[expect(
        clippy::expect_used,
        reason = "a mock-server client that fails to build should abort the calling test at setup"
    )]
    async fn mock_client() -> (MockServer, Client) {
        let server = MockServer::start().await;
        let client = Client::new(&server.uri(), "test-token").expect("client");
        (server, client)
    }

    #[test]
    #[expect(
        clippy::expect_used,
        reason = "a failed request, decode or setup step should fail this test at the named step; the asserted values stay as written"
    )]
    fn base_url_without_trailing_slash_keeps_path_when_joined() {
        // Forgejo deployed under a subpath
        // (`https://example.com/forgejo`) is a real configuration.
        // Without normalisation, `Url::join("api/v1/")` would
        // resolve relative to the last path component and silently
        // drop `forgejo` from the URL, making every API call hit
        // the wrong endpoint.
        let client = Client::new("https://example.com/forgejo", "tok").expect("client");
        let url = client.url("repos/x/y/pulls/1.diff").expect("url");
        assert!(
            url.as_str().contains("/forgejo/api/v1/"),
            "subpath was dropped: {url}"
        );
    }

    #[test]
    #[expect(
        clippy::expect_used,
        reason = "a failed request, decode or setup step should fail this test at the named step; the asserted values stay as written"
    )]
    fn base_url_with_trailing_slash_works() {
        let client = Client::new("https://example.com/forgejo/", "tok").expect("client");
        let url = client.url("repos/x/y/pulls/1.diff").expect("url");
        assert!(url.as_str().contains("/forgejo/api/v1/"));
    }

    #[test]
    #[expect(
        clippy::expect_used,
        reason = "a failed request, decode or setup step should fail this test at the named step; the asserted values stay as written"
    )]
    fn base_url_at_root_works_with_or_without_trailing_slash() {
        for input in ["https://example.com", "https://example.com/"] {
            let client = Client::new(input, "tok").expect("client");
            let url = client.url("repos/x/y/pulls/1.diff").expect("url");
            assert_eq!(
                url.as_str(),
                "https://example.com/api/v1/repos/x/y/pulls/1.diff",
                "input = {input}"
            );
        }
    }

    #[test]
    #[expect(
        clippy::expect_used,
        reason = "a failed request, decode or setup step should fail this test at the named step; the asserted values stay as written"
    )]
    fn clone_url_uses_oauth2_token_and_preserves_subpath() {
        let client = Client::new("https://example.com/forgejo", "to/k:n#1").expect("client");
        let url = client.clone_url("alice", "widgets").expect("clone url");
        assert_eq!(
            url,
            "https://oauth2:to%2Fk%3An%231@example.com/forgejo/alice/widgets.git"
        );
    }

    #[tokio::test]
    #[expect(
        clippy::expect_used,
        reason = "a failed request, decode or setup step should fail this test at the named step; the asserted values stay as written"
    )]
    async fn get_pr_diff_returns_text() {
        let (server, client) = mock_client().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/repos/o/r/pulls/7.diff"))
            .and(header("Authorization", "token test-token"))
            .respond_with(ResponseTemplate::new(200).set_body_string("diff --git\n+hi\n"))
            .mount(&server)
            .await;

        let diff = client.get_pr_diff("o", "r", 7).await.expect("diff");
        assert!(diff.contains("+hi"));
    }

    #[tokio::test]
    #[expect(
        clippy::default_numeric_fallback,
        reason = "serde_json::json! numbers here are untyped JSON wire values in the mocked Forgejo payload; type suffixes would edit the fixture without changing the bytes sent"
    )]
    #[expect(
        clippy::expect_used,
        reason = "a failed request, decode or setup step should fail this test at the named step; the asserted values stay as written"
    )]
    #[expect(
        clippy::indexing_slicing,
        reason = "the length assertion just before pins how many elements exist, so indexing cannot panic and the asserted values stay as written"
    )]
    async fn list_changed_files_decodes_json() {
        let (server, client) = mock_client().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/repos/o/r/pulls/7/files"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {
                    "filename": "src/x.rs",
                    "status": "modified",
                    "additions": 3,
                    "deletions": 1,
                    "changes": 4,
                    "patch": "@@ -1,1 +1,3 @@\n hi\n+a\n+b\n"
                }
            ])))
            .mount(&server)
            .await;

        let files = client.list_changed_files("o", "r", 7).await.expect("files");
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].filename, "src/x.rs");
        assert_eq!(files[0].additions, 3);
    }

    #[tokio::test]
    #[expect(
        clippy::default_numeric_fallback,
        reason = "serde_json::json! numbers here are untyped JSON wire values in the mocked Forgejo payload; type suffixes would edit the fixture without changing the bytes sent"
    )]
    #[expect(
        clippy::expect_used,
        reason = "a failed request, decode or setup step should fail this test at the named step; the asserted values stay as written"
    )]
    #[expect(
        clippy::indexing_slicing,
        reason = "the length assertion just before pins how many elements exist, so indexing cannot panic and the asserted values stay as written"
    )]
    async fn list_changed_files_paginates_through_full_result_set() {
        use wiremock::matchers::query_param;
        let (server, client) = mock_client().await;
        // Build a 50-element page (full) followed by a 7-element
        // page (short → loop terminates).
        let page1: Vec<serde_json::Value> = (0..50)
            .map(|i| {
                serde_json::json!({
                    "filename": format!("file{i}.rs"),
                    "status": "modified",
                    "additions": 1,
                    "deletions": 0,
                    "changes": 1,
                    "patch": null
                })
            })
            .collect();
        let page2: Vec<serde_json::Value> = (50..57)
            .map(|i| {
                serde_json::json!({
                    "filename": format!("file{i}.rs"),
                    "status": "modified",
                    "additions": 1,
                    "deletions": 0,
                    "changes": 1,
                    "patch": null
                })
            })
            .collect();

        Mock::given(method("GET"))
            .and(path("/api/v1/repos/o/r/pulls/7/files"))
            .and(query_param("page", "1"))
            .and(query_param("limit", "50"))
            .respond_with(ResponseTemplate::new(200).set_body_json(&page1))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v1/repos/o/r/pulls/7/files"))
            .and(query_param("page", "2"))
            .and(query_param("limit", "50"))
            .respond_with(ResponseTemplate::new(200).set_body_json(&page2))
            .expect(1)
            .mount(&server)
            .await;

        let files = client.list_changed_files("o", "r", 7).await.expect("files");
        assert_eq!(files.len(), 57);
        assert_eq!(files[0].filename, "file0.rs");
        assert_eq!(files[56].filename, "file56.rs");
    }

    #[tokio::test]
    #[expect(
        clippy::default_numeric_fallback,
        reason = "serde_json::json! numbers here are untyped JSON wire values in the mocked Forgejo payload; type suffixes would edit the fixture without changing the bytes sent"
    )]
    #[expect(
        clippy::expect_used,
        reason = "a failed request, decode or setup step should fail this test at the named step; the asserted values stay as written"
    )]
    async fn list_changed_files_short_first_page_terminates_loop() {
        use wiremock::matchers::query_param;
        let (server, client) = mock_client().await;
        // 3 files, well below the 50-row page size — single page.
        let body: Vec<serde_json::Value> = (0..3)
            .map(|i| {
                serde_json::json!({
                    "filename": format!("file{i}.rs"),
                    "status": "modified",
                    "additions": 0,
                    "deletions": 0,
                    "changes": 0,
                    "patch": null
                })
            })
            .collect();

        Mock::given(method("GET"))
            .and(path("/api/v1/repos/o/r/pulls/7/files"))
            .and(query_param("page", "1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(&body))
            .expect(1)
            .mount(&server)
            .await;
        // page=2 must NOT be hit when page=1 returned a short
        // response. expect(0) proves the loop short-circuited.
        Mock::given(method("GET"))
            .and(path("/api/v1/repos/o/r/pulls/7/files"))
            .and(query_param("page", "2"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([])))
            .expect(0)
            .mount(&server)
            .await;

        let files = client.list_changed_files("o", "r", 7).await.expect("files");
        assert_eq!(files.len(), 3);
    }

    #[tokio::test]
    #[expect(
        clippy::default_numeric_fallback,
        reason = "serde_json::json! numbers here are untyped JSON wire values in the mocked Forgejo payload; type suffixes would edit the fixture without changing the bytes sent"
    )]
    #[expect(
        clippy::expect_used,
        reason = "a failed request, decode or setup step should fail this test at the named step; the asserted values stay as written"
    )]
    async fn create_review_posts_expected_body() {
        let (server, client) = mock_client().await;
        let req = CreateReviewRequest {
            body: "LGTM with notes".into(),
            commit_id: "deadbeef".into(),
            event: ReviewEvent::Comment,
            comments: vec![ReviewComment {
                path: "src/x.rs".into(),
                body: "off-by-one?".into(),
                old_position: None,
                new_position: Some(42),
            }],
        };
        Mock::given(method("POST"))
            .and(path("/api/v1/repos/o/r/pulls/7/reviews"))
            .and(body_json(&req))
            .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({
                "id": 99,
                "state": "COMMENT"
            })))
            .mount(&server)
            .await;

        let created = client.create_review("o", "r", 7, &req).await.expect("ok");
        assert_eq!(created.id, 99);
    }

    #[tokio::test]
    #[expect(
        clippy::expect_used,
        reason = "a failed request, decode or setup step should fail this test at the named step; the asserted values stay as written"
    )]
    async fn post_commit_status_succeeds() {
        let (server, client) = mock_client().await;
        let status = CommitStatus {
            state: CommitStatusState::Success,
            target_url: "https://example.com".into(),
            description: "all good".into(),
            context: "auto_review".into(),
        };
        Mock::given(method("POST"))
            .and(path("/api/v1/repos/o/r/statuses/abc123"))
            .and(body_json(&status))
            .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({})))
            .mount(&server)
            .await;

        client
            .post_commit_status("o", "r", "abc123", &status)
            .await
            .expect("ok");
    }

    #[tokio::test]
    #[expect(
        clippy::expect_used,
        reason = "a failed request, decode or setup step should fail this test at the named step; the asserted values stay as written"
    )]
    async fn get_compare_diff_returns_text() {
        let (server, client) = mock_client().await;
        Mock::given(method("GET"))
            .and(path("/o/r/compare/abc...def.diff"))
            .respond_with(ResponseTemplate::new(200).set_body_string("diff --git a/x b/x\n+y\n"))
            .mount(&server)
            .await;
        let diff = client
            .get_compare_diff("o", "r", "abc", "def")
            .await
            .expect("ok");
        assert!(diff.contains("+y"));
    }

    #[tokio::test]
    #[expect(
        clippy::expect_used,
        reason = "a failed request, decode or setup step should fail this test at the named step; the asserted values stay as written"
    )]
    #[expect(
        clippy::panic,
        reason = "an unexpected Error variant must fail this test, printing the variant it got"
    )]
    #[expect(
        clippy::wildcard_enum_match_arm,
        reason = "every Error variant other than the asserted one is a test failure, reported by the catch-all panic arm"
    )]
    async fn get_compare_diff_propagates_404() {
        let (server, client) = mock_client().await;
        Mock::given(method("GET"))
            .and(path("/o/r/compare/abc...def.diff"))
            .respond_with(ResponseTemplate::new(404).set_body_string("not found"))
            .mount(&server)
            .await;
        let err = client
            .get_compare_diff("o", "r", "abc", "def")
            .await
            .expect_err("err");
        match err {
            Error::Api { status, body } => {
                assert_eq!(status, 404);
                assert_eq!(body, "not found");
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    #[expect(
        clippy::default_numeric_fallback,
        reason = "serde_json::json! numbers here are untyped JSON wire values in the mocked Forgejo payload; type suffixes would edit the fixture without changing the bytes sent"
    )]
    #[expect(
        clippy::expect_used,
        reason = "a failed request, decode or setup step should fail this test at the named step; the asserted values stay as written"
    )]
    async fn post_issue_comment_returns_new_id() {
        let (server, client) = mock_client().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/repos/o/r/issues/7/comments"))
            .and(body_json(serde_json::json!({"body": "hi from the bot"})))
            .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({
                "id": 42,
                "body": "hi from the bot"
            })))
            .mount(&server)
            .await;
        let id = client
            .post_issue_comment("o", "r", 7, "hi from the bot")
            .await
            .expect("ok");
        assert_eq!(id, 42);
    }

    #[tokio::test]
    #[expect(
        clippy::expect_used,
        reason = "a failed request, decode or setup step should fail this test at the named step; the asserted values stay as written"
    )]
    #[expect(
        clippy::panic,
        reason = "an unexpected Error variant must fail this test, printing the variant it got"
    )]
    #[expect(
        clippy::rest_pattern_accessible_field,
        reason = "this test asserts only the status code; the error body is irrelevant to the case"
    )]
    #[expect(
        clippy::wildcard_enum_match_arm,
        reason = "every Error variant other than the asserted one is a test failure, reported by the catch-all panic arm"
    )]
    async fn post_issue_comment_propagates_403() {
        let (server, client) = mock_client().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/repos/o/r/issues/7/comments"))
            .respond_with(ResponseTemplate::new(403).set_body_string("forbidden"))
            .mount(&server)
            .await;
        let err = client
            .post_issue_comment("o", "r", 7, "x")
            .await
            .expect_err("err");
        match err {
            Error::Api { status, .. } => assert_eq!(status, 403),
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    #[expect(
        clippy::default_numeric_fallback,
        reason = "serde_json::json! numbers here are untyped JSON wire values in the mocked Forgejo payload; type suffixes would edit the fixture without changing the bytes sent"
    )]
    #[expect(
        clippy::expect_used,
        reason = "a failed request, decode or setup step should fail this test at the named step; the asserted values stay as written"
    )]
    async fn get_pull_request_decodes_summary() {
        let (server, client) = mock_client().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/repos/o/r/pulls/7"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "number": 7,
                "title": "fix: thing",
                "body": "details here",
                "draft": false,
                "user": {"login": "alice", "id": 1},
                "head": {"ref": "topic", "sha": "deadbeef"},
                "base": {"ref": "main", "sha": "cafef00d"}
            })))
            .mount(&server)
            .await;

        let pr = client.get_pull_request("o", "r", 7).await.expect("ok");
        assert_eq!(pr.number, 7);
        assert_eq!(pr.title, "fix: thing");
        assert_eq!(pr.body, "details here");
        assert!(!pr.draft);
        assert_eq!(pr.head.sha, "deadbeef");
        assert_eq!(pr.base.ref_name, "main");
    }

    #[tokio::test]
    #[expect(
        clippy::expect_used,
        reason = "a failed request, decode or setup step should fail this test at the named step; the asserted values stay as written"
    )]
    #[expect(
        clippy::panic,
        reason = "an unexpected Error variant must fail this test, printing the variant it got"
    )]
    #[expect(
        clippy::rest_pattern_accessible_field,
        reason = "this test asserts only the status code; the error body is irrelevant to the case"
    )]
    #[expect(
        clippy::wildcard_enum_match_arm,
        reason = "every Error variant other than the asserted one is a test failure, reported by the catch-all panic arm"
    )]
    async fn get_pull_request_propagates_404() {
        let (server, client) = mock_client().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/repos/o/r/pulls/9999"))
            .respond_with(ResponseTemplate::new(404).set_body_string("not found"))
            .mount(&server)
            .await;
        let err = client
            .get_pull_request("o", "r", 9999)
            .await
            .expect_err("err");
        match err {
            Error::Api { status, .. } => assert_eq!(status, 404),
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    #[expect(
        clippy::default_numeric_fallback,
        reason = "serde_json::json! numbers here are untyped JSON wire values in the mocked Forgejo payload; type suffixes would edit the fixture without changing the bytes sent"
    )]
    #[expect(
        clippy::expect_used,
        reason = "a failed request, decode or setup step should fail this test at the named step; the asserted values stay as written"
    )]
    #[expect(
        clippy::non_ascii_literal,
        reason = "the PR body fixture contains the `…` the override stamp writes; escaping it would edit the expected request body"
    )]
    async fn update_pull_request_patches_title_and_body() {
        let (server, client) = mock_client().await;
        Mock::given(method("PATCH"))
            .and(path("/api/v1/repos/o/r/pulls/7"))
            .and(body_json(serde_json::json!({
                "title": "[override-approved] fix: thing",
                "body": "details\n\n## Approval override\n…"
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "number": 7,
                "title": "[override-approved] fix: thing"
            })))
            .mount(&server)
            .await;

        client
            .update_pull_request(
                "o",
                "r",
                7,
                Some("[override-approved] fix: thing"),
                Some("details\n\n## Approval override\n…"),
            )
            .await
            .expect("ok");
    }

    #[tokio::test]
    #[expect(
        clippy::expect_used,
        reason = "a failed request, decode or setup step should fail this test at the named step; the asserted values stay as written"
    )]
    async fn update_pull_request_omits_unset_fields() {
        let (server, client) = mock_client().await;
        // Only `title` is provided; the JSON body must NOT carry `body`.
        Mock::given(method("PATCH"))
            .and(path("/api/v1/repos/o/r/pulls/7"))
            .and(body_json(serde_json::json!({ "title": "fix: thing" })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({})))
            .mount(&server)
            .await;

        client
            .update_pull_request("o", "r", 7, Some("fix: thing"), None)
            .await
            .expect("ok");
    }

    #[tokio::test]
    #[expect(
        clippy::expect_used,
        reason = "a failed request, decode or setup step should fail this test at the named step; the asserted values stay as written"
    )]
    async fn get_file_content_returns_some_on_200() {
        let (server, client) = mock_client().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/repos/o/r/raw/.auto_review.yaml"))
            .respond_with(
                ResponseTemplate::new(200).set_body_string("override_approvers:\n  - jwilger\n"),
            )
            .mount(&server)
            .await;

        let contents = client
            .get_file_content("o", "r", ".auto_review.yaml", "main")
            .await
            .expect("ok");
        assert_eq!(
            contents.as_deref(),
            Some("override_approvers:\n  - jwilger\n")
        );
    }

    #[tokio::test]
    #[expect(
        clippy::expect_used,
        reason = "a failed request, decode or setup step should fail this test at the named step; the asserted values stay as written"
    )]
    async fn get_file_content_percent_encodes_ref_query_param() {
        use wiremock::matchers::{query_param, query_param_is_missing};
        let (server, client) = mock_client().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/repos/o/r/raw/.auto_review.yaml"))
            .and(query_param("ref", "feature/a&b"))
            .and(query_param_is_missing("b"))
            .respond_with(ResponseTemplate::new(200).set_body_string("enabled: true\n"))
            .mount(&server)
            .await;

        let contents = client
            .get_file_content("o", "r", ".auto_review.yaml", "feature/a&b")
            .await
            .expect("ok");
        assert_eq!(contents.as_deref(), Some("enabled: true\n"));
    }

    #[tokio::test]
    #[expect(
        clippy::expect_used,
        reason = "a failed request, decode or setup step should fail this test at the named step; the asserted values stay as written"
    )]
    async fn get_file_content_percent_encodes_path_segments() {
        let (server, client) = mock_client().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/repos/o/r/raw/docs/a%23b.md"))
            .respond_with(ResponseTemplate::new(200).set_body_string("hash in name\n"))
            .mount(&server)
            .await;

        let contents = client
            .get_file_content("o", "r", "docs/a#b.md", "main")
            .await
            .expect("ok");
        assert_eq!(contents.as_deref(), Some("hash in name\n"));
    }

    #[tokio::test]
    #[expect(
        clippy::expect_used,
        reason = "a failed request, decode or setup step should fail this test at the named step; the asserted values stay as written"
    )]
    async fn get_file_content_returns_none_on_404() {
        let (server, client) = mock_client().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/repos/o/r/raw/.auto_review.yaml"))
            .respond_with(ResponseTemplate::new(404).set_body_string("Not Found"))
            .mount(&server)
            .await;

        let contents = client
            .get_file_content("o", "r", ".auto_review.yaml", "main")
            .await
            .expect("ok");
        assert!(contents.is_none());
    }

    #[tokio::test]
    #[expect(
        clippy::expect_used,
        reason = "a failed request, decode or setup step should fail this test at the named step; the asserted values stay as written"
    )]
    #[expect(
        clippy::min_ident_chars,
        reason = "the short binding feeds the assertion below; renaming it would edit that assertion"
    )]
    async fn get_server_version_returns_version_string() {
        let (server, client) = mock_client().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/version"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "version": "8.0.3"
            })))
            .mount(&server)
            .await;

        let v = client.get_server_version().await.expect("ok");
        assert_eq!(v, "8.0.3");
    }

    #[tokio::test]
    #[expect(
        clippy::expect_used,
        reason = "a failed request, decode or setup step should fail this test at the named step; the asserted values stay as written"
    )]
    #[expect(
        clippy::panic,
        reason = "an unexpected Error variant must fail this test, printing the variant it got"
    )]
    #[expect(
        clippy::rest_pattern_accessible_field,
        reason = "this test asserts only the status code; the error body is irrelevant to the case"
    )]
    #[expect(
        clippy::wildcard_enum_match_arm,
        reason = "every Error variant other than the asserted one is a test failure, reported by the catch-all panic arm"
    )]
    async fn get_server_version_propagates_5xx_errors() {
        let (server, client) = mock_client().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/version"))
            .respond_with(ResponseTemplate::new(503).set_body_string("down"))
            .mount(&server)
            .await;

        let err = client.get_server_version().await.expect_err("err");
        match err {
            Error::Api { status, .. } => assert_eq!(status, 503),
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    #[expect(
        clippy::absolute_paths,
        reason = "the test names the request types by their crate path, as the struct literal sent to the mock server was written"
    )]
    #[expect(
        clippy::default_numeric_fallback,
        reason = "serde_json::json! numbers here are untyped JSON wire values in the mocked Forgejo payload; type suffixes would edit the fixture without changing the bytes sent"
    )]
    #[expect(
        clippy::expect_used,
        reason = "a failed request, decode or setup step should fail this test at the named step; the asserted values stay as written"
    )]
    async fn create_webhook_posts_expected_body() {
        let (server, client) = mock_client().await;
        let req = crate::types::CreateWebhookRequest {
            kind: "forgejo".into(),
            config: crate::types::WebhookConfig {
                url: "https://reviewer.example.com/webhooks/forgejo".into(),
                content_type: "json".into(),
                secret: "shh".into(),
            },
            events: vec!["pull_request".into()],
            active: true,
        };

        Mock::given(method("POST"))
            .and(path("/api/v1/repos/o/r/hooks"))
            .and(body_json(&req))
            .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({
                "id": 7,
                "active": true,
                "events": ["pull_request"]
            })))
            .mount(&server)
            .await;

        let created = client.create_webhook("o", "r", &req).await.expect("ok");
        assert_eq!(created.id, 7);
        assert!(created.active);
    }

    #[tokio::test]
    #[expect(
        clippy::default_numeric_fallback,
        reason = "serde_json::json! numbers here are untyped JSON wire values in the mocked Forgejo payload; type suffixes would edit the fixture without changing the bytes sent"
    )]
    #[expect(
        clippy::expect_used,
        reason = "a failed request, decode or setup step should fail this test at the named step; the asserted values stay as written"
    )]
    #[expect(
        clippy::indexing_slicing,
        reason = "the length assertion just before pins how many elements exist, so indexing cannot panic and the asserted values stay as written"
    )]
    async fn list_webhooks_flattens_config_url_into_summary() {
        let (server, client) = mock_client().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/repos/o/r/hooks"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {
                    "id": 7,
                    "type": "forgejo",
                    "active": true,
                    "events": ["pull_request", "issue_comment"],
                    "config": {
                        "url": "https://reviewer.example.com/webhooks/forgejo",
                        "content_type": "json",
                        "secret": ""
                    }
                },
                {
                    "id": 12,
                    "type": "gitea",
                    "active": false,
                    "events": ["push"],
                    "config": {
                        "url": "https://other.example/legacy",
                        "content_type": "json",
                        "secret": ""
                    }
                }
            ])))
            .mount(&server)
            .await;

        let hooks = client.list_webhooks("o", "r").await.expect("ok");
        assert_eq!(hooks.len(), 2);
        assert_eq!(hooks[0].id, 7);
        assert_eq!(
            hooks[0].url,
            "https://reviewer.example.com/webhooks/forgejo"
        );
        assert!(hooks[0].active);
        assert_eq!(hooks[0].events, vec!["pull_request", "issue_comment"]);
        assert_eq!(hooks[1].id, 12);
        assert_eq!(hooks[1].url, "https://other.example/legacy");
        assert!(!hooks[1].active);
    }

    #[tokio::test]
    #[expect(
        clippy::default_numeric_fallback,
        reason = "serde_json::json! numbers here are untyped JSON wire values in the mocked Forgejo payload; type suffixes would edit the fixture without changing the bytes sent"
    )]
    #[expect(
        clippy::expect_used,
        reason = "a failed request, decode or setup step should fail this test at the named step; the asserted values stay as written"
    )]
    #[expect(
        clippy::indexing_slicing,
        reason = "the length assertion just before pins how many elements exist, so indexing cannot panic and the asserted values stay as written"
    )]
    async fn list_webhooks_paginates_through_full_result_set() {
        use wiremock::matchers::query_param;
        let (server, client) = mock_client().await;
        let page1: Vec<serde_json::Value> = (0..50)
            .map(|i| {
                serde_json::json!({
                    "id": i,
                    "type": "forgejo",
                    "active": true,
                    "events": [],
                    "config": {"url": format!("https://h{i}.example/x")}
                })
            })
            .collect();
        let page2: Vec<serde_json::Value> = (50..53)
            .map(|i| {
                serde_json::json!({
                    "id": i,
                    "type": "forgejo",
                    "active": true,
                    "events": [],
                    "config": {"url": format!("https://h{i}.example/x")}
                })
            })
            .collect();
        Mock::given(method("GET"))
            .and(path("/api/v1/repos/o/r/hooks"))
            .and(query_param("page", "1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(&page1))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v1/repos/o/r/hooks"))
            .and(query_param("page", "2"))
            .respond_with(ResponseTemplate::new(200).set_body_json(&page2))
            .expect(1)
            .mount(&server)
            .await;

        let hooks = client.list_webhooks("o", "r").await.expect("ok");
        assert_eq!(hooks.len(), 53);
        assert_eq!(hooks[0].id, 0);
        assert_eq!(hooks[52].id, 52);
    }

    #[tokio::test]
    #[expect(
        clippy::default_numeric_fallback,
        reason = "serde_json::json! numbers here are untyped JSON wire values in the mocked Forgejo payload; type suffixes would edit the fixture without changing the bytes sent"
    )]
    #[expect(
        clippy::expect_used,
        reason = "a failed request, decode or setup step should fail this test at the named step; the asserted values stay as written"
    )]
    #[expect(
        clippy::min_ident_chars,
        reason = "the short binding feeds the assertion below; renaming it would edit that assertion"
    )]
    async fn list_pr_review_comments_paginates() {
        use wiremock::matchers::query_param;
        let (server, client) = mock_client().await;
        let page1: Vec<serde_json::Value> = (0..50)
            .map(|i| {
                serde_json::json!({
                    "id": i,
                    "body": format!("comment {i}"),
                    "user": {"login": "alice", "id": 1}
                })
            })
            .collect();
        let page2: Vec<serde_json::Value> = (50..51)
            .map(|i| {
                serde_json::json!({
                    "id": i,
                    "body": "@auto_review re-review",
                    "user": {"login": "bob", "id": 2}
                })
            })
            .collect();
        Mock::given(method("GET"))
            .and(path("/api/v1/repos/o/r/issues/7/comments"))
            .and(query_param("page", "1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(&page1))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v1/repos/o/r/issues/7/comments"))
            .and(query_param("page", "2"))
            .respond_with(ResponseTemplate::new(200).set_body_json(&page2))
            .expect(1)
            .mount(&server)
            .await;

        let comments = client
            .list_pr_review_comments("o", "r", 7)
            .await
            .expect("ok");
        assert_eq!(comments.len(), 51);
        // The mention on comment id=50 (page 2) is now visible
        // to ChatPoller — previously it was silently dropped.
        let mention = comments
            .iter()
            .find(|c| c.body.contains("@auto_review"))
            .expect("mention on page 2 visible");
        assert_eq!(mention.id, 50);
    }

    #[tokio::test]
    #[expect(
        clippy::assert_is_empty,
        reason = "the assertion stays as written; it checks the decoded webhook list is empty"
    )]
    #[expect(
        clippy::expect_used,
        reason = "a failed request, decode or setup step should fail this test at the named step; the asserted values stay as written"
    )]
    async fn list_webhooks_handles_empty_response() {
        let (server, client) = mock_client().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/repos/o/r/hooks"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([])))
            .mount(&server)
            .await;
        let hooks = client.list_webhooks("o", "r").await.expect("ok");
        assert!(hooks.is_empty());
    }

    #[tokio::test]
    #[expect(
        clippy::expect_used,
        reason = "a failed request, decode or setup step should fail this test at the named step; the asserted values stay as written"
    )]
    async fn delete_webhook_uses_delete_verb_and_id_path() {
        let (server, client) = mock_client().await;
        Mock::given(method("DELETE"))
            .and(path("/api/v1/repos/o/r/hooks/7"))
            .respond_with(ResponseTemplate::new(204))
            .mount(&server)
            .await;
        client.delete_webhook("o", "r", 7).await.expect("ok");
    }

    #[tokio::test]
    #[expect(
        clippy::expect_used,
        reason = "a failed request, decode or setup step should fail this test at the named step; the asserted values stay as written"
    )]
    #[expect(
        clippy::panic,
        reason = "an unexpected Error variant must fail this test, printing the variant it got"
    )]
    #[expect(
        clippy::rest_pattern_accessible_field,
        reason = "this test asserts only the status code; the error body is irrelevant to the case"
    )]
    #[expect(
        clippy::wildcard_enum_match_arm,
        reason = "every Error variant other than the asserted one is a test failure, reported by the catch-all panic arm"
    )]
    async fn delete_webhook_propagates_404_as_api_error() {
        let (server, client) = mock_client().await;
        Mock::given(method("DELETE"))
            .and(path("/api/v1/repos/o/r/hooks/999"))
            .respond_with(ResponseTemplate::new(404).set_body_string("not found"))
            .mount(&server)
            .await;
        let err = client.delete_webhook("o", "r", 999).await.expect_err("404");
        match err {
            Error::Api { status, .. } => assert_eq!(status, 404),
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    #[expect(
        clippy::expect_used,
        reason = "a failed request, decode or setup step should fail this test at the named step; the asserted values stay as written"
    )]
    #[expect(
        clippy::panic,
        reason = "an unexpected Error variant must fail this test, printing the variant it got"
    )]
    #[expect(
        clippy::wildcard_enum_match_arm,
        reason = "every Error variant other than the asserted one is a test failure, reported by the catch-all panic arm"
    )]
    async fn api_error_propagates_status_and_body() {
        let (server, client) = mock_client().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/repos/o/r/pulls/7.diff"))
            .respond_with(ResponseTemplate::new(404).set_body_string("not found"))
            .mount(&server)
            .await;

        let err = client.get_pr_diff("o", "r", 7).await.expect_err("err");
        match err {
            Error::Api { status, body } => {
                assert_eq!(status, 404);
                assert_eq!(body, "not found");
            }
            other => panic!("unexpected err: {other:?}"),
        }
    }

    #[tokio::test]
    #[expect(
        clippy::default_numeric_fallback,
        reason = "serde_json::json! numbers here are untyped JSON wire values in the mocked Forgejo payload; type suffixes would edit the fixture without changing the bytes sent"
    )]
    #[expect(
        clippy::expect_used,
        reason = "a failed request, decode or setup step should fail this test at the named step; the asserted values stay as written"
    )]
    async fn list_pull_review_comments_fetches_an_unpaginated_endpoint_once() {
        // Forgejo's review-comments endpoint takes no page/limit and
        // returns the whole list on every request. A client that paged it
        // would get the same full page back until the page cap, every
        // poll cycle, for any review with 50 or more comments.
        let (server, client) = mock_client().await;
        let comments: Vec<serde_json::Value> = (1..=60)
            .map(|i| {
                serde_json::json!({
                    "id": i,
                    "body": "nit",
                    "user": {"login": "bob"},
                    "path": "src/lib.rs",
                    "position": i,
                    "original_position": 0
                })
            })
            .collect();
        Mock::given(method("GET"))
            .and(path("/api/v1/repos/o/r/pulls/7/reviews/3/comments"))
            .respond_with(ResponseTemplate::new(200).set_body_json(&comments))
            .expect(1)
            .mount(&server)
            .await;

        let got = client
            .list_pull_review_comments("o", "r", 7, 3)
            .await
            .expect("comments");
        assert_eq!(got.len(), 60);
        server.verify().await;
    }
}

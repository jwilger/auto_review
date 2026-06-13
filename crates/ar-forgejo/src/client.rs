use crate::types::{
    ChangedFile, CommitStatus, CreateReviewRequest, CreateWebhookRequest, CreatedWebhook,
    PullRequestSummary,
};
use reqwest::header::{HeaderMap, HeaderValue, ACCEPT, AUTHORIZATION, CONTENT_TYPE, USER_AGENT};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use url::Url;

#[derive(Debug, Error)]
pub enum Error {
    #[error("invalid base URL: {0}")]
    InvalidBaseUrl(String),
    #[error("HTTP error: {0}")]
    Http(#[from] reqwest::Error),
    #[error("API error {status}: {body}")]
    Api { status: u16, body: String },
    #[error("URL build error: {0}")]
    UrlBuild(#[from] url::ParseError),
}

/// Default Forgejo page size for paginated list endpoints.
/// Forgejo's actual default is 50; this matches it explicitly so
/// behaviour is deterministic regardless of server-side defaults
/// (which can be tuned per-instance via `app.ini`).
pub(crate) const PAGINATION_PAGE_SIZE: u32 = 50;

/// Defensive cap on the number of pages a paginated list call
/// will fetch. 100 pages × 50 rows = 5,000 rows. Hitting this
/// limit means either an accidental everything-is-renamed PR or
/// a bug in the loop's termination condition; better to stop
/// here than OOM on serialised JSON.
pub(crate) const PAGINATION_MAX_PAGES: u32 = 100;

/// Forgejo REST client.
///
/// Wraps a `reqwest::Client` with auth + base URL + JSON helpers. Constructed
/// from a base URL like `https://forgejo.example.com` and an API token issued
/// to the bot user.
#[derive(Debug, Clone)]
pub struct Client {
    http: reqwest::Client,
    base: Url,
}

impl Client {
    pub fn new(base_url: &str, token: &str) -> Result<Self, Error> {
        // Forgejo can be deployed under a subpath (e.g.
        // `https://example.com/forgejo`). `Url::join` resolves
        // relative to the *last path component*, so a base without
        // a trailing slash silently drops the subpath when joining
        // `api/v1/`. Append `/` defensively so subpath deploys work
        // without requiring operators to remember the convention.
        let normalized = if base_url.ends_with('/') {
            base_url.to_string()
        } else {
            format!("{base_url}/")
        };
        let base =
            Url::parse(&normalized).map_err(|_| Error::InvalidBaseUrl(normalized.clone()))?;
        let mut headers = HeaderMap::new();
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
        Ok(Self { http, base })
    }

    fn url(&self, path: &str) -> Result<Url, Error> {
        // Strip leading slash so `Url::join` doesn't drop the base path.
        let trimmed = path.trim_start_matches('/');
        let joined = self.base.join("api/v1/")?.join(trimmed)?;
        Ok(joined)
    }

    /// Fetch the unified diff for a pull request.
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

    /// List changed files for a pull request, including patch
    /// hunks. Paginates internally — see `PAGINATION_PAGE_SIZE`
    /// and `PAGINATION_MAX_PAGES`. Large PRs return the full
    /// file set instead of the first page only.
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
            if chunk_len < PAGINATION_PAGE_SIZE as usize {
                break;
            }
        }
        Ok(all)
    }

    /// Create a review on a pull request, optionally with inline line comments.
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

    /// Post a commit status (the aggregate pass/fail badge on the PR).
    pub async fn post_commit_status(
        &self,
        owner: &str,
        repo: &str,
        sha: &str,
        status: &CommitStatus,
    ) -> Result<(), Error> {
        let url = self.url(&format!("repos/{owner}/{repo}/statuses/{sha}"))?;
        let resp = self.http.post(url).json(status).send().await?;
        let s = resp.status();
        if !s.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(Error::Api {
                status: s.as_u16(),
                body: cap_for_error(&body),
            });
        }
        Ok(())
    }

    /// Register a webhook on a repository so it POSTs PR events to
    /// `request.config.url`. `secret` is what the gateway HMAC-verifies.
    pub async fn create_webhook(
        &self,
        owner: &str,
        repo: &str,
        request: &CreateWebhookRequest,
    ) -> Result<CreatedWebhook, Error> {
        let url = self.url(&format!("repos/{owner}/{repo}/hooks"))?;
        json_post(&self.http, url, request).await
    }

    /// List webhooks installed on a repository. Operators use this
    /// to audit which webhook(s) point at the gateway and to find
    /// the `id` needed for `delete_webhook`. Paginates internally
    /// (50/page) so a repo with many hooks returns the full set.
    pub async fn list_webhooks(
        &self,
        owner: &str,
        repo: &str,
    ) -> Result<Vec<crate::types::WebhookSummary>, Error> {
        let mut all = Vec::new();
        for page in 1..=PAGINATION_MAX_PAGES {
            let url = self.url(&format!(
                "repos/{owner}/{repo}/hooks?page={page}&limit={PAGINATION_PAGE_SIZE}"
            ))?;
            let chunk: Vec<crate::types::WebhookListItem> = json_get(&self.http, url).await?;
            let chunk_len = chunk.len();
            all.extend(chunk.into_iter().map(Into::into));
            if chunk_len < PAGINATION_PAGE_SIZE as usize {
                break;
            }
        }
        Ok(all)
    }

    /// Delete a webhook by id. The id comes from `list_webhooks` or
    /// from the operator's records when they ran `register-webhook`
    /// (it printed the id at creation).
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

    /// Fetch a compact summary of a pull request — used by ad-hoc CLI
    /// invocations (e.g. `auto_review review-once`) to drive the same
    /// pipeline the webhook flow uses, without needing the webhook
    /// payload.
    pub async fn get_pull_request(
        &self,
        owner: &str,
        repo: &str,
        n: u64,
    ) -> Result<PullRequestSummary, Error> {
        let url = self.url(&format!("repos/{owner}/{repo}/pulls/{n}"))?;
        json_get(&self.http, url).await
    }

    /// Edit a pull request's title and/or body. Only the provided fields are
    /// sent (Forgejo leaves omitted fields unchanged). Used to stamp — and
    /// later strip — the human-override marker on the PR so it rides into the
    /// squash/merge commit the maintainer creates from the PR title/body.
    pub async fn update_pull_request(
        &self,
        owner: &str,
        repo: &str,
        n: u64,
        title: Option<&str>,
        body: Option<&str>,
    ) -> Result<(), Error> {
        #[derive(Serialize)]
        struct Req<'a> {
            #[serde(skip_serializing_if = "Option::is_none")]
            title: Option<&'a str>,
            #[serde(skip_serializing_if = "Option::is_none")]
            body: Option<&'a str>,
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
            let body = resp.text().await.unwrap_or_default();
            return Err(Error::Api {
                status: status.as_u16(),
                body: cap_for_error(&body),
            });
        }
        Ok(())
    }

    /// Fetch a file's raw contents from the repo at `ref_` (a branch, tag, or
    /// commit SHA). Returns `Ok(None)` when the file does not exist (404) so
    /// callers can treat "no config" the same as "default config". Used by the
    /// chat path to read `.auto_review.yaml` without cloning the repo.
    pub async fn get_file_content(
        &self,
        owner: &str,
        repo: &str,
        file_path: &str,
        ref_: &str,
    ) -> Result<Option<String>, Error> {
        let url = self.url(&format!("repos/{owner}/{repo}/raw/{file_path}?ref={ref_}"))?;
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

    /// Fetch the unified diff between two commit SHAs (or branches).
    /// Used for incremental review: when a PR gets new commits, the
    /// orchestrator can ask for `previous_head..current_head` instead
    /// of re-reviewing the whole PR.
    ///
    /// Forgejo accepts the standard `base...head` triple-dot syntax
    /// for range diffs.
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

    /// Post a top-level comment on an issue or pull request. Used by
    /// the agentic chat handler to reply to `@auto_review` mentions.
    /// Returns the comment id on success.
    pub async fn post_issue_comment(
        &self,
        owner: &str,
        repo: &str,
        issue_number: u64,
        body: &str,
    ) -> Result<u64, Error> {
        #[derive(serde::Serialize)]
        struct Req<'a> {
            body: &'a str,
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

    /// List inline review-thread comments on a pull request.
    ///
    /// Used by the chat poller as a fallback for the
    /// `pull_request_review_comment` webhook, which Forgejo doesn't
    /// fire reliably for thread replies. Returns every review
    /// comment on the PR; the caller filters by id cursor to detect
    /// new ones.
    ///
    /// List the inline review-thread comments on a PR. Used by
    /// `ChatPoller` to scan for `@<bot>` mentions inside review
    /// threads (which Forgejo's `pull_request_review_comment`
    /// webhook doesn't fire reliably for; gitea#26023).
    /// Paginates internally — chatty PR threads with > 50
    /// comments now return the full set instead of the first
    /// page only.
    /// List top-level conversation-tab comments on a PR. Forgejo treats
    /// PRs as issues for the conversation thread, so this hits the
    /// issues endpoint — `pulls/{n}/comments` does not exist in Forgejo
    /// (verified against 15.0.0; returns 404). The chat poller uses this
    /// to detect new `@<bot>` mentions on the PR.
    pub async fn list_pr_review_comments(
        &self,
        owner: &str,
        repo: &str,
        n: u64,
    ) -> Result<Vec<crate::types::PrReviewComment>, Error> {
        let mut all = Vec::new();
        for page in 1..=PAGINATION_MAX_PAGES {
            let url = self.url(&format!(
                "repos/{owner}/{repo}/issues/{n}/comments?page={page}&limit={PAGINATION_PAGE_SIZE}"
            ))?;
            let chunk: Vec<crate::types::PrReviewComment> = json_get(&self.http, url).await?;
            let chunk_len = chunk.len();
            all.extend(chunk);
            if chunk_len < PAGINATION_PAGE_SIZE as usize {
                break;
            }
        }
        Ok(all)
    }

    /// List pull request review records.
    pub async fn list_pull_reviews(
        &self,
        owner: &str,
        repo: &str,
        n: u64,
    ) -> Result<Vec<crate::types::PullReviewSummary>, Error> {
        let mut all = Vec::new();
        for page in 1..=PAGINATION_MAX_PAGES {
            let url = self.url(&format!(
                "repos/{owner}/{repo}/pulls/{n}/reviews?page={page}&limit={PAGINATION_PAGE_SIZE}"
            ))?;
            let chunk: Vec<crate::types::PullReviewSummary> = json_get(&self.http, url).await?;
            let chunk_len = chunk.len();
            all.extend(chunk);
            if chunk_len < PAGINATION_PAGE_SIZE as usize {
                break;
            }
        }
        Ok(all)
    }

    /// List comments attached to one pull request review.
    pub async fn list_pull_review_comments(
        &self,
        owner: &str,
        repo: &str,
        n: u64,
        review_id: u64,
    ) -> Result<Vec<crate::types::PrReviewComment>, Error> {
        let mut all = Vec::new();
        for page in 1..=PAGINATION_MAX_PAGES {
            let url = self.url(&format!(
                "repos/{owner}/{repo}/pulls/{n}/reviews/{review_id}/comments?page={page}&limit={PAGINATION_PAGE_SIZE}"
            ))?;
            let chunk: Vec<crate::types::PrReviewComment> = json_get(&self.http, url).await?;
            let chunk_len = chunk.len();
            all.extend(chunk);
            if chunk_len < PAGINATION_PAGE_SIZE as usize {
                break;
            }
        }
        Ok(all)
    }

    /// Fetch the Forgejo server's reported version string. Used as a
    /// cheap connectivity probe by readiness checks at gateway startup.
    pub async fn get_server_version(&self) -> Result<String, Error> {
        #[derive(serde::Deserialize)]
        struct VersionResponse {
            version: String,
        }
        let url = self.url("version")?;
        let resp: VersionResponse = json_get(&self.http, url).await?;
        Ok(resp.version)
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct CreatedReview {
    pub id: u64,
    #[serde(default)]
    pub state: String,
}

async fn json_get<T: for<'de> Deserialize<'de>>(
    http: &reqwest::Client,
    url: Url,
) -> Result<T, Error> {
    let resp = http.get(url).send().await?;
    let status = resp.status();
    let body = resp.text().await?;
    if !status.is_success() {
        return Err(Error::Api {
            status: status.as_u16(),
            body: cap_for_error(&body),
        });
    }
    serde_json::from_str(&body).map_err(|e| Error::Api {
        status: 200,
        body: format!("decode error: {e}: {}", cap_for_error(&body)),
    })
}

async fn json_post<I: Serialize, T: for<'de> Deserialize<'de>>(
    http: &reqwest::Client,
    url: Url,
    input: &I,
) -> Result<T, Error> {
    let resp = http.post(url).json(input).send().await?;
    let status = resp.status();
    let body = resp.text().await?;
    if !status.is_success() {
        return Err(Error::Api {
            status: status.as_u16(),
            body: cap_for_error(&body),
        });
    }
    serde_json::from_str(&body).map_err(|e| Error::Api {
        status: 200,
        body: format!("decode error: {e}: {}", cap_for_error(&body)),
    })
}

/// Cap the response body included in error messages. Forgejo
/// errors are usually small JSON ({"message": "..."}), but a
/// misbehaving reverse proxy can return a multi-KB HTML stub.
/// Including it verbatim in `Error::Api` holds that much in memory
/// and pollutes operator logs. 1 KiB easily covers any real
/// Forgejo error response while bounding the worst case.
pub(crate) fn cap_for_error(s: &str) -> String {
    const CAP: usize = 1024;
    if s.len() <= CAP {
        return s.to_string();
    }
    let mut cut = CAP;
    while cut > 0 && !s.is_char_boundary(cut) {
        cut -= 1;
    }
    format!("{}… [+{} bytes]", &s[..cut], s.len() - cut)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{CommitStatus, CommitStatusState, ReviewComment, ReviewEvent};
    use wiremock::matchers::{body_json, header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    async fn mock_client() -> (MockServer, Client) {
        let server = MockServer::start().await;
        let client = Client::new(&server.uri(), "test-token").expect("client");
        (server, client)
    }

    #[test]
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
    fn base_url_with_trailing_slash_works() {
        let client = Client::new("https://example.com/forgejo/", "tok").expect("client");
        let url = client.url("repos/x/y/pulls/1.diff").expect("url");
        assert!(url.as_str().contains("/forgejo/api/v1/"));
    }

    #[test]
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

    #[tokio::test]
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
}

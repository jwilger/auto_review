use crate::types::{
    ChangedFile, CommitStatus, CreateReviewRequest, CreateWebhookRequest, CreatedWebhook,
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
        let base = Url::parse(base_url).map_err(|_| Error::InvalidBaseUrl(base_url.to_string()))?;
        let mut headers = HeaderMap::new();
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_str(&format!("token {token}"))
                .map_err(|_| Error::InvalidBaseUrl("non-ascii token".into()))?,
        );
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
                body,
            });
        }
        Ok(body)
    }

    /// List changed files for a pull request, including patch hunks.
    pub async fn list_changed_files(
        &self,
        owner: &str,
        repo: &str,
        n: u64,
    ) -> Result<Vec<ChangedFile>, Error> {
        let url = self.url(&format!("repos/{owner}/{repo}/pulls/{n}/files"))?;
        json_get(&self.http, url).await
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
            return Err(Error::Api {
                status: s.as_u16(),
                body: resp.text().await.unwrap_or_default(),
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
            body,
        });
    }
    serde_json::from_str(&body).map_err(|e| Error::Api {
        status: 200,
        body: format!("decode error: {e}: {body}"),
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
            body,
        });
    }
    serde_json::from_str(&body).map_err(|e| Error::Api {
        status: 200,
        body: format!("decode error: {e}: {body}"),
    })
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

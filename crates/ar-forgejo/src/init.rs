//! Bootstrap helpers used by `auto_review init`.
//!
//! Forgejo's "create access token" endpoint requires HTTP Basic auth, not a
//! token, since the bot doesn't have a token yet at init time. [`InitClient`]
//! is a thin separate client that speaks Basic auth so we don't have to
//! complicate the main token-based [`Client`](crate::Client) for a
//! once-per-install operation.

use crate::client::Error;
use crate::types::{CreateAccessTokenRequest, CreatedAccessToken};
use reqwest::header::{HeaderMap, HeaderValue, ACCEPT, AUTHORIZATION, CONTENT_TYPE, USER_AGENT};
use url::Url;

/// One-shot client for setup operations that pre-date the bot's PAT.
#[derive(Debug, Clone)]
pub struct InitClient {
    http: reqwest::Client,
    base: Url,
}

impl InitClient {
    pub fn new(base_url: &str, username: &str, password: &str) -> Result<Self, Error> {
        let base = Url::parse(base_url).map_err(|_| Error::InvalidBaseUrl(base_url.to_string()))?;
        let mut headers = HeaderMap::new();
        let basic = base64_basic(username, password);
        let mut auth_value = HeaderValue::from_str(&format!("Basic {basic}"))
            .map_err(|_| Error::InvalidBaseUrl("non-ascii credentials".into()))?;
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
        let trimmed = path.trim_start_matches('/');
        Ok(self.base.join("api/v1/")?.join(trimmed)?)
    }

    /// Mint a personal access token for `username` with the given scopes.
    /// `username` must match the basic-auth principal — Forgejo rejects
    /// cross-user token minting.
    pub async fn create_access_token(
        &self,
        username: &str,
        request: &CreateAccessTokenRequest,
    ) -> Result<CreatedAccessToken, Error> {
        let url = self.url(&format!("users/{username}/tokens"))?;
        let resp = self.http.post(url).json(request).send().await?;
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
}

fn base64_basic(user: &str, pass: &str) -> String {
    use base64::Engine;
    let raw = format!("{user}:{pass}");
    base64::engine::general_purpose::STANDARD.encode(raw.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{body_partial_json, header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn create_access_token_sends_basic_auth_and_decodes_response() {
        let server = MockServer::start().await;
        let client = InitClient::new(&server.uri(), "alice", "hunter2").expect("client");

        // base64("alice:hunter2") = YWxpY2U6aHVudGVyMg==
        Mock::given(method("POST"))
            .and(path("/api/v1/users/alice/tokens"))
            .and(header("Authorization", "Basic YWxpY2U6aHVudGVyMg=="))
            .and(body_partial_json(serde_json::json!({
                "name": "auto_review",
                "scopes": ["write:repository", "write:issue"]
            })))
            .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({
                "id": 99,
                "name": "auto_review",
                "sha1": "tok_secret_value",
                "scopes": ["write:repository", "write:issue"]
            })))
            .mount(&server)
            .await;

        let req = CreateAccessTokenRequest {
            name: "auto_review".into(),
            scopes: vec!["write:repository".into(), "write:issue".into()],
        };
        let token = client.create_access_token("alice", &req).await.expect("ok");
        assert_eq!(token.id, 99);
        assert_eq!(token.name, "auto_review");
        assert_eq!(token.sha1, "tok_secret_value");
    }

    #[tokio::test]
    async fn create_access_token_propagates_401() {
        let server = MockServer::start().await;
        let client = InitClient::new(&server.uri(), "alice", "wrong").expect("client");

        Mock::given(method("POST"))
            .and(path("/api/v1/users/alice/tokens"))
            .respond_with(ResponseTemplate::new(401).set_body_string("unauthorized"))
            .mount(&server)
            .await;

        let err = client
            .create_access_token(
                "alice",
                &CreateAccessTokenRequest {
                    name: "x".into(),
                    scopes: vec![],
                },
            )
            .await
            .expect_err("err");
        match err {
            Error::Api { status, .. } => assert_eq!(status, 401),
            other => panic!("unexpected: {other:?}"),
        }
    }
}

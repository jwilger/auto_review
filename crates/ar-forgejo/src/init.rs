//! Bootstrap helpers used by `auto_review init`.
//!
//! Forgejo's "create access token" endpoint requires HTTP Basic auth, not a
//! token, since the bot doesn't have a token yet at init time. [`InitClient`]
//! is a thin separate client that speaks Basic auth so we don't have to
//! complicate the main token-based [`Client`](crate::Client) for a
//! once-per-install operation.

use crate::cap_for_error;
use crate::client::Error;
use crate::types::{CreateAccessTokenRequest, CreatedAccessToken};
use base64::engine::general_purpose::STANDARD;
use base64::Engine as _;
use reqwest::header::{HeaderMap, HeaderValue, ACCEPT, AUTHORIZATION, CONTENT_TYPE, USER_AGENT};
use url::Url;

/// One-shot client for setup operations that pre-date the bot's PAT.
#[derive(Debug, Clone)]
#[expect(
    clippy::module_name_repetitions,
    reason = "ar-cli (commands) imports this as ar_forgejo::InitClient; renaming it breaks that \
              import"
)]
pub struct InitClient {
    /// Normalised server base URL, always ending in `/`.
    base: Url,
    /// HTTP client carrying the Basic-auth and JSON default headers.
    http: reqwest::Client,
}

impl InitClient {
    /// Mint a personal access token for `username` with the given scopes.
    /// `username` must match the basic-auth principal — Forgejo rejects
    /// cross-user token minting.
    ///
    /// # Errors
    ///
    /// Returns an error when the request fails, Forgejo answers with a
    /// non-success status, or the response does not decode.
    #[inline]
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
                body: cap_for_error(&body),
            });
        }
        serde_json::from_str(&body).map_err(|error| Error::Api {
            status: 200,
            body: format!("decode error: {error}: {}", cap_for_error(&body)),
        })
    }

    /// Build a client for the Forgejo server at `base_url` that
    /// authenticates with HTTP Basic auth as `username`.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidBaseUrl`] when `base_url` is not a URL or the
    /// credentials are not a valid header value, and [`Error::Http`] when the
    /// HTTP client cannot be built.
    #[inline]
    pub fn new(base_url: &str, username: &str, password: &str) -> Result<Self, Error> {
        // Same subpath-deploy normalisation as `Client::new`. Forgejo
        // mounted at `https://example.com/forgejo` would otherwise
        // silently lose its subpath when joining `api/v1/`, leading
        // to confusing 404s during `auto_review init` — and init is
        // operators' first impression of the bot.
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
        let basic = base64_basic(username, password);
        #[expect(
            clippy::map_err_ignore,
            reason = "InvalidHeaderValue carries no detail beyond its kind, and the error \
                      text must never echo the credentials it rejected"
        )]
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
        Ok(Self { base, http })
    }

    /// Resolve an API path such as `users/alice/tokens` against
    /// `{base}api/v1/`.
    fn url(&self, path: &str) -> Result<Url, Error> {
        let trimmed = path.trim_start_matches('/');
        Ok(self.base.join("api/v1/")?.join(trimmed)?)
    }
}

/// Encode `user:pass` as the base64 token of an HTTP Basic `Authorization`
/// header.
fn base64_basic(user: &str, pass: &str) -> String {
    let raw = format!("{user}:{pass}");
    STANDARD.encode(raw.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{body_partial_json, header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    #[expect(
        clippy::default_numeric_fallback,
        reason = "serde_json::json! numbers here are untyped JSON wire values in the mocked Forgejo payload; type suffixes would edit the fixture without changing the bytes sent"
    )]
    #[expect(
        clippy::expect_used,
        reason = "a failed request, decode or setup step should fail this test at the named step; the asserted values stay as written"
    )]
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

    #[test]
    #[expect(
        clippy::expect_used,
        reason = "a failed request, decode or setup step should fail this test at the named step; the asserted values stay as written"
    )]
    fn init_client_subpath_base_url_keeps_path_when_joined() {
        // Same defence-in-depth as the main client. Operators
        // running `auto_review init` against a subpath-deployed
        // Forgejo (e.g. https://example.com/forgejo) shouldn't
        // silently 404 because the base URL lost its subpath when
        // building the token-mint URL.
        let client = InitClient::new("https://example.com/forgejo", "alice", "pw").expect("client");
        let url = client.url("users/alice/tokens").expect("url");
        assert!(
            url.as_str().contains("/forgejo/api/v1/users/alice/tokens"),
            "subpath was dropped: {url}"
        );
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

//! OpenAI-compatible chat-completions + embeddings client.
//!
//! Works against any backend speaking `OpenAI`'s `/v1/chat/completions` and
//! `/v1/embeddings` shape: hosted `OpenAI`, Ollama, vLLM, `OpenRouter`,
//! Together.ai, Groq, etc. The base URL is configurable so the same
//! provider implementation serves all of them.

#[cfg(test)]
use crate::types::Message;
use crate::types::{CompleteRequest, CompleteResponse, Error, LlmProvider, ResponseFormat, Role};
use async_trait::async_trait;
use reqwest::header::{HeaderMap, HeaderValue, ACCEPT, AUTHORIZATION, CONTENT_TYPE};
use serde::{Deserialize, Serialize};
use url::Url;

#[derive(Debug, Clone)]
pub struct OpenAiProvider {
    /// API root, always ending in `/` so joins keep any subpath.
    base: Url,
    /// Model named in every `/v1/chat/completions` request.
    chat_model: String,
    /// Optional Ollama `options.num_ctx` sent on every `/v1/embeddings`
    /// POST. Hosted `OpenAI` ignores unknown fields, but Ollama uses
    /// it to size the model's context window for that request — so
    /// inputs sized to the operator's configured byte cap aren't
    /// silently truncated by Ollama's default `num_ctx=2048`.
    embed_num_ctx: Option<u32>,
    /// Model named in `/v1/embeddings` requests; `None` makes `embed`
    /// return [`Error::Unsupported`].
    embedding_model: Option<String>,
    /// HTTP client carrying the JSON and optional bearer-auth headers.
    http: reqwest::Client,
}

impl OpenAiProvider {
    /// `base_url` should be the API root, e.g. `https://api.openai.com` or
    /// `http://localhost:11434` for Ollama.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidBaseUrl`] when `base_url` does not parse or
    /// `api_key` is not a valid header value, and [`Error::Http`] when the
    /// HTTP client cannot be built.
    #[inline]
    pub fn new(base_url: &str, api_key: Option<&str>, chat_model: &str) -> Result<Self, Error> {
        // Same subpath-deploy normalisation as the Forgejo client.
        // OpenRouter and self-hosted LLM gateways are sometimes
        // mounted under a path (e.g. https://example.com/openrouter);
        // without trailing-slash normalisation Url::join("v1/")
        // would silently drop the subpath and every LLM call would
        // hit the wrong endpoint.
        let normalized = if base_url.ends_with('/') {
            base_url.to_owned()
        } else {
            format!("{base_url}/")
        };
        #[expect(
            clippy::map_err_ignore,
            reason = "InvalidBaseUrl carries the normalized URL as its whole message; appending \
                      the parse error would change the error text callers see"
        )]
        let base =
            Url::parse(&normalized).map_err(|_| Error::InvalidBaseUrl(normalized.clone()))?;
        let mut headers = HeaderMap::new();
        headers.insert(ACCEPT, HeaderValue::from_static("application/json"));
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        if let Some(key) = api_key {
            let auth = format!("Bearer {key}");
            #[expect(
                clippy::map_err_ignore,
                reason = "the error names only the api key setting so nothing derived from the \
                          secret header value reaches the message"
            )]
            let mut hv = HeaderValue::from_str(&auth)
                .map_err(|_| Error::InvalidBaseUrl("api key".into()))?;
            hv.set_sensitive(true);
            headers.insert(AUTHORIZATION, hv);
        }
        let http = reqwest::Client::builder()
            .default_headers(headers)
            .build()?;
        Ok(Self {
            base,
            chat_model: chat_model.to_owned(),
            embed_num_ctx: None,
            embedding_model: None,
            http,
        })
    }

    /// Build the endpoint URL for `path` under `<base>/v1/`.
    fn url(&self, path: &str) -> Result<Url, Error> {
        self.base
            .join("v1/")
            .and_then(|v1| v1.join(path.trim_start_matches('/')))
            .map_err(|err| Error::InvalidBaseUrl(err.to_string()))
    }

    /// Set Ollama's `options.num_ctx` for embedding requests. Use
    /// when pointing the embedding tier at Ollama (or any backend
    /// that honours the Ollama options block) so inputs the size
    /// of the operator's configured byte cap aren't silently
    /// truncated by the server's default 2048-token context. Has
    /// no effect on hosted `OpenAI` (extra fields are ignored).
    #[inline]
    #[must_use = "the builder returns the configured provider; dropping it discards the setting"]
    pub fn with_embed_num_ctx(mut self, num_ctx: u32) -> Self {
        self.embed_num_ctx = Some(num_ctx);
        self
    }

    #[inline]
    #[must_use = "the builder returns the configured provider; dropping it discards the setting"]
    pub fn with_embedding_model(mut self, model: impl Into<String>) -> Self {
        self.embedding_model = Some(model.into());
        self
    }
}

#[async_trait]
impl LlmProvider for OpenAiProvider {
    #[inline]
    async fn complete(&self, req: CompleteRequest) -> Result<CompleteResponse, Error> {
        let body = ChatRequest::from_request(&self.chat_model, &req);
        let url = self.url("chat/completions")?;
        let resp = self.http.post(url).json(&body).send().await?;
        let status = resp.status();
        let text = resp.text().await?;
        if !status.is_success() {
            return Err(Error::Provider {
                status: status.as_u16(),
                body: cap_for_error(&text),
            });
        }
        let parsed: ChatResponse = serde_json::from_str(&text)
            .map_err(|err| Error::Decode(format!("{err}: {}", cap_for_error(&text))))?;
        let content = parsed
            .choices
            .into_iter()
            .next()
            .and_then(|choice| choice.message.content)
            .unwrap_or_default();
        let usage = parsed.usage.unwrap_or_default();
        Ok(CompleteResponse {
            content,
            input_tokens: usage.prompt_tokens,
            output_tokens: usage.completion_tokens,
        })
    }

    #[inline]
    fn completion_model_name(&self) -> String {
        self.chat_model.clone()
    }

    #[inline]
    async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, Error> {
        Ok(self.embed_with_usage(texts).await?.0)
    }

    #[inline]
    async fn embed_with_usage(&self, texts: &[String]) -> Result<(Vec<Vec<f32>>, u32, u32), Error> {
        let model = self.embedding_model.as_deref().ok_or(Error::Unsupported)?;
        let body = EmbedRequest {
            model,
            input: texts,
            options: self.embed_num_ctx.map(|num_ctx| EmbedOptions { num_ctx }),
        };
        let url = self.url("embeddings")?;
        let resp = self.http.post(url).json(&body).send().await?;
        let status = resp.status();
        let text = resp.text().await?;
        if !status.is_success() {
            return Err(Error::Provider {
                status: status.as_u16(),
                body: cap_for_error(&text),
            });
        }
        let parsed: EmbedResponse = serde_json::from_str(&text)
            .map_err(|err| Error::Decode(format!("{err}: {}", cap_for_error(&text))))?;
        let vectors = parsed
            .data
            .into_iter()
            .map(|datum| datum.embedding)
            .collect();
        let usage = parsed.usage.unwrap_or_default();
        Ok((vectors, usage.prompt_tokens, usage.completion_tokens))
    }

    #[inline]
    fn embedding_model_name(&self) -> String {
        self.embedding_model
            .clone()
            .unwrap_or_else(|| "embedding-model".to_owned())
    }

    #[inline]
    fn provider_base_url(&self) -> String {
        self.base.as_str().trim_end_matches('/').to_owned()
    }
}

// ---- wire types ----

/// Body of a `/v1/chat/completions` request.
#[derive(Debug, Serialize)]
#[expect(
    clippy::arbitrary_source_item_ordering,
    reason = "field order is the JSON key order of the chat request sent to the provider; \
              reordering changes the request body on the wire"
)]
struct ChatRequest<'req> {
    /// Chat model name.
    model: &'req str,
    /// Optional system prompt followed by the conversation.
    messages: Vec<WireMessage<'req>>,
    /// Completion token cap.
    #[serde(skip_serializing_if = "Option::is_none")]
    max_tokens: Option<u32>,
    /// Sampling temperature.
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f32>,
    /// Structured-output constraint.
    #[serde(skip_serializing_if = "Option::is_none")]
    response_format: Option<WireResponseFormat<'req>>,
}

impl<'req> ChatRequest<'req> {
    /// Borrow `req` into the wire shape, prepending the system prompt as
    /// a `system` message.
    fn from_request(model: &'req str, req: &'req CompleteRequest) -> Self {
        let mut messages: Vec<WireMessage> =
            Vec::with_capacity(req.messages.len().saturating_add(1));
        if let Some(sys) = req.system.as_deref() {
            messages.push(WireMessage {
                role: "system",
                content: sys,
            });
        }
        for message in &req.messages {
            messages.push(WireMessage {
                role: role_str(message.role),
                content: message.content.as_str(),
            });
        }
        #[expect(
            clippy::pattern_type_mismatch,
            reason = "borrowing name and schema out of &ResponseFormat needs either default \
                      binding modes or `ref` patterns, and clippy::ref_patterns forbids `ref`"
        )]
        let response_format = req.response_format.as_ref().map(|rf| match rf {
            ResponseFormat::Text => WireResponseFormat::Text,
            ResponseFormat::JsonSchema { name, schema } => WireResponseFormat::JsonSchema {
                json_schema: WireJsonSchema {
                    name,
                    schema,
                    strict: true,
                },
            },
        });
        Self {
            model,
            messages,
            max_tokens: req.max_tokens,
            temperature: req.temperature,
            response_format,
        }
    }
}

/// One chat message on the wire.
#[derive(Debug, Serialize)]
#[expect(
    clippy::arbitrary_source_item_ordering,
    reason = "field order is the JSON key order of each message sent to the provider; \
              reordering changes the request body on the wire"
)]
struct WireMessage<'req> {
    /// `system`, `user`, `assistant` or `tool`.
    role: &'req str,
    /// Message text.
    content: &'req str,
}

/// `response_format` on the wire.
#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum WireResponseFormat<'req> {
    /// Strict JSON Schema output.
    JsonSchema {
        /// Named schema with `strict: true`.
        json_schema: WireJsonSchema<'req>,
    },
    /// Free text output.
    Text,
}

/// `response_format.json_schema` on the wire.
#[derive(Debug, Serialize)]
struct WireJsonSchema<'req> {
    /// Schema name.
    name: &'req str,
    /// The JSON Schema itself.
    schema: &'req serde_json::Value,
    /// Always `true`: ask the provider to enforce the schema.
    strict: bool,
}

/// `/v1/chat/completions` response body.
#[derive(Debug, Deserialize)]
struct ChatResponse {
    /// Candidate completions; only the first is used.
    choices: Vec<ChatChoice>,
    /// Token usage, when the provider reports it.
    #[serde(default)]
    usage: Option<Usage>,
}

/// One candidate completion.
#[derive(Debug, Deserialize)]
struct ChatChoice {
    /// The assistant message.
    message: ChatMessage,
}

/// Assistant message in a completion.
#[derive(Debug, Deserialize)]
struct ChatMessage {
    /// Message text; `null` or absent maps to empty.
    #[serde(default)]
    content: Option<String>,
}

/// Token usage reported by the provider.
#[derive(Debug, Default, Deserialize)]
struct Usage {
    /// Output tokens.
    #[serde(default)]
    completion_tokens: u32,
    /// Input tokens.
    #[serde(default)]
    prompt_tokens: u32,
}

/// Body of a `/v1/embeddings` request.
#[derive(Debug, Serialize)]
#[expect(
    clippy::arbitrary_source_item_ordering,
    reason = "field order is the JSON key order of the embeddings request sent to the provider; \
              reordering changes the request body on the wire"
)]
struct EmbedRequest<'req> {
    /// Embedding model name.
    model: &'req str,
    /// Texts to embed.
    input: &'req [String],
    /// Ollama options block.
    #[serde(skip_serializing_if = "Option::is_none")]
    options: Option<EmbedOptions>,
}

/// Ollama `options` block for embeddings.
#[derive(Debug, Serialize)]
struct EmbedOptions {
    /// Context window size in tokens.
    num_ctx: u32,
}

/// `/v1/embeddings` response body.
#[derive(Debug, Deserialize)]
struct EmbedResponse {
    /// One vector per input, in input order.
    data: Vec<EmbedDatum>,
    /// Token usage, when the provider reports it.
    #[serde(default)]
    usage: Option<Usage>,
}

/// One embedding vector.
#[derive(Debug, Deserialize)]
struct EmbedDatum {
    /// The vector.
    embedding: Vec<f32>,
}

/// Cap the response body included in error messages. A misbehaving
/// proxy or provider can return an HTML error page that's hundreds
/// of KB; including it verbatim in `Error::Provider` / `Error::Decode`
/// holds that much in memory and pollutes operator logs. 1 KiB is
/// enough to surface the actual error message at the top of the
/// body without dragging the whole HTML stub along.
fn cap_for_error(body: &str) -> String {
    /// Maximum bytes of `body` kept in the message.
    const CAP: usize = 1024;
    if body.len() <= CAP {
        return body.to_owned();
    }
    let mut cut = CAP;
    while cut > 0 && !body.is_char_boundary(cut) {
        cut = cut.saturating_sub(1);
    }
    let head = body.get(..cut).unwrap_or_default();
    format!("{head}… [+{} bytes]", body.len().saturating_sub(cut))
}

/// Wire name of a [`Role`].
fn role_str(role: Role) -> &'static str {
    match role {
        Role::User => "user",
        Role::Assistant => "assistant",
        Role::Tool => "tool",
    }
}

#[cfg(test)]
#[expect(
    clippy::expect_used,
    reason = "test setup and calls under test use expect so a broken fixture or an unexpected \
              error fails the test at that line with a label"
)]
#[expect(
    clippy::default_numeric_fallback,
    reason = "untyped numbers inside the json! wiremock bodies are JSON values, not Rust \
              arithmetic; the mock bodies stay as written"
)]
mod tests {
    use super::*;
    use wiremock::matchers::{body_partial_json, header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[test]
    fn subpath_base_url_keeps_path_when_joined() {
        // Same defence as the Forgejo client. LLM gateways are
        // sometimes mounted under a path
        // (e.g. https://example.com/openrouter); without
        // trailing-slash normalisation Url::join("v1/") would
        // resolve relative to the last path component and silently
        // drop the subpath.
        let provider =
            OpenAiProvider::new("https://example.com/openrouter", None, "model").expect("provider");
        let url = provider.url("chat/completions").expect("url");
        assert!(
            url.as_str().contains("/openrouter/v1/chat/completions"),
            "subpath was dropped: {url}"
        );
    }

    #[test]
    fn root_base_url_works_with_or_without_trailing_slash() {
        for input in ["https://api.openai.com", "https://api.openai.com/"] {
            let provider = OpenAiProvider::new(input, None, "model").expect("provider");
            let url = provider.url("chat/completions").expect("url");
            assert_eq!(
                url.as_str(),
                "https://api.openai.com/v1/chat/completions",
                "input = {input}"
            );
        }
    }

    #[test]
    fn cap_for_error_passes_short_strings_through() {
        assert_eq!(cap_for_error("brief error"), "brief error");
    }

    #[test]
    fn cap_for_error_truncates_long_bodies_with_byte_count() {
        // A proxy returning a 200KB HTML error page would otherwise
        // hold 200KB in Error::Provider and pollute operator logs.
        let big = format!("<html>{}</html>", "x".repeat(200_000));
        let out = cap_for_error(&big);
        assert!(out.len() < 1300, "output was {} bytes", out.len());
        assert!(out.starts_with("<html>"));
        assert!(out.contains("[+"));
        assert!(out.contains("bytes]"));
    }

    #[test]
    #[expect(
        clippy::absolute_paths,
        reason = "`std::str::from_utf8` is inside the assertion below, which this lint-only \
                  change leaves untouched"
    )]
    #[expect(
        clippy::std_instead_of_core,
        reason = "`std::str::from_utf8` is inside the assertion below, which this lint-only \
                  change leaves untouched"
    )]
    #[expect(
        clippy::assertions_on_result_states,
        reason = "the assertion below checks only that the capped output is valid UTF-8; it \
                  stays unedited in this lint-only change"
    )]
    fn cap_for_error_respects_utf8_boundaries() {
        let mut body = "x".repeat(1022);
        body.push_str("\u{1f980}tail"); // emoji at the boundary
        let out = cap_for_error(&body);
        assert!(std::str::from_utf8(out.as_bytes()).is_ok());
    }

    #[tokio::test]
    async fn complete_sends_model_messages_and_returns_content() {
        let server = MockServer::start().await;
        let provider =
            OpenAiProvider::new(&server.uri(), Some("sk-test"), "gpt-4o-mini").expect("provider");

        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .and(header("Authorization", "Bearer sk-test"))
            .and(body_partial_json(serde_json::json!({
                "model": "gpt-4o-mini",
                "messages": [
                    {"role": "system", "content": "be brief"},
                    {"role": "user", "content": "hi"}
                ]
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "choices": [{"message": {"content": "hello"}}],
                "usage": {"prompt_tokens": 5, "completion_tokens": 1}
            })))
            .mount(&server)
            .await;

        let req = CompleteRequest {
            system: Some("be brief".into()),
            messages: vec![Message::user("hi")],
            ..Default::default()
        };
        let resp = provider.complete(req).await.expect("ok");
        assert_eq!(resp.content, "hello");
        assert_eq!(resp.input_tokens, 5);
        assert_eq!(resp.output_tokens, 1);
    }

    #[tokio::test]
    async fn complete_serializes_json_schema_response_format() {
        let server = MockServer::start().await;
        let provider = OpenAiProvider::new(&server.uri(), None, "qwen2.5").expect("provider");

        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .and(body_partial_json(serde_json::json!({
                "response_format": {
                    "type": "json_schema",
                    "json_schema": {
                        "name": "Review",
                        "strict": true,
                        "schema": {"type": "object"}
                    }
                }
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "choices": [{"message": {"content": "{}"}}]
            })))
            .mount(&server)
            .await;

        let req = CompleteRequest {
            messages: vec![Message::user("emit json")],
            response_format: Some(ResponseFormat::JsonSchema {
                name: "Review".into(),
                schema: serde_json::json!({"type": "object"}),
            }),
            ..Default::default()
        };
        let resp = provider.complete(req).await.expect("ok");
        assert_eq!(resp.content, "{}");
    }

    #[tokio::test]
    #[expect(
        clippy::min_ident_chars,
        reason = "`v` is read by the assertions below; renaming it would edit those assertion \
                  lines, which this lint-only change leaves untouched"
    )]
    #[expect(
        clippy::indexing_slicing,
        reason = "the assertion below indexes the first vector after asserting the length; it \
                  stays unedited in this lint-only change"
    )]
    async fn embed_returns_vectors_when_model_configured() {
        let server = MockServer::start().await;
        let provider = OpenAiProvider::new(&server.uri(), None, "ignored")
            .expect("provider")
            .with_embedding_model("text-embedding-3-small");

        Mock::given(method("POST"))
            .and(path("/v1/embeddings"))
            .and(body_partial_json(serde_json::json!({
                "model": "text-embedding-3-small",
                "input": ["a", "b"]
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": [
                    {"embedding": [0.1, 0.2]},
                    {"embedding": [0.3, 0.4]}
                ]
            })))
            .mount(&server)
            .await;

        let v = provider.embed(&["a".into(), "b".into()]).await.expect("ok");
        assert_eq!(v.len(), 2);
        assert_eq!(v[0], vec![0.1, 0.2]);
    }

    #[tokio::test]
    #[expect(
        clippy::min_ident_chars,
        reason = "`v` is read by the assertion below; renaming it would edit that assertion \
                  line, which this lint-only change leaves untouched"
    )]
    async fn embed_includes_options_num_ctx_when_configured() {
        // Pointing the embedding tier at Ollama: explicit options.num_ctx
        // keeps the server from silently truncating to its default 2048
        // even though the operator raised the byte cap.
        let server = MockServer::start().await;
        let provider = OpenAiProvider::new(&server.uri(), None, "ignored")
            .expect("provider")
            .with_embedding_model("nomic-embed-text")
            .with_embed_num_ctx(4096);

        Mock::given(method("POST"))
            .and(path("/v1/embeddings"))
            .and(body_partial_json(serde_json::json!({
                "model": "nomic-embed-text",
                "input": ["a"],
                "options": {"num_ctx": 4096}
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": [{"embedding": [0.1]}]
            })))
            .mount(&server)
            .await;

        let v = provider.embed(&["a".into()]).await.expect("ok");
        assert_eq!(v.len(), 1);
    }

    #[tokio::test]
    #[expect(
        clippy::indexing_slicing,
        reason = "reads the only received request right after the assertion that exactly one \
                  request arrived"
    )]
    #[expect(
        clippy::absolute_paths,
        reason = "the qualified path is part of the wiremock matcher expectation, which this \
                  lint-only change leaves untouched"
    )]
    async fn embed_omits_options_field_when_not_configured() {
        // Hosted OpenAI ignores extra fields, but emitting an empty
        // `options: {}` (or worse, `options: null`) would still hit
        // the wire. Guard against that.
        let server = MockServer::start().await;
        let provider = OpenAiProvider::new(&server.uri(), None, "ignored")
            .expect("provider")
            .with_embedding_model("text-embedding-3-small");

        Mock::given(method("POST"))
            .and(path("/v1/embeddings"))
            .and(wiremock::matchers::body_string_contains(
                "text-embedding-3-small",
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": [{"embedding": [0.1]}]
            })))
            .mount(&server)
            .await;

        let _: Vec<Vec<f32>> = provider.embed(&["a".into()]).await.expect("ok");

        let received = server.received_requests().await.expect("requests");
        assert_eq!(received.len(), 1);
        let body: serde_json::Value = serde_json::from_slice(&received[0].body).expect("json body");
        assert!(
            body.get("options").is_none(),
            "expected no `options` field in default OpenAI embed body, got: {body}"
        );
    }

    #[tokio::test]
    async fn embed_without_model_is_unsupported() {
        let server = MockServer::start().await;
        let provider = OpenAiProvider::new(&server.uri(), None, "x").expect("provider");
        let err = provider
            .embed(&["a".into()])
            .await
            .expect_err("unsupported");
        assert!(matches!(err, Error::Unsupported));
    }

    #[tokio::test]
    #[expect(
        clippy::panic,
        reason = "the catch-all arm fails the test and names the unexpected error variant"
    )]
    #[expect(
        clippy::wildcard_enum_match_arm,
        reason = "every variant other than Provider is a test failure, so one catch-all arm \
                  reports them all"
    )]
    async fn complete_propagates_provider_error_status() {
        let server = MockServer::start().await;
        let provider = OpenAiProvider::new(&server.uri(), None, "x").expect("provider");

        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(ResponseTemplate::new(429).set_body_string("rate limited"))
            .mount(&server)
            .await;

        let err = provider
            .complete(CompleteRequest {
                messages: vec![Message::user("x")],
                ..Default::default()
            })
            .await
            .expect_err("err");
        match err {
            Error::Provider { status, body } => {
                assert_eq!(status, 429);
                assert_eq!(body, "rate limited");
            }
            other => panic!("unexpected: {other:?}"),
        }
    }
}

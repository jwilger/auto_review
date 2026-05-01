//! OpenAI-compatible chat-completions + embeddings client.
//!
//! Works against any backend speaking OpenAI's `/v1/chat/completions` and
//! `/v1/embeddings` shape: hosted OpenAI, Ollama, vLLM, OpenRouter,
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
    http: reqwest::Client,
    base: Url,
    chat_model: String,
    embedding_model: Option<String>,
}

impl OpenAiProvider {
    /// `base_url` should be the API root, e.g. `https://api.openai.com` or
    /// `http://localhost:11434` for Ollama.
    pub fn new(base_url: &str, api_key: Option<&str>, chat_model: &str) -> Result<Self, Error> {
        // Same subpath-deploy normalisation as the Forgejo client.
        // OpenRouter and self-hosted LLM gateways are sometimes
        // mounted under a path (e.g. https://example.com/openrouter);
        // without trailing-slash normalisation Url::join("v1/")
        // would silently drop the subpath and every LLM call would
        // hit the wrong endpoint.
        let normalized = if base_url.ends_with('/') {
            base_url.to_string()
        } else {
            format!("{base_url}/")
        };
        let base =
            Url::parse(&normalized).map_err(|_| Error::InvalidBaseUrl(normalized.clone()))?;
        let mut headers = HeaderMap::new();
        headers.insert(ACCEPT, HeaderValue::from_static("application/json"));
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        if let Some(key) = api_key {
            let auth = format!("Bearer {key}");
            let mut hv = HeaderValue::from_str(&auth)
                .map_err(|_| Error::InvalidBaseUrl("api key".into()))?;
            hv.set_sensitive(true);
            headers.insert(AUTHORIZATION, hv);
        }
        let http = reqwest::Client::builder()
            .default_headers(headers)
            .build()?;
        Ok(Self {
            http,
            base,
            chat_model: chat_model.to_string(),
            embedding_model: None,
        })
    }

    pub fn with_embedding_model(mut self, model: impl Into<String>) -> Self {
        self.embedding_model = Some(model.into());
        self
    }

    fn url(&self, path: &str) -> Result<Url, Error> {
        self.base
            .join("v1/")
            .and_then(|u| u.join(path.trim_start_matches('/')))
            .map_err(|e| Error::InvalidBaseUrl(e.to_string()))
    }
}

#[async_trait]
impl LlmProvider for OpenAiProvider {
    async fn complete(&self, req: CompleteRequest) -> Result<CompleteResponse, Error> {
        let body = ChatRequest::from_request(&self.chat_model, &req);
        let url = self.url("chat/completions")?;
        let resp = self.http.post(url).json(&body).send().await?;
        let status = resp.status();
        let text = resp.text().await?;
        if !status.is_success() {
            return Err(Error::Provider {
                status: status.as_u16(),
                body: text,
            });
        }
        let parsed: ChatResponse =
            serde_json::from_str(&text).map_err(|e| Error::Decode(format!("{e}: {text}")))?;
        let content = parsed
            .choices
            .into_iter()
            .next()
            .map(|c| c.message.content.unwrap_or_default())
            .unwrap_or_default();
        let usage = parsed.usage.unwrap_or_default();
        Ok(CompleteResponse {
            content,
            input_tokens: usage.prompt_tokens,
            output_tokens: usage.completion_tokens,
        })
    }

    async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, Error> {
        let model = self.embedding_model.as_deref().ok_or(Error::Unsupported)?;
        let body = EmbedRequest {
            model,
            input: texts,
        };
        let url = self.url("embeddings")?;
        let resp = self.http.post(url).json(&body).send().await?;
        let status = resp.status();
        let text = resp.text().await?;
        if !status.is_success() {
            return Err(Error::Provider {
                status: status.as_u16(),
                body: text,
            });
        }
        let parsed: EmbedResponse =
            serde_json::from_str(&text).map_err(|e| Error::Decode(format!("{e}: {text}")))?;
        Ok(parsed.data.into_iter().map(|d| d.embedding).collect())
    }
}

// ---- wire types ----

#[derive(Debug, Serialize)]
struct ChatRequest<'a> {
    model: &'a str,
    messages: Vec<WireMessage<'a>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    response_format: Option<WireResponseFormat<'a>>,
}

impl<'a> ChatRequest<'a> {
    fn from_request(model: &'a str, req: &'a CompleteRequest) -> Self {
        let mut messages: Vec<WireMessage> = Vec::with_capacity(req.messages.len() + 1);
        if let Some(sys) = req.system.as_deref() {
            messages.push(WireMessage {
                role: "system",
                content: sys,
            });
        }
        for m in &req.messages {
            messages.push(WireMessage {
                role: role_str(m.role),
                content: m.content.as_str(),
            });
        }
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

fn role_str(r: Role) -> &'static str {
    match r {
        Role::User => "user",
        Role::Assistant => "assistant",
        Role::Tool => "tool",
    }
}

#[derive(Debug, Serialize)]
struct WireMessage<'a> {
    role: &'a str,
    content: &'a str,
}

#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum WireResponseFormat<'a> {
    Text,
    JsonSchema { json_schema: WireJsonSchema<'a> },
}

#[derive(Debug, Serialize)]
struct WireJsonSchema<'a> {
    name: &'a str,
    schema: &'a serde_json::Value,
    strict: bool,
}

#[derive(Debug, Deserialize)]
struct ChatResponse {
    choices: Vec<ChatChoice>,
    #[serde(default)]
    usage: Option<Usage>,
}

#[derive(Debug, Deserialize)]
struct ChatChoice {
    message: ChatMessage,
}

#[derive(Debug, Deserialize)]
struct ChatMessage {
    #[serde(default)]
    content: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct Usage {
    #[serde(default)]
    prompt_tokens: u32,
    #[serde(default)]
    completion_tokens: u32,
}

#[derive(Debug, Serialize)]
struct EmbedRequest<'a> {
    model: &'a str,
    input: &'a [String],
}

#[derive(Debug, Deserialize)]
struct EmbedResponse {
    data: Vec<EmbedDatum>,
}

#[derive(Debug, Deserialize)]
struct EmbedDatum {
    embedding: Vec<f32>,
}

#[cfg(test)]
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

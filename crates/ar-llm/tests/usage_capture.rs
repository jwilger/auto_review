extern crate alloc;

use alloc::sync::Arc;
use ar_llm::{
    CompleteRequest, CompleteResponse, Error, LlmProvider, Message, ModelTier, OpenAiProvider,
    Router,
};
use async_trait::async_trait;
use core::sync::atomic::{AtomicU32, Ordering};
use std::sync::Mutex;
use wiremock::matchers::{body_partial_json, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

struct UsageProbeProvider {
    calls: AtomicU32,
    complete_response: CompleteResponse,
}

impl UsageProbeProvider {
    fn with_response(complete_response: CompleteResponse) -> Self {
        Self {
            calls: AtomicU32::new(0),
            complete_response,
        }
    }
}

#[async_trait]
impl LlmProvider for UsageProbeProvider {
    async fn complete(&self, _req: CompleteRequest) -> Result<CompleteResponse, Error> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(self.complete_response.clone())
    }

    async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, Error> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(vec![vec![0.0]; texts.len()])
    }
}

struct MetadataProbeProvider {
    base_url: &'static str,
    calls: AtomicU32,
    complete_response: CompleteResponse,
    embed_model: Option<&'static str>,
    model: &'static str,
}

#[async_trait]
impl LlmProvider for MetadataProbeProvider {
    async fn complete(&self, _req: CompleteRequest) -> Result<CompleteResponse, Error> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(self.complete_response.clone())
    }

    fn completion_model_name(&self) -> String {
        self.model.to_owned()
    }

    async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, Error> {
        if self.embed_model.is_none() {
            return Err(Error::Unsupported);
        }
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(vec![vec![0.0]; texts.len()])
    }

    fn embedding_model_name(&self) -> String {
        self.embed_model
            .map_or_else(|| "embedding-model".to_owned(), str::to_owned)
    }

    fn provider_base_url(&self) -> String {
        self.base_url.to_owned()
    }
}

#[tokio::test]
#[expect(
    clippy::tests_outside_test_module,
    reason = "files under tests/ are separate integration-test crates built only by cargo test; \
              there is no #[cfg(test)] module to place this in"
)]
#[expect(
    clippy::unwrap_used,
    reason = "a poisoned usage log means a collector panicked, which must fail the test"
)]
#[expect(
    clippy::expect_used,
    reason = "router calls use expect so an unexpected error fails the test at that line with a \
              label"
)]
#[expect(
    clippy::indexing_slicing,
    reason = "the assertions below index the usage log after asserting its length; they stay \
              unedited in this lint-only change"
)]
async fn router_usage_collector_records_complete_and_embedding_calls() {
    let observed_log = Arc::new(Mutex::new(Vec::<(ModelTier, String, u32, u32)>::new()));
    let observed_for_cb = Arc::clone(&observed_log);

    let router = Router::new()
        .with_usage_collector(
            move |tier, _provider_base_url, model, input_tokens, output_tokens| {
                observed_for_cb.lock().unwrap().push((
                    tier,
                    model.to_owned(),
                    input_tokens,
                    output_tokens,
                ));
            },
        )
        .with(
            ModelTier::Cheap,
            Arc::new(UsageProbeProvider::with_response(CompleteResponse {
                content: "complete".into(),
                input_tokens: 12,
                output_tokens: 3,
            })),
        )
        .with(
            ModelTier::Embedding,
            Arc::new(UsageProbeProvider::with_response(CompleteResponse {
                content: "embed".into(),
                input_tokens: 0,
                output_tokens: 0,
            })),
        );

    let _: CompleteResponse = router
        .complete(
            ModelTier::Cheap,
            CompleteRequest {
                messages: vec![Message::user("probe")],
                ..Default::default()
            },
        )
        .await
        .expect("completion should succeed");

    let _: Vec<Vec<f32>> = router
        .embed(ModelTier::Embedding, &["left".into(), "right".into()])
        .await
        .expect("embedding should succeed");

    let observed = observed_log.lock().unwrap();
    assert_eq!(observed.len(), 2);
    assert_eq!(observed[0].0, ModelTier::Cheap);
    assert_eq!(observed[0].1, "completion-model");
    assert_eq!(observed[0].2, 12);
    assert_eq!(observed[0].3, 3);
    assert_eq!(observed[1].0, ModelTier::Embedding);
    assert_eq!(observed[1].1, "embedding-model");
}

#[tokio::test]
#[expect(
    clippy::tests_outside_test_module,
    reason = "files under tests/ are separate integration-test crates built only by cargo test; \
              there is no #[cfg(test)] module to place this in"
)]
#[expect(
    clippy::unwrap_used,
    reason = "a poisoned usage log means a collector panicked, which must fail the test"
)]
#[expect(
    clippy::expect_used,
    reason = "router calls use expect so an unexpected error fails the test at that line with a \
              label"
)]
#[expect(
    clippy::indexing_slicing,
    reason = "the assertions below index the usage log after asserting its length; they stay \
              unedited in this lint-only change"
)]
async fn router_usage_collector_records_provider_and_model_names() {
    let observed_log = Arc::new(Mutex::new(
        Vec::<(ModelTier, String, String, u32, u32)>::new(),
    ));
    let observed_for_cb = Arc::clone(&observed_log);

    let router = Router::new()
        .with_usage_collector(
            move |tier, provider_base_url, model, input_tokens, output_tokens| {
                observed_for_cb.lock().unwrap().push((
                    tier,
                    provider_base_url.to_owned(),
                    model.to_owned(),
                    input_tokens,
                    output_tokens,
                ));
            },
        )
        .with(
            ModelTier::Cheap,
            Arc::new(MetadataProbeProvider {
                base_url: "https://cheap.example",
                calls: AtomicU32::new(0),
                complete_response: CompleteResponse {
                    content: "complete".into(),
                    input_tokens: 12,
                    output_tokens: 3,
                },
                embed_model: None,
                model: "gpt-4o-mini",
            }),
        )
        .with(
            ModelTier::Embedding,
            Arc::new(MetadataProbeProvider {
                base_url: "https://embeddings.example",
                calls: AtomicU32::new(0),
                complete_response: CompleteResponse {
                    content: "embed".into(),
                    input_tokens: 0,
                    output_tokens: 0,
                },
                embed_model: Some("text-embedding-ada-002"),
                model: "text-embedding-ada-002",
            }),
        );

    let _: CompleteResponse = router
        .complete(
            ModelTier::Cheap,
            CompleteRequest {
                messages: vec![Message::user("probe")],
                ..Default::default()
            },
        )
        .await
        .expect("completion should succeed");

    let _: Vec<Vec<f32>> = router
        .embed(
            ModelTier::Embedding,
            &["left".to_owned(), "right".to_owned()],
        )
        .await
        .expect("embedding should succeed");

    let observed = observed_log.lock().unwrap();
    assert_eq!(observed.len(), 2);
    assert_eq!(observed[0].0, ModelTier::Cheap);
    assert_eq!(observed[0].1, "https://cheap.example");
    assert_eq!(observed[0].2, "gpt-4o-mini");
    assert_eq!(observed[0].3, 12);
    assert_eq!(observed[0].4, 3);

    assert_eq!(observed[1].0, ModelTier::Embedding);
    assert_eq!(observed[1].1, "https://embeddings.example");
    assert_eq!(observed[1].2, "text-embedding-ada-002");
}

#[tokio::test]
#[expect(
    clippy::tests_outside_test_module,
    reason = "files under tests/ are separate integration-test crates built only by cargo test; \
              there is no #[cfg(test)] module to place this in"
)]
#[expect(
    clippy::unwrap_used,
    reason = "a poisoned usage log means a collector panicked, which must fail the test"
)]
#[expect(
    clippy::expect_used,
    reason = "provider setup and the router call use expect so an unexpected error fails the \
              test at that line with a label"
)]
#[expect(
    clippy::indexing_slicing,
    reason = "the assertions below index the usage log after asserting its length; they stay \
              unedited in this lint-only change"
)]
#[expect(
    clippy::default_numeric_fallback,
    reason = "untyped numbers inside the json! wiremock response body are JSON values; the mock \
              response stays as written"
)]
async fn router_usage_collector_records_embedding_prompt_tokens_from_openai_response() {
    let server = MockServer::start().await;
    let provider = OpenAiProvider::new(&server.uri(), None, "ignored")
        .expect("provider")
        .with_embedding_model("text-embedding-3-small");

    Mock::given(method("POST"))
        .and(path("/v1/embeddings"))
        .and(body_partial_json(serde_json::json!({
            "model": "text-embedding-3-small",
            "input": ["alpha", "beta"]
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "data": [
                {"embedding": [0.1, 0.2]},
                {"embedding": [0.3, 0.4]}
            ],
            "usage": {"prompt_tokens": 17, "completion_tokens": 0, "total_tokens": 17}
        })))
        .mount(&server)
        .await;

    let observed_log = Arc::new(Mutex::new(Vec::<(ModelTier, String, u32, u32)>::new()));
    let observed_for_cb = Arc::clone(&observed_log);

    let router = Router::new()
        .with_usage_collector(
            move |tier, _provider_base_url, model, input_tokens, output_tokens| {
                observed_for_cb.lock().unwrap().push((
                    tier,
                    model.to_owned(),
                    input_tokens,
                    output_tokens,
                ));
            },
        )
        .with(ModelTier::Embedding, Arc::new(provider));

    let vectors = router
        .embed(ModelTier::Embedding, &["alpha".into(), "beta".into()])
        .await
        .expect("embedding should succeed");

    assert_eq!(vectors, vec![vec![0.1, 0.2], vec![0.3, 0.4]]);

    let observed = observed_log.lock().unwrap();
    assert_eq!(observed.len(), 1);
    assert_eq!(observed[0].0, ModelTier::Embedding);
    assert_eq!(observed[0].2, 17);
    assert_eq!(observed[0].3, 0);
}

use ar_llm::{CompleteRequest, CompleteResponse, Error, LlmProvider, Message, ModelTier, Router};
use async_trait::async_trait;
use std::sync::{
    atomic::{AtomicU32, Ordering},
    Arc, Mutex,
};

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

#[tokio::test]
async fn router_usage_collector_records_complete_and_embedding_calls() {
    let observed = Arc::new(Mutex::new(Vec::<(ModelTier, String, u32, u32)>::new()));
    let observed_for_cb = observed.clone();

    let router = Router::new()
        .with_usage_collector(move |tier, model, input_tokens, output_tokens| {
            observed_for_cb
                .lock()
                .unwrap()
                .push((tier, model.to_string(), input_tokens, output_tokens));
        })
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

    let _ = router
        .complete(
            ModelTier::Cheap,
            CompleteRequest {
                messages: vec![Message::user("probe")],
                ..Default::default()
            },
        )
        .await
        .expect("completion should succeed");

    let _ = router
        .embed(ModelTier::Embedding, &["left".into(), "right".into()])
        .await
        .expect("embedding should succeed");

    let observed = observed.lock().unwrap();
    assert_eq!(observed.len(), 2);
    assert_eq!(observed[0].0, ModelTier::Cheap);
    assert_eq!(observed[0].1, "completion-model");
    assert_eq!(observed[0].2, 12);
    assert_eq!(observed[0].3, 3);
    assert_eq!(observed[1].0, ModelTier::Embedding);
    assert_eq!(observed[1].1, "embedding-model");
}

use crate::types::{CompleteRequest, CompleteResponse, Error, LlmProvider, ModelTier};
use std::collections::HashMap;
use std::sync::Arc;

type UsageCollector = Arc<dyn Fn(ModelTier, &str, &str, u32, u32) + Send + Sync>;

/// Maps `ModelTier` → provider instance.
///
/// Activities don't talk to providers directly — they ask the router for a
/// completion at a given tier, and the router routes. This makes swapping the
/// cheap-tier model from cloud to local a single config change.
#[derive(Clone, Default)]
pub struct Router {
    providers: HashMap<ModelTier, Arc<dyn LlmProvider>>,
    usage_collector: Option<UsageCollector>,
}

impl Router {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with(mut self, tier: ModelTier, provider: Arc<dyn LlmProvider>) -> Self {
        self.providers.insert(tier, provider);
        self
    }

    pub fn with_usage_collector<F>(mut self, collector: F) -> Self
    where
        F: Fn(ModelTier, &str, &str, u32, u32) + Send + Sync + 'static,
    {
        let collector = Arc::new(collector);
        self.usage_collector = match self.usage_collector {
            Some(existing) => Some(Arc::new(
                move |tier, provider_base_url, model_name, input_tokens, output_tokens| {
                    existing(
                        tier,
                        provider_base_url,
                        model_name,
                        input_tokens,
                        output_tokens,
                    );
                    collector(
                        tier,
                        provider_base_url,
                        model_name,
                        input_tokens,
                        output_tokens,
                    );
                },
            )),
            None => Some(collector),
        };
        self
    }

    pub fn provider(&self, tier: ModelTier) -> Result<&Arc<dyn LlmProvider>, Error> {
        self.providers.get(&tier).ok_or(Error::NoProvider(tier))
    }

    pub async fn complete(
        &self,
        tier: ModelTier,
        req: CompleteRequest,
    ) -> Result<CompleteResponse, Error> {
        let provider = self.provider(tier)?;
        let resp = provider.complete(req).await?;
        self.record_usage(&**provider, tier, resp.input_tokens, resp.output_tokens);
        Ok(resp)
    }

    pub async fn embed(&self, tier: ModelTier, texts: &[String]) -> Result<Vec<Vec<f32>>, Error> {
        let provider = self.provider(tier)?;
        let (vectors, input_tokens, output_tokens) = provider.embed_with_usage(texts).await?;
        self.record_usage(&**provider, tier, input_tokens, output_tokens);
        Ok(vectors)
    }

    fn record_usage(
        &self,
        provider: &dyn LlmProvider,
        tier: ModelTier,
        input_tokens: u32,
        output_tokens: u32,
    ) {
        if let Some(collector) = &self.usage_collector {
            let provider_base_url = provider.provider_base_url();
            let model_name = provider.provider_model_name(tier);
            collector(
                tier,
                &provider_base_url,
                &model_name,
                input_tokens,
                output_tokens,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{CompleteRequest, CompleteResponse, Message};
    use async_trait::async_trait;
    use std::sync::atomic::{AtomicU32, Ordering};

    struct CountingProvider {
        label: &'static str,
        calls: AtomicU32,
    }

    #[async_trait]
    impl LlmProvider for CountingProvider {
        async fn complete(&self, _req: CompleteRequest) -> Result<CompleteResponse, Error> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(CompleteResponse {
                content: self.label.into(),
                input_tokens: 0,
                output_tokens: 0,
            })
        }
    }

    #[tokio::test]
    async fn router_dispatches_by_tier() {
        let cheap = Arc::new(CountingProvider {
            label: "cheap",
            calls: AtomicU32::new(0),
        });
        let reasoning = Arc::new(CountingProvider {
            label: "reasoning",
            calls: AtomicU32::new(0),
        });
        let router = Router::new()
            .with(ModelTier::Cheap, cheap.clone())
            .with(ModelTier::Reasoning, reasoning.clone());

        let r = router
            .complete(
                ModelTier::Cheap,
                CompleteRequest {
                    messages: vec![Message::user("x")],
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert_eq!(r.content, "cheap");

        let r = router
            .complete(
                ModelTier::Reasoning,
                CompleteRequest {
                    messages: vec![Message::user("x")],
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert_eq!(r.content, "reasoning");

        assert_eq!(cheap.calls.load(Ordering::SeqCst), 1);
        assert_eq!(reasoning.calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn router_errors_when_tier_unconfigured() {
        let router = Router::new();
        let err = router
            .complete(
                ModelTier::Cheap,
                CompleteRequest {
                    messages: vec![Message::user("x")],
                    ..Default::default()
                },
            )
            .await
            .unwrap_err();
        assert!(matches!(err, Error::NoProvider(ModelTier::Cheap)));
    }
}

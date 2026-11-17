use crate::types::{CompleteRequest, CompleteResponse, Error, LlmProvider, ModelTier};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{Semaphore, SemaphorePermit};

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
    /// Process-wide cap on in-flight provider calls. Shared (never
    /// copied) across every clone of this router, including clones that
    /// add usage collectors, so the cap holds no matter how many agents a
    /// review fans out to. `None` means unlimited.
    inflight: Option<Arc<Semaphore>>,
}

impl Router {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with(mut self, tier: ModelTier, provider: Arc<dyn LlmProvider>) -> Self {
        self.providers.insert(tier, provider);
        self
    }

    /// Cap the number of simultaneous provider calls (`complete` and
    /// `embed`, across all tiers) made through this router and every
    /// clone of it. `0` is clamped to `1` so a zero setting can never
    /// deadlock, and values above `Semaphore::MAX_PERMITS` are clamped
    /// down to it so an oversized setting cannot panic.
    pub fn with_inflight_limit(mut self, n: usize) -> Self {
        self.inflight = Some(Arc::new(Semaphore::new(n.clamp(1, Semaphore::MAX_PERMITS))));
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
        // Resolve the provider first so configuration errors return at
        // once instead of queueing behind the in-flight limit.
        let provider = self.provider(tier)?;
        let resp = {
            let _permit = self.acquire_inflight().await;
            provider.complete(req).await
        }?;
        self.record_usage(&**provider, tier, resp.input_tokens, resp.output_tokens);
        Ok(resp)
    }

    pub async fn embed(&self, tier: ModelTier, texts: &[String]) -> Result<Vec<Vec<f32>>, Error> {
        let provider = self.provider(tier)?;
        let (vectors, input_tokens, output_tokens) = {
            let _permit = self.acquire_inflight().await;
            provider.embed_with_usage(texts).await
        }?;
        self.record_usage(&**provider, tier, input_tokens, output_tokens);
        Ok(vectors)
    }

    /// Wait for an in-flight slot when a limit is configured. The permit
    /// is RAII: dropping it (on success or error) frees the slot.
    async fn acquire_inflight(&self) -> Option<SemaphorePermit<'_>> {
        let semaphore = self.inflight.as_ref()?;
        // `acquire` only fails on a closed semaphore. The router never
        // closes it, but if that ever happened we proceed without a
        // permit rather than panic or fail the call.
        semaphore.acquire().await.ok()
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
    use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};
    use std::time::Duration;
    use tokio::sync::Notify;

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

    /// Records the peak number of simultaneous `complete`/`embed` calls.
    #[derive(Default)]
    struct PeakProvider {
        in_flight: AtomicUsize,
        peak: AtomicUsize,
    }

    impl PeakProvider {
        async fn enter_and_park(&self) {
            let now = self.in_flight.fetch_add(1, Ordering::SeqCst) + 1;
            self.peak.fetch_max(now, Ordering::SeqCst);
            tokio::time::sleep(Duration::from_millis(20)).await;
            self.in_flight.fetch_sub(1, Ordering::SeqCst);
        }
    }

    #[async_trait]
    impl LlmProvider for PeakProvider {
        async fn complete(&self, _req: CompleteRequest) -> Result<CompleteResponse, Error> {
            self.enter_and_park().await;
            Ok(CompleteResponse {
                content: "ok".into(),
                input_tokens: 0,
                output_tokens: 0,
            })
        }

        async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, Error> {
            self.enter_and_park().await;
            Ok(texts.iter().map(|_| vec![0.0]).collect())
        }
    }

    fn req() -> CompleteRequest {
        CompleteRequest {
            messages: vec![Message::user("x")],
            ..Default::default()
        }
    }

    #[derive(Clone, Copy)]
    enum Call {
        Complete,
        Embed,
    }

    fn peak_router(provider: &Arc<PeakProvider>) -> Router {
        Router::new()
            .with(ModelTier::Cheap, provider.clone())
            .with(ModelTier::Embedding, provider.clone())
    }

    /// Runs `tasks` concurrent calls through clones of `router`, asserts
    /// every call succeeded, and returns the provider's peak concurrency.
    async fn run_fan_out(
        router: Router,
        provider: Arc<PeakProvider>,
        tasks: usize,
        call: Call,
    ) -> usize {
        let handles: Vec<_> = (0..tasks)
            .map(|_| {
                let router = router.clone();
                tokio::spawn(async move {
                    match call {
                        Call::Complete => {
                            router.complete(ModelTier::Cheap, req()).await.map(|_| ())
                        }
                        Call::Embed => router
                            .embed(ModelTier::Embedding, &["t".to_string()])
                            .await
                            .map(|_| ()),
                    }
                })
            })
            .collect();
        for handle in handles {
            handle.await.unwrap().unwrap();
        }
        provider.peak.load(Ordering::SeqCst)
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn inflight_limit_caps_concurrent_complete_calls() {
        let n = 2;
        let provider = Arc::new(PeakProvider::default());
        let router = peak_router(&provider).with_inflight_limit(n);
        let peak = run_fan_out(router, provider, 3 * n, Call::Complete).await;
        assert!(peak <= n, "peak {peak} exceeded limit {n}");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn inflight_limit_caps_concurrent_embed_calls() {
        let n = 2;
        let provider = Arc::new(PeakProvider::default());
        let router = peak_router(&provider).with_inflight_limit(n);
        let peak = run_fan_out(router, provider, 3 * n, Call::Embed).await;
        assert!(peak <= n, "peak {peak} exceeded limit {n}");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn router_without_limit_is_unbounded() {
        let n = 2;
        let provider = Arc::new(PeakProvider::default());
        let peak = run_fan_out(peak_router(&provider), provider, 3 * n, Call::Complete).await;
        assert!(peak > n, "expected unlimited fan-out above {n}, got {peak}");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn collector_clone_shares_the_inflight_semaphore() {
        let n = 2;
        let provider = Arc::new(PeakProvider::default());
        let original = peak_router(&provider).with_inflight_limit(n);
        let augmented = original
            .clone()
            .with_usage_collector(|_, _, _, _, _| {})
            .with(ModelTier::Reasoning, provider.clone());
        let (Some(a), Some(b)) = (&original.inflight, &augmented.inflight) else {
            panic!("both routers should carry the in-flight semaphore");
        };
        assert!(Arc::ptr_eq(a, b));

        let handles: Vec<_> = (0..3 * n)
            .map(|i| {
                let router = if i % 2 == 0 {
                    original.clone()
                } else {
                    augmented.clone()
                };
                tokio::spawn(async move { router.complete(ModelTier::Cheap, req()).await })
            })
            .collect();
        for handle in handles {
            handle.await.unwrap().unwrap();
        }
        let peak = provider.peak.load(Ordering::SeqCst);
        assert!(peak <= n, "peak {peak} exceeded limit {n}");
    }

    #[tokio::test]
    async fn inflight_limit_zero_clamps_to_one() {
        let provider = Arc::new(PeakProvider::default());
        let router = peak_router(&provider).with_inflight_limit(0);
        let result = tokio::time::timeout(
            Duration::from_secs(5),
            router.complete(ModelTier::Cheap, req()),
        )
        .await;
        assert!(matches!(result, Ok(Ok(_))));
    }

    #[tokio::test]
    async fn inflight_limit_above_max_permits_clamps_instead_of_panicking() {
        let provider = Arc::new(PeakProvider::default());
        let router = peak_router(&provider).with_inflight_limit(usize::MAX);
        let result = router.complete(ModelTier::Cheap, req()).await;
        assert!(result.is_ok());
    }

    /// Parks inside `complete` until released, holding the router permit.
    struct ParkedProvider {
        entered: Arc<Notify>,
        release: Arc<Notify>,
    }

    #[async_trait]
    impl LlmProvider for ParkedProvider {
        async fn complete(&self, _req: CompleteRequest) -> Result<CompleteResponse, Error> {
            self.entered.notify_one();
            self.release.notified().await;
            Ok(CompleteResponse {
                content: "parked".into(),
                input_tokens: 0,
                output_tokens: 0,
            })
        }
    }

    #[tokio::test]
    async fn no_provider_error_does_not_wait_for_a_permit() {
        let entered = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());
        let router = Router::new()
            .with(
                ModelTier::Reasoning,
                Arc::new(ParkedProvider {
                    entered: entered.clone(),
                    release: release.clone(),
                }),
            )
            .with_inflight_limit(1);

        let holder = {
            let router = router.clone();
            tokio::spawn(async move { router.complete(ModelTier::Reasoning, req()).await })
        };
        entered.notified().await;

        let result = tokio::time::timeout(
            Duration::from_millis(100),
            router.complete(ModelTier::Cheap, req()),
        )
        .await;
        assert!(matches!(
            result,
            Ok(Err(Error::NoProvider(ModelTier::Cheap)))
        ));

        release.notify_one();
        holder.await.unwrap().unwrap();
    }
}

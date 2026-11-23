//! HTTP webhook intake.
//!
//! Validates Forgejo's HMAC-SHA256 signature and accepts low-cost
//! `pull_request` intake without dispatching semantic review work by default.
//! CI-triggered `/reviews/ci` requests and explicit chat commands use the
//! dispatcher abstraction so the gateway can return quickly while reviews run in
//! the background.

#![expect(clippy::pub_use, reason = "Preserve the crate-root startup re-exports")]

extern crate alloc;

pub mod config;
pub mod dedup;
pub mod github_webhook;
pub mod hmac;
pub mod metrics;
pub mod poller;
pub mod ratelimit;
/// Configuration and wiring for gateway startup.
mod startup;
pub mod webhook;

pub use startup::{run_from_env, StartupOptions};

use alloc::sync::Arc;
use ar_forgejo::Client as ForgejoClient;
use ar_github::AppReviewHost;
use ar_index::LearningsStore;
use ar_llm::Router as LlmRouter;
use ar_orchestrator::JobDispatcher;
use axum::extract::State;
use axum::http::{header, StatusCode};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use core::time::Duration;
use dedup::DeliveryDedup;
use metrics::Metrics;
use poller::SharedCommentCursors;
use ratelimit::TokenBucket;
use serde::Serialize;
use serde_json::Value;
use std::time::Instant;
use tokio::sync::Mutex;

#[expect(
    clippy::arbitrary_source_item_ordering,
    reason = "Preserve field declaration and drop order for the existing state"
)]
#[expect(
    clippy::exhaustive_structs,
    reason = "External callers construct state literals"
)]
#[derive(Clone)]
pub struct AppState {
    pub webhook_secret: Arc<String>,
    pub dispatcher: Arc<dyn JobDispatcher>,
    /// Optional chat-handler dependencies. Populated when running the
    /// full gateway; tests that only exercise the dispatch surface can
    /// leave [`Self::chat`] as `None` and `issue_comment` events will
    /// be parsed-but-not-handled.
    pub chat: Option<ChatDeps>,
    pub metrics: Arc<Metrics>,
    /// Forgejo username the bot authenticates as. Used for self-loop
    /// detection (don't act on the bot's own comments). Defaults to
    /// `auto-review`.
    pub bot_login: Arc<String>,
    /// Mention-handle the bot listens for (`@<bot_name>`). Often the
    /// same as `bot_login`. Defaults to `auto-review`.
    pub bot_name: Arc<String>,
    /// Optional probe target for the `/readyz` readiness endpoint.
    /// Holds the Forgejo client (already wired for chat/dispatch) and
    /// a small TTL cache so we don't hammer Forgejo on every k8s
    /// readiness probe.
    pub readiness: Option<Arc<ReadinessProbe>>,
    /// Snapshot of runtime configuration surfaced at `/info`. None
    /// for tests that don't bother populating it; production
    /// `main.rs` always sets this.
    pub info: Option<Arc<GatewayInfo>>,
    /// Optional global token-bucket throttle on the
    /// `/webhooks/forgejo` route. T7 mitigation per the threat
    /// model. None = no throttling (current default for tests and
    /// trust-the-environment deployments).
    pub webhook_rate_limit: Option<Arc<TokenBucket>>,
    /// Optional dedup of recently-seen `X-Forgejo-Delivery` IDs.
    /// `None` = no dedup (caller dispatches every well-signed
    /// webhook even on Forgejo retry). Backed by either the in-memory
    /// LRU or the SQLite table — `main.rs` picks based on env.
    pub webhook_dedup: Option<Arc<dyn DeliveryDedup>>,
    pub chat_comment_cursors: Option<SharedCommentCursors>,
    pub ci_review_endpoint: Option<CiReviewEndpointDeps>,
    /// GitHub App support. When set, `/webhooks/github` accepts
    /// `X-Hub-Signature-256`-signed deliveries and `/reviews/ci` accepts
    /// `"provider": "github"` requests.
    pub github: Option<GitHubDeps>,
}

#[expect(
    clippy::exhaustive_structs,
    reason = "Preserve public dependency literals"
)]
#[derive(Clone)]
pub struct CiReviewEndpointDeps {
    pub action_token: Arc<String>,
    /// Forgejo client for `provider: "forgejo"` CI requests. `None` in
    /// GitHub-only deployments.
    pub forgejo: Option<Arc<ForgejoClient>>,
}

/// Everything the gateway needs to serve GitHub App repositories.
#[expect(
    clippy::exhaustive_structs,
    reason = "Preserve public dependency literals"
)]
#[expect(
    clippy::arbitrary_source_item_ordering,
    reason = "Preserve field drop order and keep review wiring before optional chat settings"
)]
#[derive(Clone)]
pub struct GitHubDeps {
    /// Secret configured on the GitHub App webhook; verifies
    /// `X-Hub-Signature-256`.
    pub webhook_secret: Arc<String>,
    pub host: Arc<AppReviewHost>,
    /// Dispatcher whose reviews run against [`Self::host`].
    pub dispatcher: Arc<dyn JobDispatcher>,
    /// Login GitHub shows for the app's own comments (`<slug>[bot]`).
    pub bot_login: Arc<String>,
    /// Mention handle users type (`@<bot_name>`).
    pub bot_name: Arc<String>,
    /// Chat dependencies; `None` parses commands without handling them.
    pub chat: Option<GitHubChatDeps>,
}

#[expect(
    clippy::exhaustive_structs,
    reason = "Preserve public dependency literals"
)]
#[expect(
    clippy::arbitrary_source_item_ordering,
    reason = "Preserve field drop order"
)]
#[derive(Clone)]
pub struct GitHubChatDeps {
    pub llm: Arc<LlmRouter>,
    pub learnings: Arc<dyn LearningsStore>,
}

impl GitHubDeps {
    #[inline]
    pub fn new(
        webhook_secret: impl Into<String>,
        host: Arc<AppReviewHost>,
        dispatcher: Arc<dyn JobDispatcher>,
    ) -> Self {
        Self {
            webhook_secret: Arc::new(webhook_secret.into()),
            host,
            dispatcher,
            bot_login: Arc::new("auto-review[bot]".into()),
            bot_name: Arc::new("auto-review".into()),
            chat: None,
        }
    }

    #[inline]
    #[must_use = "Use the returned dependencies to retain the new bot identity"]
    pub fn with_bot_identity(
        mut self,
        bot_login: impl Into<String>,
        bot_name: impl Into<String>,
    ) -> Self {
        self.bot_login = Arc::new(bot_login.into());
        self.bot_name = Arc::new(bot_name.into());
        self
    }

    #[inline]
    #[must_use = "Use the returned dependencies to enable chat"]
    pub fn with_chat(mut self, chat: GitHubChatDeps) -> Self {
        self.chat = Some(chat);
        self
    }
}

#[expect(
    clippy::exhaustive_structs,
    reason = "Preserve public snapshot literals"
)]
#[expect(
    clippy::arbitrary_source_item_ordering,
    reason = "Preserve serialized field order"
)]
#[derive(Debug, Clone, Serialize)]
pub struct RuntimeIsolationPostureInfo {
    pub kind: &'static str,
    pub label: String,
    pub detail: String,
}

impl RuntimeIsolationPostureInfo {
    #[inline]
    #[must_use = "The default posture snapshot must be retained to report it"]
    pub fn oci_default() -> Self {
        Self {
            kind: "oci_default",
            label: "packaged OCI container isolation".to_owned(),
            detail: "Gateway uses embedded OCI container-equivalent isolation by default."
                .to_owned(),
        }
    }

    #[inline]
    #[must_use = "The failure posture snapshot must be retained to report it"]
    pub fn oci_setup_failed(detail: &str) -> Self {
        let public_detail = if detail.contains("/run/secrets")
            || detail.contains("secret-bearing")
            || detail.contains("ar-token")
        {
            "embedded OCI setup failed before inner gateway startup"
        } else {
            detail
        };

        Self {
            kind: "oci_setup_failed",
            label: "OCI setup failed".to_owned(),
            detail: format!("OCI setup failed: {public_detail}; set AR_GATEWAY_BARE to opt out"),
        }
    }
}

/// Runtime-config snapshot returned from `GET /info`. Captured once
/// at startup; nothing here changes during the gateway's lifetime
/// (readiness state lives at `/readyz`, counters at `/metrics`).
///
/// JSON shape is stable — operators script against this. Adding a
/// field is fine; renaming or removing one is a breaking change.
#[expect(
    clippy::exhaustive_structs,
    reason = "External callers construct snapshot literals"
)]
#[expect(
    clippy::arbitrary_source_item_ordering,
    reason = "Preserve serialized field order"
)]
#[derive(Debug, Clone, Serialize)]
pub struct GatewayInfo {
    pub name: &'static str,
    pub version: &'static str,
    pub bot_login: String,
    pub bot_name: String,
    /// Concrete `LearningsStore` backing — either `"in-memory"` or
    /// `"sqlite:<path>"`. The path lets operators verify the bot
    /// opened the file they intended.
    pub learnings: String,
    /// Concrete `ReviewHistory` backing — either `"in-memory"` or
    /// `"sqlite:<path>"`.
    pub history: String,
    /// Concrete `VectorStore` backing — either `"in-memory"` or
    /// `"sqlite:<path>"`. Symbol embeddings persist when SQLite is
    /// chosen; in-memory means every review re-embeds the workspace.
    pub vector: String,
    /// Concrete webhook delivery dedup backing — `"disabled"`,
    /// `"in-memory(capacity=N)"`, or `"sqlite:<path>"`.
    pub dedup: String,
    /// Which LLM tiers have a provider configured. Order is
    /// stable: `["reasoning", "cheap", "embedding"]`.
    pub llm_tiers: Vec<&'static str>,
    /// Reasoning model name from env at startup. Empty when not
    /// set (which would block reviews entirely; useful debug
    /// signal).
    pub reasoning_model: String,
    /// Whether the background `ChatPoller` is running. Disabled
    /// when `AR_POLL_INTERVAL_SECS=0`.
    pub poller_enabled: bool,
    /// Repository providers this gateway serves: `"forgejo"`, `"github"`.
    pub providers: Vec<&'static str>,
    /// Whether `/readyz` does an actual probe vs degrading to
    /// `/healthz` semantics.
    pub readiness_enabled: bool,
    pub runtime_isolation: RuntimeIsolationPostureInfo,
}

/// Async-Mutex-guarded TTL cache wrapping repository-host reachability
/// checks (Forgejo server version, GitHub App authentication). The cache
/// stays small intentionally — readiness should be independent of fancy
/// metrics infrastructure.
#[expect(
    clippy::arbitrary_source_item_ordering,
    reason = "Keep probe inputs before cached state"
)]
pub struct ReadinessProbe {
    /// Hosts to probe, in the order their details appear in the response.
    targets: Vec<ReadinessTarget>,
    /// How long a probe result can be reused.
    ttl: Duration,
    /// Most recent result, shared by all readiness requests.
    cache: Mutex<Option<CachedReadiness>>,
}

/// A host whose reachability or authentication is required for readiness.
enum ReadinessTarget {
    /// Probe the Forgejo server version.
    Forgejo(Arc<ForgejoClient>),
    /// Probe GitHub App authentication.
    GitHub(Arc<AppReviewHost>),
}

impl ReadinessTarget {
    /// Return this host's readiness status and a human-readable explanation.
    #[expect(
        clippy::pattern_type_mismatch,
        reason = "Match ergonomics borrow the host client"
    )]
    async fn check(&self) -> (bool, String) {
        match self {
            Self::Forgejo(forgejo) => match forgejo.get_server_version().await {
                Ok(version) => (true, format!("forgejo reachable ({version})")),
                Err(error) => (false, format!("forgejo unreachable: {error}")),
            },
            Self::GitHub(host) => match host.client().get_authenticated_app().await {
                Ok(app) => (true, format!("github reachable (app {})", app.slug)),
                Err(error) => (false, format!("github unreachable: {error}")),
            },
        }
    }
}

/// Cached result of probing all configured hosts.
#[derive(Clone)]
struct CachedReadiness {
    /// Timestamp used to check the cache TTL.
    checked_at: Instant,
    /// Combined human-readable result for all configured hosts.
    detail: String,
    /// Whether every host passed its probe.
    healthy: bool,
}

#[expect(
    clippy::arbitrary_source_item_ordering,
    reason = "Keep constructors and probe configuration together before the probe operation"
)]
impl ReadinessProbe {
    #[inline]
    #[must_use = "The readiness probe must be installed to run checks"]
    pub fn new(forgejo: Arc<ForgejoClient>) -> Self {
        Self::with_ttl(forgejo, Duration::from_secs(10))
    }

    #[inline]
    #[must_use = "The readiness probe must be installed to run checks"]
    pub fn with_ttl(forgejo: Arc<ForgejoClient>, ttl: Duration) -> Self {
        Self::from_targets(vec![ReadinessTarget::Forgejo(forgejo)], ttl)
    }

    /// Probe only GitHub App authentication (GitHub-only gateways).
    #[inline]
    pub fn github_with_ttl(host: Arc<AppReviewHost>, ttl: Duration) -> Self {
        Self::from_targets(vec![ReadinessTarget::GitHub(host)], ttl)
    }

    /// Also require GitHub App authentication to succeed.
    #[inline]
    #[must_use = "Use the returned probe to include the GitHub check"]
    pub fn and_github(mut self, host: Arc<AppReviewHost>) -> Self {
        self.targets.push(ReadinessTarget::GitHub(host));
        self
    }

    /// Create an empty cache for the configured targets and TTL.
    fn from_targets(targets: Vec<ReadinessTarget>, ttl: Duration) -> Self {
        Self {
            targets,
            ttl,
            cache: Mutex::new(None),
        }
    }

    /// Returns `(healthy, detail)`. Probes every configured host when the
    /// cache is empty or stale; serves from cache otherwise. Healthy only
    /// when every host is reachable.
    #[inline]
    pub async fn check(&self) -> (bool, String) {
        let mut guard = self.cache.lock().await;
        let now = Instant::now();
        if let Some(cached) = guard.as_ref() {
            if now.duration_since(cached.checked_at) < self.ttl {
                return (cached.healthy, cached.detail.clone());
            }
        }
        let mut healthy = true;
        let mut details = Vec::with_capacity(self.targets.len());
        for target in &self.targets {
            let (target_healthy, detail) = target.check().await;
            healthy &= target_healthy;
            details.push(detail);
        }
        let detail = details.join("; ");
        *guard = Some(CachedReadiness {
            checked_at: now,
            healthy,
            detail: detail.clone(),
        });
        (healthy, detail)
    }
}

/// Dependencies the chat handler needs. Bundled so the optional-ness
/// is one Option, not three.
#[expect(
    clippy::exhaustive_structs,
    reason = "Preserve public dependency literals"
)]
#[expect(
    clippy::arbitrary_source_item_ordering,
    reason = "Preserve field drop order"
)]
#[derive(Clone)]
pub struct ChatDeps {
    pub forgejo: Arc<ForgejoClient>,
    pub llm: Arc<LlmRouter>,
    pub learnings: Arc<dyn LearningsStore>,
}

#[expect(
    clippy::arbitrary_source_item_ordering,
    reason = "Group handler wiring and runtime overrides by purpose rather than name"
)]
impl AppState {
    #[inline]
    pub fn new(webhook_secret: impl Into<String>, dispatcher: Arc<dyn JobDispatcher>) -> Self {
        Self {
            webhook_secret: Arc::new(webhook_secret.into()),
            dispatcher,
            chat: None,
            metrics: Arc::new(Metrics::new()),
            bot_login: Arc::new("auto-review".into()),
            bot_name: Arc::new("auto-review".into()),
            readiness: None,
            info: None,
            webhook_rate_limit: None,
            webhook_dedup: None,
            chat_comment_cursors: None,
            ci_review_endpoint: None,
            github: None,
        }
    }

    /// Wire in a global token-bucket rate limiter for the
    /// `/webhooks/forgejo` route. Without this, the route accepts
    /// every well-signed request.
    #[inline]
    #[must_use = "Use the returned state to enable webhook throttling"]
    pub fn with_webhook_rate_limit(mut self, bucket: Arc<TokenBucket>) -> Self {
        self.webhook_rate_limit = Some(bucket);
        self
    }

    /// Wire in a recently-seen-delivery dedup so retried webhooks
    /// (same `X-Forgejo-Delivery` UUID) are answered 200 OK
    /// without re-dispatching to the orchestrator.
    #[inline]
    #[must_use = "Use the returned state to enable webhook deduplication"]
    pub fn with_webhook_dedup(mut self, dedup: Arc<dyn DeliveryDedup>) -> Self {
        self.webhook_dedup = Some(dedup);
        self
    }

    #[inline]
    #[must_use = "Use the returned state to install the comment cursors"]
    pub fn with_chat_comment_cursors(mut self, cursors: SharedCommentCursors) -> Self {
        self.chat_comment_cursors = Some(cursors);
        self
    }

    #[inline]
    #[must_use = "Use the returned state to enable the Forgejo CI review endpoint"]
    pub fn with_ci_review_endpoint(
        mut self,
        action_token: impl Into<String>,
        forgejo: Arc<ForgejoClient>,
    ) -> Self {
        self.ci_review_endpoint = Some(CiReviewEndpointDeps {
            action_token: Arc::new(action_token.into()),
            forgejo: Some(forgejo),
        });
        self
    }

    /// Enable `/reviews/ci` without a Forgejo client, for GitHub-only
    /// deployments. Forgejo-provider requests are then rejected.
    #[inline]
    #[must_use = "Use the returned state to enable GitHub-only CI reviews"]
    pub fn with_ci_review_token(mut self, action_token: impl Into<String>) -> Self {
        self.ci_review_endpoint = Some(CiReviewEndpointDeps {
            action_token: Arc::new(action_token.into()),
            forgejo: None,
        });
        self
    }

    #[inline]
    #[must_use = "Use the returned state to install the GitHub dependencies"]
    pub fn with_github(mut self, github: GitHubDeps) -> Self {
        self.github = Some(github);
        self
    }

    /// Wire in a `/readyz` probe. When unset, `/readyz` returns the
    /// same response as `/healthz` (200 OK) — readiness checks
    /// degrade to liveness, which is safe for single-pod deploys.
    #[inline]
    #[must_use = "Use the returned state to install the readiness probe"]
    pub fn with_readiness(mut self, probe: Arc<ReadinessProbe>) -> Self {
        self.readiness = Some(probe);
        self
    }

    /// Inject the runtime-config snapshot surfaced at `/info`.
    #[inline]
    #[must_use = "Use the returned state to expose the runtime snapshot"]
    pub fn with_info(mut self, info: Arc<GatewayInfo>) -> Self {
        self.info = Some(info);
        self
    }

    #[inline]
    #[must_use = "Use the returned state to enable chat"]
    pub fn with_chat(mut self, chat: ChatDeps) -> Self {
        self.chat = Some(chat);
        self
    }

    /// Override the bot identity used for self-loop detection and
    /// `@<bot_name>` mention parsing.
    #[inline]
    #[must_use = "Use the returned state to retain the new bot identity"]
    pub fn with_bot_identity(
        mut self,
        bot_login: impl Into<String>,
        bot_name: impl Into<String>,
    ) -> Self {
        self.bot_login = Arc::new(bot_login.into());
        self.bot_name = Arc::new(bot_name.into());
        self
    }

    /// Inject a shared metrics handle so the orchestrator's
    /// `MetricsObserver` and the `/metrics` endpoint read/write the
    /// same counters. Without this, `/metrics` returns the gateway's
    /// own (empty) instance.
    #[inline]
    #[must_use = "Use the returned state to install the shared counters"]
    pub fn with_metrics(mut self, metrics: Arc<Metrics>) -> Self {
        self.metrics = metrics;
        self
    }
}

#[inline]
pub fn build_router(state: AppState) -> Router {
    Router::new()
        .route("/healthz", get(|| async { "ok" }))
        .route("/readyz", get(readyz_handler))
        .route("/version", get(version_handler))
        .route("/info", get(info_handler))
        .route("/metrics", get(metrics_handler))
        .route("/reviews/ci", post(webhook::handle_ci_review))
        .route("/webhooks/forgejo", post(webhook::handle))
        .route("/webhooks/github", post(github_webhook::handle))
        .with_state(state)
}

/// Serve the startup configuration, or version metadata when it is not wired.
#[expect(
    clippy::unreachable,
    reason = "GatewayInfo contains only JSON-compatible fields"
)]
async fn info_handler(State(state): State<AppState>) -> impl IntoResponse {
    match state.info {
        Some(info) => {
            // Keep the Value conversion so /info retains its existing key order.
            let Ok(value) = serde_json::to_value(&*info) else {
                unreachable!("GatewayInfo contains only JSON-compatible fields");
            };
            (StatusCode::OK, Json(value)).into_response()
        }
        // No info wired (tests, partial setups). Fall back to the
        // same {name, version} the /version endpoint emits so the
        // route still answers.
        None => (
            StatusCode::OK,
            Json(serde_json::json!({
                "name": "auto_review",
                "version": env!("CARGO_PKG_VERSION"),
                "info": "not wired",
            })),
        )
            .into_response(),
    }
}

/// Serve readiness using the optional host probe, falling back to liveness.
async fn readyz_handler(State(state): State<AppState>) -> impl IntoResponse {
    match state.readiness {
        Some(probe) => {
            let (healthy, detail) = probe.check().await;
            let status = if healthy {
                StatusCode::OK
            } else {
                StatusCode::SERVICE_UNAVAILABLE
            };
            (
                status,
                [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
                detail,
            )
        }
        None => (
            StatusCode::OK,
            [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
            "ready (no probe configured; same as /healthz)".to_owned(),
        ),
    }
}

/// Serve the gateway name and build version.
async fn version_handler() -> Json<Value> {
    Json(serde_json::json!({
        "name": "auto_review",
        "version": env!("CARGO_PKG_VERSION"),
    }))
}

/// Serve the current metrics in Prometheus text format.
async fn metrics_handler(State(state): State<AppState>) -> impl IntoResponse {
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "text/plain; version=0.0.4")],
        state.metrics.render(),
    )
}

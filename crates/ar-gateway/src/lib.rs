//! HTTP webhook intake.
//!
//! Validates Forgejo's HMAC-SHA256 signature and accepts low-cost
//! `pull_request` intake without dispatching semantic review work by default.
//! CI-triggered `/reviews/ci` requests and explicit chat commands use the
//! dispatcher abstraction so the gateway can return quickly while reviews run in
//! the background.

pub mod config;
pub mod dedup;
pub mod hmac;
pub mod metrics;
pub mod poller;
pub mod ratelimit;
mod startup;
pub mod webhook;

pub use startup::{run_from_env, StartupOptions};

use ar_forgejo::Client as ForgejoClient;
use ar_index::LearningsStore;
use ar_llm::Router as LlmRouter;
use ar_orchestrator::JobDispatcher;
use axum::extract::State;
use axum::http::{header, StatusCode};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::Router;
use metrics::Metrics;
use poller::SharedCommentCursors;
use serde::Serialize;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;

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
    /// `auto_review`.
    pub bot_login: Arc<String>,
    /// Mention-handle the bot listens for (`@<bot_name>`). Often the
    /// same as `bot_login`. Defaults to `auto_review`.
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
    pub webhook_rate_limit: Option<Arc<crate::ratelimit::TokenBucket>>,
    /// Optional dedup of recently-seen `X-Forgejo-Delivery` IDs.
    /// `None` = no dedup (caller dispatches every well-signed
    /// webhook even on Forgejo retry). Backed by either the in-memory
    /// LRU or the SQLite table — `main.rs` picks based on env.
    pub webhook_dedup: Option<Arc<dyn crate::dedup::DeliveryDedup>>,
    pub chat_comment_cursors: Option<SharedCommentCursors>,
    pub ci_review_endpoint: Option<CiReviewEndpointDeps>,
}

#[derive(Clone)]
pub struct CiReviewEndpointDeps {
    pub action_token: Arc<String>,
    pub forgejo: Arc<ForgejoClient>,
}

/// Runtime-config snapshot returned from `GET /info`. Captured once
/// at startup; nothing here changes during the gateway's lifetime
/// (readiness state lives at `/readyz`, counters at `/metrics`).
///
/// JSON shape is stable — operators script against this. Adding a
/// field is fine; renaming or removing one is a breaking change.
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
    /// Whether `/readyz` does an actual probe vs degrading to
    /// `/healthz` semantics.
    pub readiness_enabled: bool,
}

/// Async-Mutex-guarded TTL cache wrapping a Forgejo reachability
/// check. The cache stays small intentionally — readiness should be
/// independent of fancy metrics infrastructure.
pub struct ReadinessProbe {
    forgejo: Arc<ForgejoClient>,
    ttl: Duration,
    cache: Mutex<Option<CachedReadiness>>,
}

#[derive(Clone)]
struct CachedReadiness {
    checked_at: Instant,
    healthy: bool,
    detail: String,
}

impl ReadinessProbe {
    pub fn new(forgejo: Arc<ForgejoClient>) -> Self {
        Self::with_ttl(forgejo, Duration::from_secs(10))
    }

    pub fn with_ttl(forgejo: Arc<ForgejoClient>, ttl: Duration) -> Self {
        Self {
            forgejo,
            ttl,
            cache: Mutex::new(None),
        }
    }

    /// Returns `(healthy, detail)`. Probes Forgejo when the cache is
    /// empty or stale; serves from cache otherwise.
    pub async fn check(&self) -> (bool, String) {
        let mut guard = self.cache.lock().await;
        let now = Instant::now();
        if let Some(c) = guard.as_ref() {
            if now.duration_since(c.checked_at) < self.ttl {
                return (c.healthy, c.detail.clone());
            }
        }
        let (healthy, detail) = match self.forgejo.get_server_version().await {
            Ok(v) => (true, format!("forgejo reachable ({v})")),
            Err(e) => (false, format!("forgejo unreachable: {e}")),
        };
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
#[derive(Clone)]
pub struct ChatDeps {
    pub forgejo: Arc<ForgejoClient>,
    pub llm: Arc<LlmRouter>,
    pub learnings: Arc<dyn LearningsStore>,
}

impl AppState {
    pub fn new(webhook_secret: impl Into<String>, dispatcher: Arc<dyn JobDispatcher>) -> Self {
        Self {
            webhook_secret: Arc::new(webhook_secret.into()),
            dispatcher,
            chat: None,
            metrics: Arc::new(Metrics::new()),
            bot_login: Arc::new("auto_review".into()),
            bot_name: Arc::new("auto_review".into()),
            readiness: None,
            info: None,
            webhook_rate_limit: None,
            webhook_dedup: None,
            chat_comment_cursors: None,
            ci_review_endpoint: None,
        }
    }

    /// Wire in a global token-bucket rate limiter for the
    /// `/webhooks/forgejo` route. Without this, the route accepts
    /// every well-signed request.
    pub fn with_webhook_rate_limit(mut self, bucket: Arc<crate::ratelimit::TokenBucket>) -> Self {
        self.webhook_rate_limit = Some(bucket);
        self
    }

    /// Wire in a recently-seen-delivery dedup so retried webhooks
    /// (same `X-Forgejo-Delivery` UUID) are answered 200 OK
    /// without re-dispatching to the orchestrator.
    pub fn with_webhook_dedup(mut self, dedup: Arc<dyn crate::dedup::DeliveryDedup>) -> Self {
        self.webhook_dedup = Some(dedup);
        self
    }

    pub fn with_chat_comment_cursors(mut self, cursors: SharedCommentCursors) -> Self {
        self.chat_comment_cursors = Some(cursors);
        self
    }

    pub fn with_ci_review_endpoint(
        mut self,
        action_token: impl Into<String>,
        forgejo: Arc<ForgejoClient>,
    ) -> Self {
        self.ci_review_endpoint = Some(CiReviewEndpointDeps {
            action_token: Arc::new(action_token.into()),
            forgejo,
        });
        self
    }

    /// Wire in a `/readyz` probe. When unset, `/readyz` returns the
    /// same response as `/healthz` (200 OK) — readiness checks
    /// degrade to liveness, which is safe for single-pod deploys.
    pub fn with_readiness(mut self, probe: Arc<ReadinessProbe>) -> Self {
        self.readiness = Some(probe);
        self
    }

    /// Inject the runtime-config snapshot surfaced at `/info`.
    pub fn with_info(mut self, info: Arc<GatewayInfo>) -> Self {
        self.info = Some(info);
        self
    }

    pub fn with_chat(mut self, chat: ChatDeps) -> Self {
        self.chat = Some(chat);
        self
    }

    /// Override the bot identity used for self-loop detection and
    /// `@<bot_name>` mention parsing.
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
    pub fn with_metrics(mut self, metrics: Arc<Metrics>) -> Self {
        self.metrics = metrics;
        self
    }
}

pub fn build_router(state: AppState) -> Router {
    Router::new()
        .route("/healthz", get(|| async { "ok" }))
        .route("/readyz", get(readyz_handler))
        .route("/version", get(version_handler))
        .route("/info", get(info_handler))
        .route("/metrics", get(metrics_handler))
        .route("/reviews/ci", post(webhook::handle_ci_review))
        .route("/webhooks/forgejo", post(webhook::handle))
        .with_state(state)
}

async fn info_handler(State(state): State<AppState>) -> impl IntoResponse {
    match state.info {
        Some(info) => (
            StatusCode::OK,
            axum::Json(serde_json::to_value(&*info).unwrap()),
        )
            .into_response(),
        // No info wired (tests, partial setups). Fall back to the
        // same {name, version} the /version endpoint emits so the
        // route still answers.
        None => (
            StatusCode::OK,
            axum::Json(serde_json::json!({
                "name": "auto_review",
                "version": env!("CARGO_PKG_VERSION"),
                "info": "not wired",
            })),
        )
            .into_response(),
    }
}

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
            "ready (no probe configured; same as /healthz)".to_string(),
        ),
    }
}

async fn version_handler() -> axum::response::Json<serde_json::Value> {
    axum::response::Json(serde_json::json!({
        "name": "auto_review",
        "version": env!("CARGO_PKG_VERSION"),
    }))
}

async fn metrics_handler(State(state): State<AppState>) -> impl IntoResponse {
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "text/plain; version=0.0.4")],
        state.metrics.render(),
    )
}

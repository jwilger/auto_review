//! HTTP webhook intake.
//!
//! Validates Forgejo's HMAC-SHA256 signature, decodes the `pull_request`
//! event, and dispatches a review job. The dispatcher abstraction lets the
//! gateway return 202 immediately while the actual review runs in the
//! background.

pub mod hmac;
pub mod metrics;
pub mod poller;
pub mod webhook;

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
    /// Which `Sandbox` impl is active. `"direct"` = no isolation
    /// (Kudelski-class RCE risk; only safe for trusted-PR sources);
    /// `"podman"` = the hardened production path.
    pub sandbox: &'static str,
    /// Which `LearningsStore` impl is wired up. `"sqlite"` =
    /// persistent across restart; `"in-memory"` = volatile.
    pub learnings: &'static str,
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
        }
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
        .route("/webhooks/forgejo", post(webhook::handle))
        .with_state(state)
}

async fn info_handler(State(state): State<AppState>) -> impl IntoResponse {
    match state.info {
        Some(info) => (StatusCode::OK, axum::Json(serde_json::to_value(&*info).unwrap()))
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

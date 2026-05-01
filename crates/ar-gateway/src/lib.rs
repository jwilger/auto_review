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
use std::sync::Arc;

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
        }
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
        .route("/version", get(version_handler))
        .route("/metrics", get(metrics_handler))
        .route("/webhooks/forgejo", post(webhook::handle))
        .with_state(state)
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

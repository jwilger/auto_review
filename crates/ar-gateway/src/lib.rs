//! HTTP webhook intake.
//!
//! Validates Forgejo's HMAC-SHA256 signature, decodes the `pull_request`
//! event, and dispatches a review job. The dispatcher abstraction lets the
//! gateway return 202 immediately while the actual review runs in the
//! background.

pub mod hmac;
pub mod webhook;

use ar_orchestrator::JobDispatcher;
use axum::routing::{get, post};
use axum::Router;
use std::sync::Arc;

#[derive(Clone)]
pub struct AppState {
    pub webhook_secret: Arc<String>,
    pub dispatcher: Arc<dyn JobDispatcher>,
}

impl AppState {
    pub fn new(webhook_secret: impl Into<String>, dispatcher: Arc<dyn JobDispatcher>) -> Self {
        Self {
            webhook_secret: Arc::new(webhook_secret.into()),
            dispatcher,
        }
    }
}

pub fn build_router(state: AppState) -> Router {
    Router::new()
        .route("/healthz", get(|| async { "ok" }))
        .route("/webhooks/forgejo", post(webhook::handle))
        .with_state(state)
}

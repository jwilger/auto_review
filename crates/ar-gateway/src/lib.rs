//! HTTP webhook intake.
//!
//! Validates Forgejo's HMAC-SHA256 signature, decodes the `pull_request`
//! event, and (in v1) hands off to the orchestrator. For now the handoff is
//! stubbed: the gateway just acks 202 once the payload is validated.

pub mod hmac;
pub mod webhook;

use axum::routing::{get, post};
use axum::Router;
use std::sync::Arc;

#[derive(Clone)]
pub struct AppState {
    pub webhook_secret: Arc<String>,
}

impl AppState {
    pub fn new(webhook_secret: impl Into<String>) -> Self {
        Self {
            webhook_secret: Arc::new(webhook_secret.into()),
        }
    }
}

pub fn build_router(state: AppState) -> Router {
    Router::new()
        .route("/healthz", get(|| async { "ok" }))
        .route("/webhooks/forgejo", post(webhook::handle))
        .with_state(state)
}

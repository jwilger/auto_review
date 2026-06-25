//! AWS Bedrock AgentCore-compatible runtime HTTP surface.

use async_trait::async_trait;
use aws_sdk_dynamodb::types::AttributeValue;
use axum::extract::rejection::JsonRejection;
use axum::extract::State;
use axum::routing::get;
use axum::{http::StatusCode, response::IntoResponse, routing::post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::net::TcpListener;
use tokio::sync::Mutex;

type SharedInvocationHandler = Arc<dyn InvocationHandler>;
type SharedInvocationIdempotency = Arc<dyn InvocationIdempotency>;

pub fn build_router() -> Router {
    Router::new()
        .route("/ping", get(ping))
        .route("/invocations", post(invocations))
}

pub fn build_router_with_handler(handler: SharedInvocationHandler) -> Router {
    Router::new()
        .route("/ping", get(ping))
        .route("/invocations", post(invocations_with_handler))
        .with_state(handler)
}

pub fn build_router_with_handler_and_idempotency(
    handler: SharedInvocationHandler,
    idempotency: SharedInvocationIdempotency,
) -> Router {
    Router::new()
        .route("/ping", get(ping))
        .route("/invocations", post(invocations_with_idempotency))
        .with_state(InvocationState {
            handler,
            idempotency,
        })
}

#[derive(Clone)]
pub struct ServeConfig {
    pub bind: String,
    pub handler: Option<SharedInvocationHandler>,
    pub idempotency: Option<SharedInvocationIdempotency>,
}

pub async fn serve(config: ServeConfig) -> anyhow::Result<()> {
    let addr: SocketAddr = config
        .bind
        .parse()
        .map_err(|e| anyhow::anyhow!("invalid AgentCore bind address `{}`: {e}", config.bind))?;
    let listener = TcpListener::bind(addr).await?;
    tracing::info!(%addr, "AgentCore runtime listening");
    axum::serve(listener, build_router_from_config(&config)).await?;
    Ok(())
}

pub fn build_router_from_config(config: &ServeConfig) -> Router {
    match &config.handler {
        Some(handler) => build_router_with_handler_and_idempotency(
            handler.clone(),
            config
                .idempotency
                .clone()
                .unwrap_or_else(|| Arc::new(InMemoryInvocationIdempotency::new())),
        ),
        None => build_router(),
    }
}

async fn ping() -> Json<PingResponse> {
    Json(PingResponse { status: "healthy" })
}

async fn invocations(
    payload: Result<Json<InvocationPayload>, JsonRejection>,
) -> Result<impl IntoResponse, InvocationErrorResponse> {
    let Json(payload) = payload.map_err(|rejection| InvocationErrorResponse {
        status: StatusCode::BAD_REQUEST,
        error: "invalid_payload",
        message: rejection.body_text(),
    })?;

    Ok((
        StatusCode::ACCEPTED,
        Json(InvocationAccepted {
            status: "accepted",
            provider: payload.provider,
            kind: payload.kind,
            owner: payload.owner,
            repo: payload.repo,
            pr_number: payload.pr_number,
            head_sha: payload.head_sha,
            force: payload.force.unwrap_or(false),
        }),
    ))
}

async fn invocations_with_handler(
    State(handler): State<SharedInvocationHandler>,
    payload: Result<Json<InvocationPayload>, JsonRejection>,
) -> Result<impl IntoResponse, InvocationErrorResponse> {
    let Json(payload) = payload.map_err(|rejection| InvocationErrorResponse {
        status: StatusCode::BAD_REQUEST,
        error: "invalid_payload",
        message: rejection.body_text(),
    })?;
    let outcome = handler.handle(payload).await.map_err(|error| {
        let status = match error.kind {
            InvocationErrorKind::InvalidPayload => StatusCode::BAD_REQUEST,
            InvocationErrorKind::StaleHead => StatusCode::CONFLICT,
            InvocationErrorKind::ExecutionFailed => StatusCode::INTERNAL_SERVER_ERROR,
        };
        InvocationErrorResponse {
            status,
            error: error.kind.as_str(),
            message: error.message,
        }
    })?;
    Ok((StatusCode::OK, Json(outcome)))
}

async fn invocations_with_idempotency(
    State(state): State<InvocationState>,
    payload: Result<Json<InvocationPayload>, JsonRejection>,
) -> Result<impl IntoResponse, InvocationErrorResponse> {
    let Json(payload) = payload.map_err(|rejection| InvocationErrorResponse {
        status: StatusCode::BAD_REQUEST,
        error: "invalid_payload",
        message: rejection.body_text(),
    })?;
    let key = payload.idempotency_key();
    if !state
        .idempotency
        .claim(&key)
        .await
        .map_err(|error| InvocationErrorResponse {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            error: "idempotency_failed",
            message: error.message,
        })?
    {
        return Ok((
            StatusCode::OK,
            Json(InvocationOutcome {
                status: "duplicate".to_string(),
                message: "invocation already handled".to_string(),
            }),
        ));
    }

    let outcome = state.handler.handle(payload).await.map_err(|error| {
        let status = match error.kind {
            InvocationErrorKind::InvalidPayload => StatusCode::BAD_REQUEST,
            InvocationErrorKind::StaleHead => StatusCode::CONFLICT,
            InvocationErrorKind::ExecutionFailed => StatusCode::INTERNAL_SERVER_ERROR,
        };
        InvocationErrorResponse {
            status,
            error: error.kind.as_str(),
            message: error.message,
        }
    })?;
    Ok((StatusCode::OK, Json(outcome)))
}

#[derive(Serialize)]
struct PingResponse {
    status: &'static str,
}

#[async_trait]
pub trait InvocationHandler: Send + Sync {
    async fn handle(
        &self,
        payload: InvocationPayload,
    ) -> Result<InvocationOutcome, InvocationError>;
}

#[async_trait]
pub trait InvocationIdempotency: Send + Sync {
    async fn claim(&self, key: &str) -> Result<bool, InvocationIdempotencyError>;
}

#[derive(Debug, thiserror::Error)]
#[error("{message}")]
pub struct InvocationIdempotencyError {
    message: String,
}

impl InvocationIdempotencyError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

#[derive(Default)]
pub struct InMemoryInvocationIdempotency {
    seen: Mutex<HashSet<String>>,
}

impl InMemoryInvocationIdempotency {
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl InvocationIdempotency for InMemoryInvocationIdempotency {
    async fn claim(&self, key: &str) -> Result<bool, InvocationIdempotencyError> {
        Ok(self.seen.lock().await.insert(key.to_string()))
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum DynamoDbClaimResult {
    #[default]
    Claimed,
    Duplicate,
}

#[async_trait]
pub trait DynamoDbIdempotencyClient: Send + Sync {
    async fn put_claim(
        &self,
        table_name: &str,
        key: &str,
        expires_at_epoch_seconds: i64,
    ) -> Result<DynamoDbClaimResult, InvocationIdempotencyError>;
}

pub trait EpochSecondsClock: Send + Sync {
    fn now_epoch_seconds(&self) -> i64;
}

#[derive(Default)]
pub struct SystemEpochSecondsClock;

impl EpochSecondsClock for SystemEpochSecondsClock {
    fn now_epoch_seconds(&self) -> i64 {
        match SystemTime::now().duration_since(UNIX_EPOCH) {
            Ok(duration) => duration.as_secs().min(i64::MAX as u64) as i64,
            Err(_) => 0,
        }
    }
}

pub struct DynamoDbInvocationIdempotency {
    client: Arc<dyn DynamoDbIdempotencyClient>,
    table_name: String,
    ttl_seconds: i64,
    clock: Arc<dyn EpochSecondsClock>,
}

impl DynamoDbInvocationIdempotency {
    pub fn new(
        client: aws_sdk_dynamodb::Client,
        table_name: impl Into<String>,
        ttl_seconds: u64,
    ) -> Self {
        Self::from_parts(
            Arc::new(AwsDynamoDbIdempotencyClient::new(client)),
            table_name,
            ttl_seconds,
            Arc::new(SystemEpochSecondsClock),
        )
    }

    pub fn from_parts(
        client: Arc<dyn DynamoDbIdempotencyClient>,
        table_name: impl Into<String>,
        ttl_seconds: u64,
        clock: Arc<dyn EpochSecondsClock>,
    ) -> Self {
        Self {
            client,
            table_name: table_name.into(),
            ttl_seconds: ttl_seconds.min(i64::MAX as u64) as i64,
            clock,
        }
    }
}

#[async_trait]
impl InvocationIdempotency for DynamoDbInvocationIdempotency {
    async fn claim(&self, key: &str) -> Result<bool, InvocationIdempotencyError> {
        let expires_at_epoch_seconds = self
            .clock
            .now_epoch_seconds()
            .saturating_add(self.ttl_seconds);
        match self
            .client
            .put_claim(&self.table_name, key, expires_at_epoch_seconds)
            .await?
        {
            DynamoDbClaimResult::Claimed => Ok(true),
            DynamoDbClaimResult::Duplicate => Ok(false),
        }
    }
}

pub struct AwsDynamoDbIdempotencyClient {
    client: aws_sdk_dynamodb::Client,
}

impl AwsDynamoDbIdempotencyClient {
    pub fn new(client: aws_sdk_dynamodb::Client) -> Self {
        Self { client }
    }
}

#[async_trait]
impl DynamoDbIdempotencyClient for AwsDynamoDbIdempotencyClient {
    async fn put_claim(
        &self,
        table_name: &str,
        key: &str,
        expires_at_epoch_seconds: i64,
    ) -> Result<DynamoDbClaimResult, InvocationIdempotencyError> {
        let result = self
            .client
            .put_item()
            .table_name(table_name)
            .item("pk", AttributeValue::S(key.to_string()))
            .item(
                "expires_at",
                AttributeValue::N(expires_at_epoch_seconds.to_string()),
            )
            .condition_expression("attribute_not_exists(pk)")
            .send()
            .await;

        match result {
            Ok(_) => Ok(DynamoDbClaimResult::Claimed),
            Err(error)
                if error.as_service_error().is_some_and(|service_error| {
                    service_error.is_conditional_check_failed_exception()
                }) =>
            {
                Ok(DynamoDbClaimResult::Duplicate)
            }
            Err(error) => Err(InvocationIdempotencyError::new(format!(
                "put DynamoDB idempotency claim: {error}"
            ))),
        }
    }
}

#[derive(Clone)]
struct InvocationState {
    handler: SharedInvocationHandler,
    idempotency: SharedInvocationIdempotency,
}

pub struct SemanticReviewHandler {
    host: Arc<dyn ar_forge::ReviewHost>,
    dispatcher: Arc<dyn ar_orchestrator::JobDispatcher>,
}

impl SemanticReviewHandler {
    pub fn new(
        host: Arc<dyn ar_forge::ReviewHost>,
        dispatcher: Arc<dyn ar_orchestrator::JobDispatcher>,
    ) -> Self {
        Self { host, dispatcher }
    }
}

#[async_trait]
impl InvocationHandler for SemanticReviewHandler {
    async fn handle(
        &self,
        payload: InvocationPayload,
    ) -> Result<InvocationOutcome, InvocationError> {
        if payload.kind != InvocationKind::SemanticReview {
            return Err(InvocationError {
                kind: InvocationErrorKind::InvalidPayload,
                message: "only semantic_review invocations are supported".to_string(),
            });
        }

        let pr = self
            .host
            .get_pull_request(&payload.owner, &payload.repo, payload.pr_number)
            .await
            .map_err(|error| InvocationError {
                kind: InvocationErrorKind::ExecutionFailed,
                message: format!("fetch pull request: {error}"),
            })?;
        if pr.head.sha != payload.head_sha {
            return Err(InvocationError {
                kind: InvocationErrorKind::StaleHead,
                message: format!(
                    "payload head_sha {} does not match current PR head {}",
                    payload.head_sha, pr.head.sha
                ),
            });
        }

        self.dispatcher
            .dispatch(ar_orchestrator::ReviewJob {
                owner: payload.owner,
                repo: payload.repo,
                pr_number: payload.pr_number,
                head_sha: payload.head_sha,
                pr_title: pr.title,
                pr_body: pr.body,
                force: payload.force.unwrap_or(false),
            })
            .await;

        Ok(InvocationOutcome {
            status: "dispatched".to_string(),
            message: "semantic review dispatched".to_string(),
        })
    }
}

#[derive(Debug, Deserialize)]
pub struct InvocationPayload {
    pub provider: Provider,
    pub kind: InvocationKind,
    pub owner: String,
    pub repo: String,
    pub pr_number: u64,
    pub head_sha: String,
    #[serde(default)]
    pub installation_id: Option<u64>,
    #[serde(default)]
    pub force: Option<bool>,
    #[serde(default)]
    pub comment_id: Option<u64>,
    #[serde(default)]
    pub comment_body: Option<String>,
}

impl InvocationPayload {
    fn idempotency_key(&self) -> String {
        format!(
            "{:?}:{:?}:{}:{}:{}:{}:{}",
            self.provider,
            self.kind,
            self.owner,
            self.repo,
            self.pr_number,
            self.head_sha,
            self.comment_id
                .map(|id| id.to_string())
                .unwrap_or_else(|| "-".to_string())
        )
    }
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Provider {
    Forgejo,
    Github,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum InvocationKind {
    SemanticReview,
    ChatCommand,
}

#[derive(Serialize)]
struct InvocationAccepted {
    status: &'static str,
    provider: Provider,
    kind: InvocationKind,
    owner: String,
    repo: String,
    pr_number: u64,
    head_sha: String,
    force: bool,
}

#[derive(Debug, Serialize)]
pub struct InvocationOutcome {
    pub status: String,
    pub message: String,
}

#[derive(Debug, thiserror::Error)]
#[error("{message}")]
pub struct InvocationError {
    pub kind: InvocationErrorKind,
    pub message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InvocationErrorKind {
    InvalidPayload,
    StaleHead,
    ExecutionFailed,
}

impl InvocationErrorKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::InvalidPayload => "invalid_payload",
            Self::StaleHead => "stale_head",
            Self::ExecutionFailed => "execution_failed",
        }
    }
}

#[derive(Serialize)]
struct InvocationErrorBody {
    status: &'static str,
    error: &'static str,
    message: String,
}

struct InvocationErrorResponse {
    status: StatusCode,
    error: &'static str,
    message: String,
}

impl IntoResponse for InvocationErrorResponse {
    fn into_response(self) -> axum::response::Response {
        (
            self.status,
            Json(InvocationErrorBody {
                status: "error",
                error: self.error,
                message: self.message,
            }),
        )
            .into_response()
    }
}

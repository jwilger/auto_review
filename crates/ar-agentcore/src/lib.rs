//! AWS Bedrock AgentCore-compatible runtime HTTP surface.

extern crate alloc;

use alloc::sync::Arc;
use async_trait::async_trait;
use aws_sdk_dynamodb::types::AttributeValue;
use axum::extract::rejection::JsonRejection;
use axum::extract::State;
use axum::response::Response;
use axum::routing::get;
use axum::{http::StatusCode, response::IntoResponse, routing::post};
use axum::{Json, Router};
use core::net::SocketAddr;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::net::TcpListener;
use tokio::sync::Mutex;

/// Invocation handler shared across the router's request tasks.
type SharedInvocationHandler = Arc<dyn InvocationHandler>;
/// Idempotency store shared across the router's request tasks.
type SharedInvocationIdempotency = Arc<dyn InvocationIdempotency>;

#[derive(Clone)]
#[expect(
    clippy::exhaustive_structs,
    reason = "ar-cli and the integration tests build ServeConfig with a struct literal; #[non_exhaustive] would break that construction"
)]
pub struct ServeConfig {
    pub bind: String,
    pub handler: Option<SharedInvocationHandler>,
    pub idempotency: Option<SharedInvocationIdempotency>,
}

/// Body of the `/ping` health response.
#[derive(Serialize)]
struct PingResponse {
    /// Health status reported to the `AgentCore` runtime.
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
    /// Human-readable description of the idempotency failure.
    message: String,
}

impl InvocationIdempotencyError {
    #[inline]
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

#[derive(Default)]
pub struct InMemoryInvocationIdempotency {
    /// Idempotency keys already claimed by this process.
    seen: Mutex<HashSet<String>>,
}

impl InMemoryInvocationIdempotency {
    #[inline]
    #[must_use = "the store does nothing until a router or caller uses it to claim keys"]
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl InvocationIdempotency for InMemoryInvocationIdempotency {
    #[inline]
    async fn claim(&self, key: &str) -> Result<bool, InvocationIdempotencyError> {
        Ok(self.seen.lock().await.insert(key.to_owned()))
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
#[expect(
    clippy::exhaustive_structs,
    reason = "unit struct other crates may construct by name; #[non_exhaustive] would forbid `SystemEpochSecondsClock` outside this crate"
)]
pub struct SystemEpochSecondsClock;

impl EpochSecondsClock for SystemEpochSecondsClock {
    #[inline]
    fn now_epoch_seconds(&self) -> i64 {
        match SystemTime::now().duration_since(UNIX_EPOCH) {
            Ok(duration) => i64::try_from(duration.as_secs()).unwrap_or(i64::MAX),
            Err(_) => 0,
        }
    }
}

pub struct DynamoDbInvocationIdempotency {
    /// `DynamoDB` client used to write conditional claims.
    client: Arc<dyn DynamoDbIdempotencyClient>,
    /// Clock used to compute the claim's TTL expiry.
    clock: Arc<dyn EpochSecondsClock>,
    /// `DynamoDB` table holding the idempotency claims.
    table_name: String,
    /// Seconds after the claim time at which `DynamoDB` may expire the claim.
    ttl_seconds: i64,
}

impl DynamoDbInvocationIdempotency {
    #[inline]
    pub fn from_parts(
        client: Arc<dyn DynamoDbIdempotencyClient>,
        table_name: impl Into<String>,
        ttl_seconds: u64,
        clock: Arc<dyn EpochSecondsClock>,
    ) -> Self {
        Self {
            client,
            clock,
            table_name: table_name.into(),
            ttl_seconds: i64::try_from(ttl_seconds).unwrap_or(i64::MAX),
        }
    }

    #[inline]
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
}

#[async_trait]
impl InvocationIdempotency for DynamoDbInvocationIdempotency {
    #[inline]
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
    /// AWS SDK `DynamoDB` client.
    client: aws_sdk_dynamodb::Client,
}

impl AwsDynamoDbIdempotencyClient {
    #[inline]
    #[must_use = "the client does nothing until an idempotency store uses it to put claims"]
    pub fn new(client: aws_sdk_dynamodb::Client) -> Self {
        Self { client }
    }
}

#[async_trait]
impl DynamoDbIdempotencyClient for AwsDynamoDbIdempotencyClient {
    #[inline]
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
            .item("pk", AttributeValue::S(key.to_owned()))
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

/// Router state for the handler-plus-idempotency `/invocations` route.
#[derive(Clone)]
struct InvocationState {
    /// Handler that executes claimed invocations.
    handler: SharedInvocationHandler,
    /// Store that rejects invocations already handled.
    idempotency: SharedInvocationIdempotency,
}

pub struct SemanticReviewHandler {
    /// Dispatcher that runs the semantic review job.
    dispatcher: Arc<dyn ar_orchestrator::JobDispatcher>,
    /// Repository host used to check the PR's current head.
    host: Arc<dyn ar_forge::ReviewHost>,
}

impl SemanticReviewHandler {
    #[inline]
    pub fn new(
        host: Arc<dyn ar_forge::ReviewHost>,
        dispatcher: Arc<dyn ar_orchestrator::JobDispatcher>,
    ) -> Self {
        Self { dispatcher, host }
    }
}

#[async_trait]
impl InvocationHandler for SemanticReviewHandler {
    #[inline]
    async fn handle(
        &self,
        payload: InvocationPayload,
    ) -> Result<InvocationOutcome, InvocationError> {
        if payload.kind != InvocationKind::SemanticReview {
            return Err(InvocationError {
                kind: InvocationErrorKind::InvalidPayload,
                message: "only semantic_review invocations are supported".to_owned(),
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
            status: "dispatched".to_owned(),
            message: "semantic review dispatched".to_owned(),
        })
    }
}

#[derive(Debug, Deserialize)]
#[expect(
    clippy::exhaustive_structs,
    reason = "ar-cli and the integration tests build InvocationPayload with a struct literal; #[non_exhaustive] would break that construction"
)]
#[expect(
    clippy::arbitrary_source_item_ordering,
    reason = "serde reports the first missing field in declaration order, so reordering would change the 400 invalid_payload message"
)]
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
    /// Key identifying one logical invocation, so retries of the same
    /// review or chat command are handled at most once.
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
                .map_or_else(|| "-".to_owned(), |id| id.to_string())
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
    ChatCommand,
    SemanticReview,
}

/// Body of the `202 Accepted` response from the handler-less `/invocations` route.
#[derive(Serialize)]
#[expect(
    clippy::arbitrary_source_item_ordering,
    reason = "field order is the serialized JSON key order of the /invocations response body, which this change keeps"
)]
struct InvocationAccepted {
    /// Always `accepted`.
    status: &'static str,
    /// Repository host named by the invocation.
    provider: Provider,
    /// Kind of work the invocation requests.
    kind: InvocationKind,
    /// Repository owner.
    owner: String,
    /// Repository name.
    repo: String,
    /// Pull request number.
    pr_number: u64,
    /// Head commit the invocation targets.
    head_sha: String,
    /// Whether the invocation forces a review.
    force: bool,
}

#[derive(Debug, Serialize)]
#[expect(
    clippy::exhaustive_structs,
    reason = "ar-cli and the integration tests build InvocationOutcome with a struct literal; #[non_exhaustive] would break that construction"
)]
#[expect(
    clippy::arbitrary_source_item_ordering,
    reason = "field order is the serialized JSON key order of the /invocations response body, which this change keeps"
)]
pub struct InvocationOutcome {
    pub status: String,
    pub message: String,
}

#[derive(Debug, thiserror::Error)]
#[error("{message}")]
#[expect(
    clippy::exhaustive_structs,
    reason = "ar-cli builds InvocationError with a struct literal; #[non_exhaustive] would break that construction"
)]
pub struct InvocationError {
    pub kind: InvocationErrorKind,
    pub message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InvocationErrorKind {
    ExecutionFailed,
    InvalidPayload,
    StaleHead,
}

impl InvocationErrorKind {
    /// Stable machine-readable error code returned in error response bodies.
    const fn as_str(self) -> &'static str {
        match self {
            Self::ExecutionFailed => "execution_failed",
            Self::InvalidPayload => "invalid_payload",
            Self::StaleHead => "stale_head",
        }
    }
}

/// JSON body of an `/invocations` error response.
#[derive(Serialize)]
#[expect(
    clippy::arbitrary_source_item_ordering,
    reason = "field order is the serialized JSON key order of the /invocations error body, which this change keeps"
)]
struct InvocationErrorBody {
    /// Always `error`.
    status: &'static str,
    /// Machine-readable error code.
    error: &'static str,
    /// Human-readable error detail.
    message: String,
}

/// `/invocations` failure carrying its HTTP status and JSON error body.
struct InvocationErrorResponse {
    /// Machine-readable error code.
    error: &'static str,
    /// Human-readable error detail.
    message: String,
    /// HTTP status returned to the caller.
    status: StatusCode,
}

impl IntoResponse for InvocationErrorResponse {
    #[inline]
    fn into_response(self) -> Response {
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

#[inline]
pub fn build_router() -> Router {
    Router::new()
        .route("/ping", get(ping))
        .route("/invocations", post(invocations))
}

#[inline]
pub fn build_router_from_config(config: &ServeConfig) -> Router {
    match config.handler.as_ref() {
        Some(handler) => build_router_with_handler_and_idempotency(
            Arc::clone(handler),
            config
                .idempotency
                .clone()
                .unwrap_or_else(|| Arc::new(InMemoryInvocationIdempotency::new())),
        ),
        None => build_router(),
    }
}

#[inline]
pub fn build_router_with_handler(handler: SharedInvocationHandler) -> Router {
    Router::new()
        .route("/ping", get(ping))
        .route("/invocations", post(invocations_with_handler))
        .with_state(handler)
}

#[inline]
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

/// Accepts a valid invocation without executing it (no handler configured).
async fn invocations(
    body: Result<Json<InvocationPayload>, JsonRejection>,
) -> Result<impl IntoResponse, InvocationErrorResponse> {
    let Json(payload) = body.map_err(|rejection| InvocationErrorResponse {
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

/// Executes every valid invocation through the configured handler.
async fn invocations_with_handler(
    State(handler): State<SharedInvocationHandler>,
    body: Result<Json<InvocationPayload>, JsonRejection>,
) -> Result<impl IntoResponse, InvocationErrorResponse> {
    let Json(payload) = body.map_err(|rejection| InvocationErrorResponse {
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

/// Executes a valid invocation through the handler once per idempotency key.
async fn invocations_with_idempotency(
    State(state): State<InvocationState>,
    body: Result<Json<InvocationPayload>, JsonRejection>,
) -> Result<impl IntoResponse, InvocationErrorResponse> {
    let Json(payload) = body.map_err(|rejection| InvocationErrorResponse {
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
                status: "duplicate".to_owned(),
                message: "invocation already handled".to_owned(),
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

/// Answers the `AgentCore` runtime health check.
async fn ping() -> Json<PingResponse> {
    Json(PingResponse { status: "healthy" })
}

/// Binds `config.bind` and serves the `AgentCore` runtime routes until the
/// server stops.
///
/// # Errors
///
/// Returns an error when `config.bind` is not a socket address, the address
/// cannot be bound, or the server fails while serving.
#[inline]
pub async fn serve(config: ServeConfig) -> anyhow::Result<()> {
    let addr: SocketAddr = config.bind.parse().map_err(|error| {
        anyhow::anyhow!("invalid AgentCore bind address `{}`: {error}", config.bind)
    })?;
    let listener = TcpListener::bind(addr).await?;
    tracing::info!(%addr, "AgentCore runtime listening");
    axum::serve(listener, build_router_from_config(&config)).await?;
    Ok(())
}

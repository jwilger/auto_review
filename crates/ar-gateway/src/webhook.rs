use crate::hmac::{verify, HmacError};
use crate::AppState;
use ar_chat::{parse_chat_command, ChatCommand, ChatContext, ChatHandler};
use ar_forgejo::{IssueCommentEvent, PullRequestAction, PullRequestEvent};
use ar_orchestrator::ReviewJob;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};

const SIG_HEADER: &str = "x-forgejo-signature";
const FALLBACK_SIG_HEADER: &str = "x-gitea-signature";
const EVENT_HEADER: &str = "x-forgejo-event";
const FALLBACK_EVENT_HEADER: &str = "x-gitea-event";
const DELIVERY_HEADER: &str = "x-forgejo-delivery";
const FALLBACK_DELIVERY_HEADER: &str = "x-gitea-delivery";

/// Top-level webhook handler.
///
/// 1. HMAC-verifies the body against the configured secret.
/// 2. Dispatches by `X-Forgejo-Event`.
/// 3. For `pull_request` opened/synchronized/reopened, decodes the payload
///    and hands a [`ReviewJob`] to the configured dispatcher.
pub async fn handle(State(state): State<AppState>, headers: HeaderMap, body: Bytes) -> Response {
    // Global throttle (T7 mitigation). Checked before HMAC verify so
    // a flood of unsigned junk can't burn CPU on signature math.
    // Operators leave this unconfigured by default; main.rs wires it
    // when AR_WEBHOOK_RATE_PER_SEC is set.
    if let Some(bucket) = state.webhook_rate_limit.as_ref() {
        if !bucket.try_take() {
            state.metrics.record_rate_limited();
            return reject(StatusCode::TOO_MANY_REQUESTS, "rate limit");
        }
    }

    let sig = headers
        .get(SIG_HEADER)
        .or_else(|| headers.get(FALLBACK_SIG_HEADER))
        .and_then(|v| v.to_str().ok());
    let Some(sig) = sig else {
        state.metrics.record_signature_failure();
        return reject(StatusCode::UNAUTHORIZED, "missing signature");
    };

    if let Err(e) = verify(&state.webhook_secret, &body, sig) {
        state.metrics.record_signature_failure();
        let status = match e {
            HmacError::Mismatch => StatusCode::UNAUTHORIZED,
            HmacError::Missing | HmacError::NotHex => StatusCode::BAD_REQUEST,
            HmacError::InvalidSecret => StatusCode::INTERNAL_SERVER_ERROR,
        };
        return reject(status, &format!("signature: {e}"));
    }

    let event = headers
        .get(EVENT_HEADER)
        .or_else(|| headers.get(FALLBACK_EVENT_HEADER))
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    state.metrics.record_event(event);

    // Delivery dedup: a Forgejo retry of the same delivery
    // (network blip, gateway restart) gets the same
    // X-Forgejo-Delivery UUID. We've already verified HMAC, so
    // bouncing duplicates here saves the orchestrator from a
    // racing second dispatch.
    if let Some(dedup) = state.webhook_dedup.as_ref() {
        let delivery_id = headers
            .get(DELIVERY_HEADER)
            .or_else(|| headers.get(FALLBACK_DELIVERY_HEADER))
            .and_then(|v| v.to_str().ok());
        if let Some(id) = delivery_id {
            if matches!(
                dedup.check_and_record(id),
                crate::dedup::CheckResult::Duplicate
            ) {
                state.metrics.record_duplicate();
                tracing::debug!(
                    delivery_id = id,
                    "duplicate delivery; replying OK without dispatch"
                );
                return (StatusCode::OK, "duplicate").into_response();
            }
        }
        // No header present: nothing to dedup against. Fall
        // through. Self-hosted Forgejo always sets the header
        // when configured to do so; old versions or custom
        // webhook posters might not.
    }

    match event {
        "pull_request" => handle_pull_request(&state, &body).await,
        "issue_comment" => handle_issue_comment(&state, &body).await,
        "ping" => (StatusCode::OK, "pong").into_response(),
        other => {
            tracing::debug!(event = other, "ignoring webhook event");
            (StatusCode::ACCEPTED, "").into_response()
        }
    }
}

async fn handle_issue_comment(state: &AppState, body: &[u8]) -> Response {
    let evt: IssueCommentEvent = match serde_json::from_slice(body) {
        Ok(e) => e,
        Err(e) => {
            state.metrics.record_payload_failure();
            return reject(StatusCode::BAD_REQUEST, &format!("payload decode: {e}"));
        }
    };
    // Only act on freshly-created comments. Edited comments would
    // re-fire the bot and produce duplicate replies (the user
    // edited their `@bot help` to add another mention; Forgejo
    // sends a second webhook). Deleted comments shouldn't trigger
    // anything either — the user already removed the mention.
    use ar_forgejo::IssueCommentAction;
    if evt.action != IssueCommentAction::Created {
        tracing::debug!(
            action = ?evt.action,
            "ignoring non-Created issue_comment action"
        );
        return (StatusCode::ACCEPTED, "").into_response();
    }
    if !evt.is_pull_request_comment() {
        // Plain issue (not PR) — ignored.
        return (StatusCode::ACCEPTED, "").into_response();
    }
    if is_bot_self(&evt.sender.login, &state.bot_login) {
        // Bot's own comments must be ignored to avoid loops.
        return (StatusCode::ACCEPTED, "").into_response();
    }
    let cmd = parse_chat_command(&evt.comment.body, &state.bot_name);
    if matches!(cmd, ChatCommand::NotMentioned) {
        return (StatusCode::ACCEPTED, "").into_response();
    }
    state.metrics.record_chat_command();

    // Hand off to the chat handler if it's wired up. Spawn the work so
    // the webhook ack stays fast — chat replies typically involve at
    // least one Forgejo round-trip and may include an LLM embed.
    if let Some(chat) = state.chat.clone() {
        let owner = evt.repository.owner.login.clone();
        let repo = evt.repository.name.clone();
        let issue_number = evt.issue.number;
        let cmd_for_log = format!("{cmd:?}");
        let dispatcher = state.dispatcher.clone();
        tokio::spawn(async move {
            let handler = ChatHandler {
                forgejo: &chat.forgejo,
                llm: &chat.llm,
                learnings: chat.learnings.as_ref(),
                dispatcher: Some(dispatcher),
            };
            let ctx = ChatContext {
                owner: &owner,
                repo: &repo,
                issue_number,
            };
            if let Err(e) = handler.handle(ctx, cmd).await {
                tracing::error!(
                    %owner, %repo, issue = issue_number, error = %e,
                    command = %cmd_for_log,
                    "chat handler failed"
                );
            }
        });
    } else {
        state.metrics.record_chat_unconfigured();
        tracing::warn!(
            repo = %evt.repository.full_name,
            issue = evt.issue.number,
            command = ?cmd,
            "chat command received but ChatDeps not configured; ignoring"
        );
    }
    (StatusCode::ACCEPTED, "").into_response()
}

fn is_bot_self(sender_login: &str, bot_login: &str) -> bool {
    sender_login.eq_ignore_ascii_case(bot_login)
}

async fn handle_pull_request(state: &AppState, body: &[u8]) -> Response {
    let evt: PullRequestEvent = match serde_json::from_slice(body) {
        Ok(e) => e,
        Err(e) => {
            state.metrics.record_payload_failure();
            return reject(StatusCode::BAD_REQUEST, &format!("payload decode: {e}"));
        }
    };
    if !is_actionable(evt.action) {
        tracing::debug!(action = ?evt.action, "ignoring non-review-triggering action");
        return (StatusCode::ACCEPTED, "").into_response();
    }
    if evt.pull_request.draft {
        tracing::debug!(number = evt.number, "ignoring draft PR");
        return (StatusCode::ACCEPTED, "").into_response();
    }
    tracing::info!(
        repo = %evt.repository.full_name,
        number = evt.number,
        action = ?evt.action,
        head = %evt.pull_request.head.sha,
        "accepted PR for review",
    );
    state.dispatcher.dispatch(ReviewJob::from(&evt)).await;
    state.metrics.record_job_dispatched();
    (StatusCode::ACCEPTED, "").into_response()
}

fn is_actionable(action: PullRequestAction) -> bool {
    matches!(
        action,
        PullRequestAction::Opened
            | PullRequestAction::Synchronized
            | PullRequestAction::Reopened
            | PullRequestAction::ReadyForReview,
    )
}

fn reject(status: StatusCode, msg: &str) -> Response {
    tracing::warn!(%status, msg, "rejecting webhook");
    (status, msg.to_string()).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::build_router;
    use ::hmac::{Hmac, Mac};
    use ar_orchestrator::{JobDispatcher, NoOpDispatcher};
    use async_trait::async_trait;
    use axum::body::Body;
    use axum::http::Request;
    use http_body_util::BodyExt;
    use sha2::Sha256;
    use std::sync::{Arc, Mutex};
    use tower::ServiceExt;

    type HmacSha256 = Hmac<Sha256>;

    struct RecordingDispatcher {
        seen: Mutex<Vec<ReviewJob>>,
    }

    impl RecordingDispatcher {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                seen: Mutex::new(Vec::new()),
            })
        }
        fn jobs(&self) -> Vec<ReviewJob> {
            self.seen.lock().unwrap().clone()
        }
    }

    #[async_trait]
    impl JobDispatcher for RecordingDispatcher {
        async fn dispatch(&self, job: ReviewJob) {
            self.seen.lock().unwrap().push(job);
        }
    }

    fn sign(secret: &str, body: &[u8]) -> String {
        let mut mac = HmacSha256::new_from_slice(secret.as_bytes()).unwrap();
        mac.update(body);
        hex::encode(mac.finalize().into_bytes())
    }

    fn pr_payload(action: &str, draft: bool) -> Vec<u8> {
        serde_json::to_vec(&serde_json::json!({
            "action": action,
            "number": 7,
            "pull_request": {
                "number": 7,
                "title": "x",
                "body": "",
                "draft": draft,
                "user": {"login": "u", "id": 1},
                "head": {"ref": "t", "sha": "deadbeef"},
                "base": {"ref": "main", "sha": "cafef00d"}
            },
            "repository": {
                "name": "r", "full_name": "o/r", "default_branch": "main",
                "owner": {"login": "o", "id": 99}
            },
            "sender": {"login": "u", "id": 1}
        }))
        .unwrap()
    }

    async fn send(
        secret: &str,
        event: &str,
        body: Vec<u8>,
        sig: Option<&str>,
        dispatcher: Arc<dyn JobDispatcher>,
    ) -> (StatusCode, String) {
        let app = build_router(AppState::new(secret, dispatcher));
        let mut req = Request::post("/webhooks/forgejo").header(EVENT_HEADER, event);
        if let Some(s) = sig {
            req = req.header(SIG_HEADER, s);
        }
        let req = req.body(Body::from(body)).unwrap();
        let resp = app.oneshot(req).await.unwrap();
        let status = resp.status();
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        (status, String::from_utf8_lossy(&bytes).into_owned())
    }

    #[tokio::test]
    async fn missing_signature_is_unauthorized() {
        let (status, _) = send(
            "s",
            "pull_request",
            pr_payload("opened", false),
            None,
            Arc::new(NoOpDispatcher),
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn bad_signature_is_unauthorized() {
        let body = pr_payload("opened", false);
        let (status, _) = send(
            "s",
            "pull_request",
            body,
            Some("00"),
            Arc::new(NoOpDispatcher),
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn valid_pr_opened_is_accepted_and_dispatched() {
        let body = pr_payload("opened", false);
        let sig = sign("s", &body);
        let recorder = RecordingDispatcher::new();
        let (status, _) = send("s", "pull_request", body, Some(&sig), recorder.clone()).await;
        assert_eq!(status, StatusCode::ACCEPTED);
        let jobs = recorder.jobs();
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].pr_number, 7);
        assert_eq!(jobs[0].owner, "o");
        assert_eq!(jobs[0].repo, "r");
        assert_eq!(jobs[0].head_sha, "deadbeef");
    }

    #[tokio::test]
    async fn draft_pr_is_skipped_and_not_dispatched() {
        let body = pr_payload("opened", true);
        let sig = sign("s", &body);
        let recorder = RecordingDispatcher::new();
        let (status, _) = send("s", "pull_request", body, Some(&sig), recorder.clone()).await;
        assert_eq!(status, StatusCode::ACCEPTED);
        assert!(recorder.jobs().is_empty());
    }

    #[tokio::test]
    async fn non_actionable_action_is_not_dispatched() {
        let body = pr_payload("closed", false);
        let sig = sign("s", &body);
        let recorder = RecordingDispatcher::new();
        let (status, _) = send("s", "pull_request", body, Some(&sig), recorder.clone()).await;
        assert_eq!(status, StatusCode::ACCEPTED);
        assert!(recorder.jobs().is_empty());
    }

    #[tokio::test]
    async fn version_endpoint_returns_name_and_version() {
        let app = build_router(AppState::new("s", Arc::new(NoOpDispatcher)));
        let req = Request::get("/version").body(Body::empty()).unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(json["name"], "auto_review");
        assert!(!json["version"].as_str().unwrap().is_empty());
    }

    #[tokio::test]
    async fn healthz_returns_ok() {
        let app = build_router(AppState::new("s", Arc::new(NoOpDispatcher)));
        let req = Request::get("/healthz").body(Body::empty()).unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn info_without_wiring_returns_fallback() {
        let app = build_router(AppState::new("s", Arc::new(NoOpDispatcher)));
        let req = Request::get("/info").body(Body::empty()).unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(json["name"], "auto_review");
        assert_eq!(json["info"], "not wired");
    }

    #[tokio::test]
    async fn info_with_full_wiring_returns_runtime_snapshot() {
        use crate::GatewayInfo;
        let info = Arc::new(GatewayInfo {
            name: "auto_review",
            version: "0.0.1",
            bot_login: "pr-bot".into(),
            bot_name: "pr-bot".into(),
            sandbox: "podman",
            learnings: "sqlite",
            history: "sqlite",
            llm_tiers: vec!["reasoning", "cheap", "embedding"],
            reasoning_model: "qwen2.5-coder:32b".into(),
            poller_enabled: true,
            readiness_enabled: true,
        });
        let app = build_router(AppState::new("s", Arc::new(NoOpDispatcher)).with_info(info));
        let req = Request::get("/info").body(Body::empty()).unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(json["bot_login"], "pr-bot");
        assert_eq!(json["sandbox"], "podman");
        assert_eq!(json["learnings"], "sqlite");
        assert_eq!(json["reasoning_model"], "qwen2.5-coder:32b");
        assert_eq!(json["poller_enabled"], true);
        assert_eq!(json["readiness_enabled"], true);
        let tiers: Vec<String> = json["llm_tiers"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap().to_owned())
            .collect();
        assert_eq!(tiers, vec!["reasoning", "cheap", "embedding"]);
    }

    #[tokio::test]
    async fn readyz_without_probe_returns_200() {
        let app = build_router(AppState::new("s", Arc::new(NoOpDispatcher)));
        let req = Request::get("/readyz").body(Body::empty()).unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let text = String::from_utf8_lossy(&bytes);
        assert!(text.contains("no probe configured"));
    }

    #[tokio::test]
    async fn readyz_with_healthy_probe_returns_200() {
        use crate::ReadinessProbe;
        use ar_forgejo::Client as ForgejoClient;
        use std::sync::Arc;
        use std::time::Duration;
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/version"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({"version": "9.0.0"})),
            )
            .mount(&server)
            .await;

        let forgejo = Arc::new(ForgejoClient::new(&server.uri(), "tok").unwrap());
        let probe = Arc::new(ReadinessProbe::with_ttl(forgejo, Duration::from_secs(60)));
        let app = build_router(AppState::new("s", Arc::new(NoOpDispatcher)).with_readiness(probe));
        let req = Request::get("/readyz").body(Body::empty()).unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let text = String::from_utf8_lossy(&bytes);
        assert!(text.contains("9.0.0"), "{text}");
    }

    #[tokio::test]
    async fn readyz_with_failing_probe_returns_503() {
        use crate::ReadinessProbe;
        use ar_forgejo::Client as ForgejoClient;
        use std::sync::Arc;
        use std::time::Duration;
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/version"))
            .respond_with(ResponseTemplate::new(500))
            .mount(&server)
            .await;

        let forgejo = Arc::new(ForgejoClient::new(&server.uri(), "tok").unwrap());
        let probe = Arc::new(ReadinessProbe::with_ttl(forgejo, Duration::from_secs(60)));
        let app = build_router(AppState::new("s", Arc::new(NoOpDispatcher)).with_readiness(probe));
        let req = Request::get("/readyz").body(Body::empty()).unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[tokio::test]
    async fn readiness_cache_serves_repeat_calls_without_re_probing() {
        use crate::ReadinessProbe;
        use ar_forgejo::Client as ForgejoClient;
        use std::sync::Arc;
        use std::time::Duration;
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        // `expect(1)` asserts the upstream is hit exactly once even
        // though we call `check()` three times within the TTL.
        Mock::given(method("GET"))
            .and(path("/api/v1/version"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({"version": "9.0.0"})),
            )
            .expect(1)
            .mount(&server)
            .await;

        let forgejo = Arc::new(ForgejoClient::new(&server.uri(), "tok").unwrap());
        let probe = ReadinessProbe::with_ttl(forgejo, Duration::from_secs(60));
        let (h1, _) = probe.check().await;
        let (h2, _) = probe.check().await;
        let (h3, _) = probe.check().await;
        assert!(h1 && h2 && h3);
    }

    #[tokio::test]
    async fn metrics_endpoint_emits_prometheus_text_format() {
        let app = build_router(AppState::new("s", Arc::new(NoOpDispatcher)));
        let req = Request::get("/metrics").body(Body::empty()).unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let ct = resp
            .headers()
            .get("content-type")
            .unwrap()
            .to_str()
            .unwrap()
            .to_owned();
        assert!(ct.starts_with("text/plain"));
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let text = String::from_utf8_lossy(&bytes);
        assert!(text.contains("# HELP auto_review_webhooks_pull_request_total"));
        assert!(text.contains("# TYPE auto_review_webhooks_pull_request_total counter"));
        assert!(text.contains("auto_review_jobs_dispatched_total 0\n"));
    }

    #[tokio::test]
    async fn metrics_track_dispatched_pr() {
        let body = pr_payload("opened", false);
        let sig = sign("s", &body);
        let recorder = RecordingDispatcher::new();
        let app = build_router(AppState::new(
            "s",
            recorder.clone() as Arc<dyn JobDispatcher>,
        ));
        let req = Request::post("/webhooks/forgejo")
            .header(EVENT_HEADER, "pull_request")
            .header(SIG_HEADER, sig)
            .body(Body::from(body))
            .unwrap();
        let resp = app.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::ACCEPTED);

        let req = Request::get("/metrics").body(Body::empty()).unwrap();
        let resp = app.oneshot(req).await.unwrap();
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let text = String::from_utf8_lossy(&bytes);
        assert!(text.contains("auto_review_webhooks_pull_request_total 1\n"));
        assert!(text.contains("auto_review_jobs_dispatched_total 1\n"));
    }

    #[tokio::test]
    async fn webhook_dedup_replies_ok_without_dispatching_on_retry() {
        use crate::dedup::RecentDeliveries;
        let dedup = Arc::new(RecentDeliveries::new(8));
        let recorder = RecordingDispatcher::new();
        let app = build_router(
            AppState::new("s", recorder.clone() as Arc<dyn JobDispatcher>)
                .with_webhook_dedup(dedup),
        );
        let body = pr_payload("opened", false);
        let sig = sign("s", &body);

        // First delivery: dispatched.
        let req = Request::post("/webhooks/forgejo")
            .header(EVENT_HEADER, "pull_request")
            .header(SIG_HEADER, &sig)
            .header(DELIVERY_HEADER, "uuid-1")
            .body(Body::from(body.clone()))
            .unwrap();
        let resp = app.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::ACCEPTED);
        assert_eq!(recorder.jobs().len(), 1);

        // Forgejo retries with the same delivery id: 200 OK,
        // no second dispatch.
        let req = Request::post("/webhooks/forgejo")
            .header(EVENT_HEADER, "pull_request")
            .header(SIG_HEADER, &sig)
            .header(DELIVERY_HEADER, "uuid-1")
            .body(Body::from(body.clone()))
            .unwrap();
        let resp = app.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(
            recorder.jobs().len(),
            1,
            "duplicate must NOT trigger a second dispatch"
        );

        // The duplicate counter ticks for the rejected request.
        let req = Request::get("/metrics").body(Body::empty()).unwrap();
        let resp = app.oneshot(req).await.unwrap();
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let text = String::from_utf8_lossy(&bytes);
        assert!(
            text.contains("auto_review_webhook_duplicates_total 1\n"),
            "{text}"
        );
    }

    #[tokio::test]
    async fn webhook_dedup_passes_through_when_no_delivery_header() {
        use crate::dedup::RecentDeliveries;
        // Some old Forgejo / custom posters don't set the
        // delivery header. We must still process the request.
        let dedup = Arc::new(RecentDeliveries::new(8));
        let recorder = RecordingDispatcher::new();
        let app = build_router(
            AppState::new("s", recorder.clone() as Arc<dyn JobDispatcher>)
                .with_webhook_dedup(dedup),
        );
        let body = pr_payload("opened", false);
        let sig = sign("s", &body);
        // No DELIVERY_HEADER.
        let req = Request::post("/webhooks/forgejo")
            .header(EVENT_HEADER, "pull_request")
            .header(SIG_HEADER, &sig)
            .body(Body::from(body))
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::ACCEPTED);
        assert_eq!(recorder.jobs().len(), 1);
    }

    #[tokio::test]
    async fn webhook_throttle_returns_429_when_bucket_empty() {
        use crate::ratelimit::TokenBucket;
        // Burst=2: first two webhooks pass; the third is throttled.
        let bucket = Arc::new(TokenBucket::new(2, 1));
        let app = build_router(
            AppState::new("s", Arc::new(NoOpDispatcher)).with_webhook_rate_limit(bucket),
        );
        let body = pr_payload("opened", false);
        let sig = sign("s", &body);
        for _ in 0..2 {
            let req = Request::post("/webhooks/forgejo")
                .header(EVENT_HEADER, "pull_request")
                .header(SIG_HEADER, &sig)
                .body(Body::from(body.clone()))
                .unwrap();
            let resp = app.clone().oneshot(req).await.unwrap();
            assert_eq!(resp.status(), StatusCode::ACCEPTED);
        }
        // Third hit: bucket empty.
        let req = Request::post("/webhooks/forgejo")
            .header(EVENT_HEADER, "pull_request")
            .header(SIG_HEADER, &sig)
            .body(Body::from(body))
            .unwrap();
        let resp = app.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::TOO_MANY_REQUESTS);

        // The metric ticks for the rejected request only.
        let req = Request::get("/metrics").body(Body::empty()).unwrap();
        let resp = app.oneshot(req).await.unwrap();
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let text = String::from_utf8_lossy(&bytes);
        assert!(
            text.contains("auto_review_webhook_rate_limited_total 1\n"),
            "{text}"
        );
    }

    #[tokio::test]
    async fn webhook_throttle_runs_before_hmac_so_unsigned_floods_dont_burn_cpu() {
        use crate::ratelimit::TokenBucket;
        // Burst=1, no signature on the request → throttle still
        // takes the token, then HMAC reject would happen, then
        // throttle on next try.
        let bucket = Arc::new(TokenBucket::new(1, 1));
        let app = build_router(
            AppState::new("s", Arc::new(NoOpDispatcher)).with_webhook_rate_limit(bucket),
        );
        // First unsigned request: throttle passes (token spent),
        // HMAC verify fails → 401.
        let req = Request::post("/webhooks/forgejo")
            .header(EVENT_HEADER, "pull_request")
            .body(Body::from(b"{}".to_vec()))
            .unwrap();
        let resp = app.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
        // Second unsigned request: throttle empty → 429
        // (NOT reaching HMAC verify, so we save the CPU).
        let req = Request::post("/webhooks/forgejo")
            .header(EVENT_HEADER, "pull_request")
            .body(Body::from(b"{}".to_vec()))
            .unwrap();
        let resp = app.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::TOO_MANY_REQUESTS);
    }

    #[tokio::test]
    async fn metrics_track_signature_failures() {
        let body = pr_payload("opened", false);
        let app = build_router(AppState::new("s", Arc::new(NoOpDispatcher)));
        let req = Request::post("/webhooks/forgejo")
            .header(EVENT_HEADER, "pull_request")
            .body(Body::from(body))
            .unwrap();
        let resp = app.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);

        let req = Request::get("/metrics").body(Body::empty()).unwrap();
        let resp = app.oneshot(req).await.unwrap();
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let text = String::from_utf8_lossy(&bytes);
        assert!(text.contains("auto_review_webhook_signature_failures_total 1\n"));
        assert!(text.contains("auto_review_jobs_dispatched_total 0\n"));
    }

    #[tokio::test]
    async fn ping_is_pong() {
        let body = b"{}".to_vec();
        let sig = sign("s", &body);
        let (status, body) = send("s", "ping", body, Some(&sig), Arc::new(NoOpDispatcher)).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, "pong");
    }

    fn comment_payload(action: &str, body: &str, sender: &str) -> Vec<u8> {
        serde_json::to_vec(&serde_json::json!({
            "action": action,
            "comment": {"id": 1, "body": body, "user": {"login": sender, "id": 1}},
            "issue": {
                "number": 7,
                "title": "x",
                "pull_request": {"html_url": "https://forge/o/r/pulls/7"}
            },
            "repository": {
                "name": "r", "full_name": "o/r", "default_branch": "main",
                "owner": {"login": "o", "id": 99}
            },
            "sender": {"login": sender, "id": 1}
        }))
        .unwrap()
    }

    #[tokio::test]
    async fn pr_issue_comment_with_mention_is_accepted() {
        let body = comment_payload("created", "@auto_review help", "alice");
        let sig = sign("s", &body);
        let (status, _) = send(
            "s",
            "issue_comment",
            body,
            Some(&sig),
            Arc::new(NoOpDispatcher),
        )
        .await;
        assert_eq!(status, StatusCode::ACCEPTED);
    }

    #[tokio::test]
    async fn issue_comment_with_edited_action_is_ignored() {
        // A user editing their `@bot help` would otherwise refire
        // the chat handler and post a duplicate reply. Forgejo
        // sends a separate webhook for the edit; the bot must
        // act only on Created.
        let secret = "s";
        let app = build_router(AppState::new(secret, Arc::new(NoOpDispatcher)));

        let body = comment_payload("edited", "@auto_review help", "alice");
        let sig = sign(secret, &body);
        let req = Request::post("/webhooks/forgejo")
            .header(EVENT_HEADER, "issue_comment")
            .header(SIG_HEADER, &sig)
            .body(Body::from(body))
            .unwrap();
        let resp = app.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::ACCEPTED);

        // No chat command should have been recorded.
        let metrics_req = Request::get("/metrics").body(Body::empty()).unwrap();
        let metrics_resp = app.oneshot(metrics_req).await.unwrap();
        let bytes = axum::body::to_bytes(metrics_resp.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let text = String::from_utf8_lossy(&bytes);
        assert!(
            text.contains("auto_review_chat_commands_received_total 0\n"),
            "edited comment must not increment chat counter:\n{text}"
        );
    }

    #[tokio::test]
    async fn issue_comment_from_bot_self_is_ignored() {
        // Sender matches the default bot_login (`auto_review`) —
        // must not act (would loop).
        let body = comment_payload("created", "@auto_review help", "auto_review");
        let sig = sign("s", &body);
        let (status, _) = send(
            "s",
            "issue_comment",
            body,
            Some(&sig),
            Arc::new(NoOpDispatcher),
        )
        .await;
        assert_eq!(status, StatusCode::ACCEPTED);
    }

    #[tokio::test]
    async fn issue_comment_self_detection_uses_configured_bot_login() {
        // Operator runs the bot under a different account
        // (`pr-bot`); a comment from that account must be filtered,
        // and a comment from a user named `auto_review_helper` must
        // NOT be filtered (it's not the bot).
        let secret = "s";
        let app = build_router(
            AppState::new(secret, Arc::new(NoOpDispatcher)).with_bot_identity("pr-bot", "pr-bot"),
        );

        // Bot's own comment: ignored (no chat command counter
        // increment).
        let body = comment_payload("created", "@pr-bot help", "pr-bot");
        let sig = sign(secret, &body);
        let req = Request::post("/webhooks/forgejo")
            .header(EVENT_HEADER, "issue_comment")
            .header(SIG_HEADER, &sig)
            .body(Body::from(body))
            .unwrap();
        let resp = app.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::ACCEPTED);

        // Non-bot user with "auto_review_helper" name: not the bot
        // any more under the new behaviour.
        let body = comment_payload("created", "@pr-bot help", "auto_review_helper");
        let sig = sign(secret, &body);
        let req = Request::post("/webhooks/forgejo")
            .header(EVENT_HEADER, "issue_comment")
            .header(SIG_HEADER, &sig)
            .body(Body::from(body))
            .unwrap();
        let resp = app.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::ACCEPTED);

        let req = Request::get("/metrics").body(Body::empty()).unwrap();
        let resp = app.oneshot(req).await.unwrap();
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let text = String::from_utf8_lossy(&bytes);
        // The bot's own message must not register; the
        // helper's message MUST register, totalling 1.
        assert!(
            text.contains("auto_review_chat_commands_received_total 1\n"),
            "expected exactly one chat command, got:\n{text}"
        );
    }

    #[tokio::test]
    async fn issue_comment_without_mention_is_accepted_silently() {
        let body = comment_payload("created", "thanks for the review", "alice");
        let sig = sign("s", &body);
        let (status, _) = send(
            "s",
            "issue_comment",
            body,
            Some(&sig),
            Arc::new(NoOpDispatcher),
        )
        .await;
        assert_eq!(status, StatusCode::ACCEPTED);
    }

    #[tokio::test]
    async fn unknown_event_is_accepted_silently() {
        let body = b"{}".to_vec();
        let sig = sign("s", &body);
        let (status, _) = send(
            "s",
            // "release" is an event the gateway doesn't currently
            // handle; should ack 202 silently.
            "release",
            body,
            Some(&sig),
            Arc::new(NoOpDispatcher),
        )
        .await;
        assert_eq!(status, StatusCode::ACCEPTED);
    }
}

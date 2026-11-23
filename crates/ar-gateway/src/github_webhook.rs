//! Signed GitHub App webhook intake.

use crate::dedup::CheckResult;
use crate::github_review::{schedule, PullRequestEvent};
use crate::{AppState, GitHubDeps};
use alloc::sync::Arc;
use ar_chat::{parse_chat_command, ChatCommand, ChatContext, ChatHandler};
use ar_github::{
    verify_webhook_signature, InstallationRef, IssueCommentEvent, PullRequestReviewCommentEvent,
    WebhookRepository, WebhookSignatureError, WebhookUser,
};
use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse as _, Response};

/// Header carrying the HMAC signature.
const SIGNATURE_HEADER: &str = "x-hub-signature-256";
/// Header identifying the webhook event type.
const EVENT_HEADER: &str = "x-github-event";
/// Header carrying the delivery identifier used for deduplication.
const DELIVERY_HEADER: &str = "x-github-delivery";
/// Cap the input to the PR JSON decoder independently of Axum's body limit.
const MAX_PR_PAYLOAD_BYTES: usize = 1 << 20;

/// Fields shared by issue comments and pull request review comments.
struct MentionEvent<'event> {
    /// Event action, such as `created`.
    action: &'event str,
    /// Comment text.
    body: &'event str,
    /// GitHub App installation supplied with the event.
    installation: Option<InstallationRef>,
    /// Pull request number within the repository.
    pr_number: u64,
    /// Repository containing the pull request.
    repository: &'event WebhookRepository,
    /// Account that sent the event.
    sender: &'event WebhookUser,
}

/// `POST /webhooks/github`.
///
/// 1. Applies the shared webhook rate limit.
/// 2. Verifies `X-Hub-Signature-256` against the GitHub App webhook secret
///    before parsing anything.
/// 3. Drops redelivered `X-GitHub-Delivery` ids.
/// 4. Routes `issue_comment` and `pull_request_review_comment` mentions to
///    the chat handler and schedules eligible pull requests for review once
///    their GitHub checks allow it.
#[inline]
pub async fn handle(State(state): State<AppState>, headers: HeaderMap, body: Bytes) -> Response {
    let Some(github) = state.github.clone() else {
        return reject(StatusCode::NOT_FOUND, "github webhooks not configured");
    };

    if let Some(bucket) = state.webhook_rate_limit.as_ref() {
        if !bucket.try_take() {
            state.metrics.record_rate_limited();
            return reject(StatusCode::TOO_MANY_REQUESTS, "rate limit");
        }
    }

    let Some(signature) = headers
        .get(SIGNATURE_HEADER)
        .and_then(|value| value.to_str().ok())
    else {
        state.metrics.record_signature_failure();
        return reject(StatusCode::UNAUTHORIZED, "missing signature");
    };
    if let Err(error) = verify_webhook_signature(&github.webhook_secret, &body, signature) {
        state.metrics.record_signature_failure();
        let status = match error {
            WebhookSignatureError::Mismatch => StatusCode::UNAUTHORIZED,
            WebhookSignatureError::InvalidFormat | WebhookSignatureError::InvalidHex => {
                StatusCode::BAD_REQUEST
            }
            WebhookSignatureError::InvalidSecret => StatusCode::INTERNAL_SERVER_ERROR,
        };
        return reject(status, &format!("signature: {error}"));
    }

    let event = headers
        .get(EVENT_HEADER)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("");
    state.metrics.record_event(event);

    if is_duplicate(&state, &headers).await {
        return (StatusCode::OK, "duplicate").into_response();
    }

    match event {
        "ping" => (StatusCode::OK, "pong").into_response(),
        "pull_request" => handle_pull_request(&state, github, &body).await,
        "issue_comment" => {
            let evt: IssueCommentEvent = match serde_json::from_slice(&body) {
                Ok(evt) => evt,
                Err(error) => return payload_failure(&state, &error),
            };
            if !evt.is_pull_request_comment() {
                return accepted();
            }
            handle_mention(
                &state,
                &github,
                MentionEvent {
                    action: &evt.action,
                    body: evt.comment.body(),
                    sender: &evt.sender,
                    repository: &evt.repository,
                    installation: evt.installation,
                    pr_number: evt.issue.number,
                },
            )
            .await
        }
        "pull_request_review_comment" => {
            let evt: PullRequestReviewCommentEvent = match serde_json::from_slice(&body) {
                Ok(evt) => evt,
                Err(error) => return payload_failure(&state, &error),
            };
            handle_mention(
                &state,
                &github,
                MentionEvent {
                    action: &evt.action,
                    body: evt.comment.body(),
                    sender: &evt.sender,
                    repository: &evt.repository,
                    installation: evt.installation,
                    pr_number: evt.pull_request.number,
                },
            )
            .await
        }
        other => {
            tracing::debug!(event = other, "ignoring github webhook event");
            accepted()
        }
    }
}

/// Recognize redeliveries without colliding with Forgejo delivery IDs.
async fn is_duplicate(state: &AppState, headers: &HeaderMap) -> bool {
    let Some(dedup) = state.webhook_dedup.as_ref() else {
        return false;
    };
    let Some(delivery_id) = headers
        .get(DELIVERY_HEADER)
        .and_then(|value| value.to_str().ok())
    else {
        return false;
    };
    match dedup
        .check_and_record(&format!("github:{delivery_id}"))
        .await
    {
        Ok(CheckResult::Duplicate) => {
            state.metrics.record_duplicate();
            true
        }
        Ok(CheckResult::FirstSight) => false,
        Err(error) => {
            // Fail open, matching the Forgejo route.
            tracing::warn!(%error, delivery_id, "github dedup check failed; handling anyway");
            false
        }
    }
}

/// Bound resource use before decoding the fixed-shape PR DTO. This function
/// is modeled by the repository's `CodeQL` extension `github-pr-payload`.
fn bounded_pr_payload(body: &[u8]) -> Option<&[u8]> {
    (body.len() <= MAX_PR_PAYLOAD_BYTES).then_some(body)
}

/// Parse the signed PR delivery and schedule its readiness watch.
async fn handle_pull_request(state: &AppState, github: GitHubDeps, body: &[u8]) -> Response {
    let Some(bounded) = bounded_pr_payload(body) else {
        return reject(
            StatusCode::PAYLOAD_TOO_LARGE,
            "pull request payload too large",
        );
    };
    // The signed delivery is bounded above. The DTO has a fixed shape and
    // ignores additional GitHub fields without allocating a JSON value tree.
    let evt: PullRequestEvent = match serde_json::from_slice(bounded) {
        Ok(evt) => evt,
        Err(error) => return payload_failure(state, &error),
    };
    schedule(state, github, evt).await;
    accepted()
}

/// Filters comment events and hands recognized mentions off to the chat handler.
async fn handle_mention(state: &AppState, github: &GitHubDeps, evt: MentionEvent<'_>) -> Response {
    // Edited and deleted comments must not re-fire commands.
    if evt.action != "created" {
        return accepted();
    }
    // Never act on bot accounts (including this app) to avoid reply loops.
    if evt.sender.is_bot() || evt.sender.login.eq_ignore_ascii_case(&github.bot_login) {
        return accepted();
    }
    let command = parse_chat_command(evt.body, &github.bot_name);
    if matches!(command, ChatCommand::NotMentioned) {
        return accepted();
    }
    state.metrics.record_chat_command();

    let owner = evt.repository.owner.login.clone();
    let repo = evt.repository.name.clone();
    if let Some(installation) = evt.installation {
        github
            .host
            .remember_installation(&owner, &repo, installation.id)
            .await;
    }

    let Some(chat) = github.chat.clone() else {
        state.metrics.record_chat_unconfigured();
        tracing::warn!(
            repo = %evt.repository.full_name,
            pr = evt.pr_number,
            "github chat command received but chat is not configured; ignoring"
        );
        return accepted();
    };

    let issue_number = evt.pr_number;
    let commenter_login = evt.sender.login.clone();
    let host = Arc::clone(&github.host);
    let dispatcher = Arc::clone(&github.dispatcher);
    let bot_login = Arc::clone(&github.bot_login);
    tokio::spawn(async move {
        let handler = ChatHandler {
            host: host.as_ref(),
            llm: &chat.llm,
            learnings: chat.learnings.as_ref(),
            dispatcher: Some(dispatcher),
        };
        let ctx = ChatContext {
            owner: &owner,
            repo: &repo,
            issue_number,
            commenter_login: &commenter_login,
            bot_login: &bot_login,
            reply_to_comment_id: None,
        };
        if let Err(error) = handler.handle(ctx, command).await {
            tracing::error!(
                %owner, %repo, pr = issue_number, %error,
                "github chat handler failed"
            );
        }
    });
    accepted()
}

/// Records a payload decode failure and returns its HTTP response.
fn payload_failure(state: &AppState, error: &serde_json::Error) -> Response {
    state.metrics.record_payload_failure();
    reject(StatusCode::BAD_REQUEST, &format!("payload decode: {error}"))
}

/// Acknowledges an event with an empty response body.
fn accepted() -> Response {
    (StatusCode::ACCEPTED, "").into_response()
}

/// Logs a rejected request and returns its status and message.
fn reject(status: StatusCode, msg: &str) -> Response {
    tracing::warn!(%status, msg, "rejecting github webhook");
    (status, msg.to_owned()).into_response()
}

#[cfg(test)]
#[expect(
    clippy::unwrap_used,
    reason = "Invalid fixtures or failed test requests should fail the test immediately"
)]
mod tests {
    use crate::{build_router, AppState, GitHubDeps};
    use alloc::sync::Arc;
    use ar_orchestrator::{JobDispatcher, NoOpDispatcher, ReviewJob};
    use async_trait::async_trait;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use core::sync::atomic::{AtomicUsize, Ordering};
    use core::time::Duration;
    use std::sync::Mutex;
    use tokio::time::{sleep, timeout};
    use tower::ServiceExt as _;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, Request as MockRequest, ResponseTemplate};

    const APP_ID: u64 = 12_345;
    const COMMENT_ID: u64 = 1;
    const INSTALLATION_ID: u64 = 42;
    const PR_NUMBER: u64 = 42;

    struct RecordingDispatcher {
        seen: Mutex<Vec<ReviewJob>>,
    }

    impl RecordingDispatcher {
        fn jobs(&self) -> Vec<ReviewJob> {
            self.seen.lock().unwrap().clone()
        }

        fn new() -> Arc<Self> {
            Arc::new(Self {
                seen: Mutex::new(Vec::new()),
            })
        }
    }

    #[async_trait]
    impl JobDispatcher for RecordingDispatcher {
        async fn dispatch(&self, job: ReviewJob) {
            self.seen.lock().unwrap().push(job);
        }
    }

    #[expect(
        clippy::arithmetic_side_effects,
        clippy::expect_used,
        clippy::string_slice,
        reason = "Extract the fixed ASCII PEM fixture using its known markers"
    )]
    fn test_private_key() -> &'static str {
        let source = include_str!("../../ar-github/tests/app_jwt.rs");
        let start = source
            .find("-----BEGIN PRIVATE KEY-----")
            .expect("key start");
        let end = source.find("-----END PRIVATE KEY-----").expect("key end")
            + "-----END PRIVATE KEY-----".len();
        &source[start..end]
    }

    #[expect(
        clippy::expect_used,
        reason = "The fixed test key and mock URL must construct a valid client"
    )]
    fn app_host(base: &str) -> Arc<ar_github::AppReviewHost> {
        let signer = ar_github::GitHubAppJwt::from_rsa_pem(12345, test_private_key().as_bytes())
            .expect("signer");
        let client = ar_github::Client::with_app_signer(base, signer).expect("client");
        Arc::new(ar_github::AppReviewHost::new(client))
    }

    fn github_deps(base: &str, dispatcher: Arc<dyn JobDispatcher>) -> GitHubDeps {
        GitHubDeps::new("github-secret", app_host(base), dispatcher)
            .with_bot_identity("auto-review[bot]", "auto-review")
    }

    async fn mount_installation_token(github: &MockServer, installation_id: u64) {
        Mock::given(method("POST"))
            .and(path(format!(
                "/app/installations/{installation_id}/access_tokens"
            )))
            .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({
                "token": "ghs_installation",
                "expires_at": "2099-01-01T00:00:00Z"
            })))
            .mount(github)
            .await;
    }

    async fn mount_pull_request(github: &MockServer, head_sha: &str) {
        Mock::given(method("GET"))
            .and(path("/repos/o/r/pulls/42"))
            .and(header("authorization", "Bearer ghs_installation"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "number": PR_NUMBER,
                "title": "feat: github review",
                "body": null,
                "draft": false,
                "state": "open",
                "head": {"ref": "topic", "sha": head_sha},
                "base": {"ref": "main", "sha": "cafef00d"}
            })))
            .mount(github)
            .await;
    }

    #[tokio::test]
    async fn ci_review_endpoint_acknowledges_legacy_github_request_without_dispatch() {
        let github = MockServer::start().await;
        mount_installation_token(&github, 42).await;
        mount_pull_request(&github, "deadbeef").await;

        let forgejo_dispatcher = RecordingDispatcher::new();
        let github_dispatcher = RecordingDispatcher::new();
        let app = build_router(
            AppState::new(
                "forgejo-secret",
                Arc::<RecordingDispatcher>::clone(&forgejo_dispatcher),
            )
            .with_ci_review_token("action-token")
            .with_github(github_deps(
                &github.uri(),
                Arc::<RecordingDispatcher>::clone(&github_dispatcher),
            )),
        );

        let req = Request::post("/reviews/ci")
            .header("authorization", "Bearer action-token")
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::to_vec(&serde_json::json!({
                    "provider": "github",
                    "installation_id": INSTALLATION_ID,
                    "owner": "o",
                    "repo": "r",
                    "pr_number": PR_NUMBER,
                    "head_sha": "deadbeef"
                }))
                .unwrap(),
            ))
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();

        assert_eq!(resp.status(), StatusCode::ACCEPTED);
        assert!(forgejo_dispatcher.jobs().is_empty());
        assert!(github_dispatcher.jobs().is_empty());
        assert!(github.received_requests().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn github_pr_webhook_rejects_oversized_signed_payload() {
        let github = MockServer::start().await;
        let deps = github_deps(&github.uri(), Arc::new(NoOpDispatcher));
        let mut payload = serde_json::to_vec(&serde_json::json!({
            "action": "opened",
            "number": PR_NUMBER,
            "pull_request": {"draft": false, "head": {"sha": "deadbeef"}},
            "repository": {"name": "r", "full_name": "o/r", "owner": {"login": "o", "type": "User"}},
            "installation": {"id": INSTALLATION_ID}
        })).unwrap();
        // Still valid JSON, and below Axum's default 2 MiB limit.
        payload.resize((1 << 20) + 1, b' ');
        let signature = sign(&deps.webhook_secret, &payload);
        let app = build_router(AppState::new("unused", Arc::new(NoOpDispatcher)).with_github(deps));
        let resp = app
            .oneshot(github_request("pull_request", &payload, Some(&signature)))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::PAYLOAD_TOO_LARGE);
        assert!(github.received_requests().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn github_pr_webhook_reviews_when_no_checks_exist() {
        github_pr_webhook_waits_for_checks_case(false).await;
    }

    #[tokio::test]
    async fn github_pr_webhook_waits_for_pending_and_failing_merge_checks() {
        github_pr_webhook_waits_for_checks_case(true).await;
    }

    #[expect(
        clippy::too_many_lines,
        reason = "Keep the test's PR/check timeline together"
    )]
    async fn github_pr_webhook_waits_for_checks_case(with_merge_checks: bool) {
        let no_checks: usize = 0;
        let one_check: usize = 1;
        let github = MockServer::start().await;
        mount_installation_token(&github, INSTALLATION_ID).await;
        let check_round = Arc::new(AtomicUsize::new(0));
        if with_merge_checks {
            let merge_round = Arc::clone(&check_round);
            Mock::given(method("GET"))
                .and(path("/repos/o/r/pulls/42"))
                .respond_with(move |_: &MockRequest| {
                    // The base advances after the original merge check fails.
                    let merge_sha = if merge_round.load(Ordering::SeqCst) < 2 {
                        "abc123"
                    } else {
                        "abc456"
                    };
                    ResponseTemplate::new(200).set_body_json(serde_json::json!({
                        "number": PR_NUMBER, "title": "Review me", "body": null,
                        "draft": false, "state": "open", "merge_commit_sha": merge_sha,
                        "head": {"ref": "topic", "sha": "deadbeef"},
                        "base": {"ref": "main", "sha": "cafef00d"}
                    }))
                })
                .mount(&github)
                .await;
            let counter = Arc::clone(&check_round);
            Mock::given(method("GET"))
                .and(path("/repos/o/r/commits/abc123/check-runs"))
                .respond_with(move |_: &MockRequest| {
                    let (status, conclusion) = match counter.fetch_add(1, Ordering::SeqCst) {
                        0 => ("in_progress", None),
                        1 => ("completed", Some("failure")),
                        _ => ("completed", Some("success")),
                    };
                    ResponseTemplate::new(200).set_body_json(serde_json::json!({
                        "total_count": one_check, "check_runs": [{"status": status, "conclusion": conclusion}]
                    }))
                })
                .mount(&github)
                .await;
            Mock::given(method("GET"))
                .and(path("/repos/o/r/commits/abc123/statuses"))
                .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                    {"state": "failure", "context": "auto_review", "creator": {"login": "auto-review[bot]"}},
                    {"state": "success", "context": "ci", "creator": null},
                    {"state": "failure", "context": "CI", "creator": null}
                ])))
                .mount(&github)
                .await;
        } else {
            mount_pull_request(&github, "deadbeef").await;
        }
        for (route, response) in [
            (
                "/repos/o/r/commits/abc456/check-runs",
                serde_json::json!({"total_count": no_checks, "check_runs": []}),
            ),
            ("/repos/o/r/commits/abc456/statuses", serde_json::json!([])),
            (
                "/repos/o/r/commits/deadbeef/check-runs",
                serde_json::json!({"total_count": no_checks, "check_runs": []}),
            ),
            (
                "/repos/o/r/commits/deadbeef/statuses",
                serde_json::json!([]),
            ),
        ] {
            Mock::given(method("GET"))
                .and(path(route))
                .respond_with(ResponseTemplate::new(200).set_body_json(response))
                .mount(&github)
                .await;
        }
        let dispatcher = RecordingDispatcher::new();
        let mut key = [0; 32];
        getrandom::fill(&mut key).unwrap();
        let secret = hex::encode(key);
        let deps = GitHubDeps::new(
            secret.as_str(),
            app_host(&github.uri()),
            Arc::<RecordingDispatcher>::clone(&dispatcher),
        )
        .with_bot_identity("auto-review[bot]", "auto-review");
        let app = build_router(
            AppState::new("forgejo-secret", Arc::new(NoOpDispatcher)).with_github(deps),
        );
        let payload = serde_json::to_vec(&serde_json::json!({
            "action": "opened",
            "number": PR_NUMBER,
            "pull_request": {"draft": false, "head": {"sha": "deadbeef"}},
            "repository": {"name": "r", "full_name": "o/r", "owner": {"login": "o", "type": "User"}},
            "installation": {"id": INSTALLATION_ID}
        }))
        .unwrap();
        let signature = sign(&secret, &payload);
        let resp = app
            .oneshot(github_request("pull_request", &payload, Some(&signature)))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::ACCEPTED);
        assert!(
            timeout(Duration::from_secs(2), async {
                while dispatcher.jobs().is_empty() {
                    sleep(Duration::from_millis(10)).await;
                }
            })
            .await
            .is_ok(),
            "signed PR should dispatch once GitHub checks allow review"
        );
        let jobs = dispatcher.jobs();
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs.first().unwrap().head_sha, "deadbeef");
        assert_eq!(
            check_round.load(Ordering::SeqCst),
            if with_merge_checks { 3 } else { 0 }
        );
    }

    fn sign(secret: &str, body: &[u8]) -> String {
        use hmac::{Hmac, KeyInit as _, Mac as _};
        let mut mac = Hmac::<sha2::Sha256>::new_from_slice(secret.as_bytes()).unwrap();
        mac.update(body);
        format!("sha256={}", hex::encode(mac.finalize().into_bytes()))
    }

    fn github_request(event: &str, body: &[u8], signature: Option<&str>) -> Request<Body> {
        let mut req = Request::post("/webhooks/github")
            .header("x-github-event", event)
            .header("x-github-delivery", "delivery-1")
            .header("content-type", "application/json");
        if let Some(signature_value) = signature {
            req = req.header("x-hub-signature-256", signature_value);
        }
        req.body(Body::from(body.to_vec())).unwrap()
    }

    fn issue_comment_payload(body: &str, sender: &str, sender_type: &str) -> Vec<u8> {
        serde_json::to_vec(&serde_json::json!({
            "action": "created",
            "issue": {"number": 42, "pull_request": {"url": "https://api.github.com/repos/o/r/pulls/42"}},
            "comment": {"id": 555, "body": body, "user": {"login": sender, "type": sender_type}},
            "repository": {"name": "r", "full_name": "o/r", "owner": {"login": "o", "type": "User"}},
            "sender": {"login": sender, "type": sender_type},
            "installation": {"id": 42}
        }))
        .unwrap()
    }

    fn state_with_github(base: &str) -> AppState {
        AppState::new("forgejo-secret", Arc::new(NoOpDispatcher))
            .with_github(github_deps(base, Arc::new(NoOpDispatcher)))
    }

    #[tokio::test]
    async fn forgejo_webhook_is_not_found_in_github_only_gateway() {
        // GitHub-only deployments have no Forgejo webhook secret. An empty
        // HMAC key must never verify a forged Forgejo delivery.
        let app = build_router(
            AppState::new("", Arc::new(NoOpDispatcher))
                .with_github(github_deps("http://127.0.0.1:9", Arc::new(NoOpDispatcher))),
        );
        let body = br#"{"zen":"hi"}"#;
        let empty_key_signature = {
            use hmac::{Hmac, KeyInit as _, Mac as _};
            let mut mac = Hmac::<sha2::Sha256>::new_from_slice(b"").unwrap();
            mac.update(body);
            hex::encode(mac.finalize().into_bytes())
        };

        let resp = app
            .oneshot(
                Request::post("/webhooks/forgejo")
                    .header("x-forgejo-event", "ping")
                    .header("x-forgejo-signature", empty_key_signature)
                    .body(Body::from(body.to_vec()))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn github_webhook_is_not_found_when_github_is_not_configured() {
        let app = build_router(AppState::new("forgejo-secret", Arc::new(NoOpDispatcher)));
        let body = br#"{"zen":"hi"}"#;

        let resp = app
            .oneshot(github_request(
                "ping",
                body,
                Some(&sign("github-secret", body)),
            ))
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn github_webhook_rejects_missing_or_mismatched_signature() {
        let state = state_with_github("http://127.0.0.1:9");
        let metrics = Arc::clone(&state.metrics);
        let app = build_router(state);
        let body = br#"{"zen":"hi"}"#;

        let missing = app
            .clone()
            .oneshot(github_request("ping", body, None))
            .await
            .unwrap();
        // A Forgejo-secret signature must not verify on the GitHub route.
        let wrong_secret = app
            .oneshot(github_request(
                "ping",
                body,
                Some(&sign("forgejo-secret", body)),
            ))
            .await
            .unwrap();

        assert_eq!(missing.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(wrong_secret.status(), StatusCode::UNAUTHORIZED);
        assert!(metrics
            .render()
            .contains("auto_review_webhook_signature_failures_total 2\n"));
    }

    #[tokio::test]
    async fn github_webhook_answers_signed_ping() {
        let app = build_router(state_with_github("http://127.0.0.1:9"));
        let body = br#"{"zen":"hi"}"#;

        let resp = app
            .oneshot(github_request(
                "ping",
                body,
                Some(&sign("github-secret", body)),
            ))
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn github_webhook_ignores_bot_authored_mentions() {
        let state = state_with_github("http://127.0.0.1:9");
        let metrics = Arc::clone(&state.metrics);
        let app = build_router(state);
        let body = issue_comment_payload("@auto-review help", "other-bot[bot]", "Bot");

        let resp = app
            .oneshot(github_request(
                "issue_comment",
                &body,
                Some(&sign("github-secret", &body)),
            ))
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::ACCEPTED);
        assert!(metrics
            .render()
            .contains("auto_review_chat_commands_received_total 0\n"));
    }

    #[tokio::test]
    async fn github_webhook_handles_pr_comment_command_with_webhook_installation() {
        let github = MockServer::start().await;
        // No /repos/o/r/installation mock: the webhook's installation id
        // must be used directly.
        mount_installation_token(&github, 42).await;
        Mock::given(method("POST"))
            .and(path("/repos/o/r/issues/42/comments"))
            .and(header("authorization", "Bearer ghs_installation"))
            .respond_with(
                ResponseTemplate::new(201).set_body_json(serde_json::json!({"id": COMMENT_ID})),
            )
            .mount(&github)
            .await;
        let deps =
            github_deps(&github.uri(), Arc::new(NoOpDispatcher)).with_chat(crate::GitHubChatDeps {
                llm: Arc::new(ar_llm::Router::new()),
                learnings: Arc::new(ar_index::InMemoryLearningsStore::new()),
            });
        let state = AppState::new("forgejo-secret", Arc::new(NoOpDispatcher)).with_github(deps);
        let metrics = Arc::clone(&state.metrics);
        let app = build_router(state);
        let body = issue_comment_payload("@auto-review help", "alice", "User");

        let resp = app
            .oneshot(github_request(
                "issue_comment",
                &body,
                Some(&sign("github-secret", &body)),
            ))
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::ACCEPTED);
        assert!(metrics
            .render()
            .contains("auto_review_chat_commands_received_total 1\n"));
        let posted = wait_for_request(&github, "/repos/o/r/issues/42/comments").await;
        assert!(
            posted,
            "help reply should be posted as a PR conversation comment"
        );
    }

    #[tokio::test]
    async fn readyz_probes_github_app_authentication() {
        let github = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/app"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "id": APP_ID,
                "slug": "auto-review"
            })))
            .mount(&github)
            .await;
        let probe = Arc::new(crate::ReadinessProbe::github_with_ttl(
            app_host(&github.uri()),
            Duration::from_secs(60),
        ));
        let app = build_router(AppState::new("", Arc::new(NoOpDispatcher)).with_readiness(probe));

        let resp = app
            .oneshot(Request::get("/readyz").body(Body::empty()).unwrap())
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        let body = http_body_util::BodyExt::collect(resp.into_body())
            .await
            .unwrap()
            .to_bytes();
        assert!(String::from_utf8_lossy(&body).contains("github reachable"));
    }

    async fn wait_for_request(server: &MockServer, request_path: &str) -> bool {
        let first_attempt: i32 = 0;
        let attempt_limit: i32 = 100;
        for _ in first_attempt..attempt_limit {
            let seen = server
                .received_requests()
                .await
                .unwrap_or_default()
                .iter()
                .any(|req| req.url.path() == request_path);
            if seen {
                return true;
            }
            sleep(Duration::from_millis(20)).await;
        }
        false
    }
}

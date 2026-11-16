//! GitHub App webhook intake and GitHub CI-review routing.

use crate::{AppState, GitHubDeps};
use ar_chat::{parse_chat_command, ChatCommand, ChatContext, ChatHandler};
use ar_github::{
    verify_webhook_signature, InstallationRef, IssueCommentEvent, PullRequestReviewCommentEvent,
    WebhookRepository, WebhookSignatureError, WebhookUser,
};
use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};

const SIGNATURE_HEADER: &str = "x-hub-signature-256";
const EVENT_HEADER: &str = "x-github-event";
const DELIVERY_HEADER: &str = "x-github-delivery";

/// `POST /webhooks/github`.
///
/// 1. Applies the shared webhook rate limit.
/// 2. Verifies `X-Hub-Signature-256` against the GitHub App webhook secret
///    before parsing anything.
/// 3. Drops redelivered `X-GitHub-Delivery` ids.
/// 4. Routes `issue_comment` and `pull_request_review_comment` mentions to
///    the chat handler. Pull request events are intake-only: semantic review
///    is dispatched from CI through `/reviews/ci` (ADR-0012).
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

    if let Some(dedup) = state.webhook_dedup.as_ref() {
        if let Some(delivery_id) = headers
            .get(DELIVERY_HEADER)
            .and_then(|value| value.to_str().ok())
        {
            // Namespace GitHub ids so they can never collide with Forgejo
            // delivery UUIDs in a shared dedup store.
            let key = format!("github:{delivery_id}");
            match dedup.check_and_record(&key).await {
                Ok(crate::dedup::CheckResult::Duplicate) => {
                    state.metrics.record_duplicate();
                    return (StatusCode::OK, "duplicate").into_response();
                }
                Ok(crate::dedup::CheckResult::FirstSight) => {}
                Err(error) => {
                    // Fail open, matching the Forgejo route.
                    tracing::warn!(
                        %error,
                        delivery_id,
                        "github dedup check failed; handling anyway"
                    );
                }
            }
        }
    }

    match event {
        "ping" => (StatusCode::OK, "pong").into_response(),
        "issue_comment" => {
            let evt: IssueCommentEvent = match serde_json::from_slice(&body) {
                Ok(evt) => evt,
                Err(error) => return payload_failure(&state, error),
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
                Err(error) => return payload_failure(&state, error),
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

struct MentionEvent<'a> {
    action: &'a str,
    body: &'a str,
    sender: &'a WebhookUser,
    repository: &'a WebhookRepository,
    installation: Option<InstallationRef>,
    pr_number: u64,
}

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
    let host = github.host.clone();
    let dispatcher = github.dispatcher.clone();
    let bot_login = github.bot_login.clone();
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

fn payload_failure(state: &AppState, error: serde_json::Error) -> Response {
    state.metrics.record_payload_failure();
    reject(StatusCode::BAD_REQUEST, &format!("payload decode: {error}"))
}

fn accepted() -> Response {
    (StatusCode::ACCEPTED, "").into_response()
}

fn reject(status: StatusCode, msg: &str) -> Response {
    tracing::warn!(%status, msg, "rejecting github webhook");
    (status, msg.to_string()).into_response()
}

#[cfg(test)]
mod tests {
    use crate::{build_router, AppState, GitHubDeps};
    use ar_orchestrator::{JobDispatcher, NoOpDispatcher, ReviewJob};
    use async_trait::async_trait;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use std::sync::{Arc, Mutex};
    use tower::ServiceExt;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

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

    fn test_private_key() -> &'static str {
        let source = include_str!("../../ar-github/tests/app_jwt.rs");
        let start = source
            .find("-----BEGIN PRIVATE KEY-----")
            .expect("key start");
        let end = source.find("-----END PRIVATE KEY-----").expect("key end")
            + "-----END PRIVATE KEY-----".len();
        &source[start..end]
    }

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
                "number": 42,
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
    async fn ci_review_endpoint_routes_github_provider_to_github_host_and_dispatcher() {
        let github = MockServer::start().await;
        mount_installation_token(&github, 42).await;
        mount_pull_request(&github, "deadbeef").await;

        let forgejo_dispatcher = RecordingDispatcher::new();
        let github_dispatcher = RecordingDispatcher::new();
        let app = build_router(
            AppState::new(
                "forgejo-secret",
                forgejo_dispatcher.clone() as Arc<dyn JobDispatcher>,
            )
            .with_ci_review_token("action-token")
            .with_github(github_deps(
                &github.uri(),
                github_dispatcher.clone() as Arc<dyn JobDispatcher>,
            )),
        );

        let req = Request::post("/reviews/ci")
            .header("authorization", "Bearer action-token")
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::to_vec(&serde_json::json!({
                    "provider": "github",
                    "installation_id": 42,
                    "owner": "o",
                    "repo": "r",
                    "pr_number": 42,
                    "head_sha": "deadbeef"
                }))
                .unwrap(),
            ))
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();

        assert_eq!(resp.status(), StatusCode::ACCEPTED);
        assert!(forgejo_dispatcher.jobs().is_empty());
        let jobs = github_dispatcher.jobs();
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].owner, "o");
        assert_eq!(jobs[0].pr_title, "feat: github review");
        assert_eq!(jobs[0].pr_body, "");
        assert!(!jobs[0].force);
    }

    fn sign(secret: &str, body: &[u8]) -> String {
        use hmac::{Hmac, Mac};
        let mut mac = Hmac::<sha2::Sha256>::new_from_slice(secret.as_bytes()).unwrap();
        mac.update(body);
        format!("sha256={}", hex::encode(mac.finalize().into_bytes()))
    }

    fn github_request(event: &str, body: &[u8], signature: Option<&str>) -> Request<Body> {
        let mut req = Request::post("/webhooks/github")
            .header("x-github-event", event)
            .header("x-github-delivery", "delivery-1")
            .header("content-type", "application/json");
        if let Some(signature) = signature {
            req = req.header("x-hub-signature-256", signature);
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
            use hmac::{Hmac, Mac};
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
        let metrics = state.metrics.clone();
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
        let metrics = state.metrics.clone();
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
            .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({"id": 1})))
            .mount(&github)
            .await;
        let deps =
            github_deps(&github.uri(), Arc::new(NoOpDispatcher)).with_chat(crate::GitHubChatDeps {
                llm: Arc::new(ar_llm::Router::new()),
                learnings: Arc::new(ar_index::InMemoryLearningsStore::new()),
            });
        let state = AppState::new("forgejo-secret", Arc::new(NoOpDispatcher)).with_github(deps);
        let metrics = state.metrics.clone();
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
                "id": 12345,
                "slug": "auto-review"
            })))
            .mount(&github)
            .await;
        let probe = Arc::new(crate::ReadinessProbe::github_with_ttl(
            app_host(&github.uri()),
            std::time::Duration::from_secs(60),
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
        for _ in 0..100 {
            let seen = server
                .received_requests()
                .await
                .unwrap_or_default()
                .iter()
                .any(|req| req.url.path() == request_path);
            if seen {
                return true;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        false
    }
}

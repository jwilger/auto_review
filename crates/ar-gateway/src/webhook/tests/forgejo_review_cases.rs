//! Signed Forgejo webhook admission cases.

use super::*;
use crate::dedup::RecentDeliveries;
use ar_forgejo::Client;
use core::future::Future;
use core::sync::atomic::{AtomicBool, Ordering};
use core::time::Duration;
use serde_json::{json, Value};
use tokio::time::{advance, pause, resume, sleep, timeout};
use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, MockServer, Request as MockRequest, ResponseTemplate};

const POLICY: &str = "automatic_review:\n  forgejo_trigger: webhook\n";
const GATED: &str = "automatic_review:\n  forgejo_trigger: webhook\n  required_checks:\n    Test: pass\n    Coverage: complete\n";
const STATUSES: &str = "/api/v1/repos/o/r/statuses/deadbeef";
const RUNS: &str = "/api/v1/repos/o/r/actions/runs";
const JOBS: &str = "/api/v1/repos/o/r/actions/runs/56/jobs";
const PR: u64 = 7;
const RUN: u64 = 56;
const WAIT: u64 = 17;

struct Fixture {
    app: axum::Router,
    recorder: Arc<RecordingDispatcher>,
    server: MockServer,
    state: AppState,
}

impl Fixture {
    async fn actions_run(&self) {
        Mock::given(method("GET"))
            .and(path("/api/v1/version"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"version":"16.0.0"})))
            .mount(&self.server)
            .await;
        let count: usize = 1;
        Mock::given(method("GET")).and(path(RUNS)).and(query_param("head_sha", "deadbeef"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"total_count": count, "workflow_runs": [{
                "id": RUN, "commit_sha": "deadbeef", "event": "pull_request_sync",
                "trigger_event": "pull_request", "prettyref": "#7", "workflow_id": "ci.yaml",
                "event_payload": json!({"number": PR, "pull_request": {"head": {"sha": "deadbeef"}}}).to_string(),
            }]})))
            .mount(&self.server).await;
    }

    async fn event(&self, action: &str, draft: bool, delivery: &str) {
        assert_eq!(
            self.post_event(action, draft, delivery).await.status(),
            StatusCode::ACCEPTED
        );
    }

    async fn idle(&self) {
        let deps = self.state.forgejo.as_ref().unwrap();
        eventually(|| deps.review_candidates.is_empty()).await;
    }

    async fn jobs_api(&self, finish: Arc<AtomicBool>) {
        self.actions_run().await;
        Mock::given(method("GET"))
            .and(path(JOBS))
            .respond_with(move |_: &MockRequest| {
                let mut jobs = vec![
                    json!({"name": "Coverage", "status": "failure"}),
                    json!({"name": "test", "status": "success"}),
                ];
                if finish.load(Ordering::SeqCst) {
                    jobs.push(json!({"name": "Test", "status": "skipped"}));
                }
                ResponseTemplate::new(200).set_body_json(jobs)
            })
            .mount(&self.server)
            .await;
    }

    async fn new(policy: &str, live_sha: &'static str, draft: Arc<AtomicBool>) -> Self {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/repos/o/r/pulls/7"))
            .respond_with(move |_: &MockRequest| {
                ResponseTemplate::new(200).set_body_json(json!({
                    "number": PR, "title": "live title", "state": "open", "draft": draft.load(Ordering::SeqCst),
                    "head": {"ref": "topic", "sha": live_sha},
                    "base": {"ref": "main", "sha": "cafef00d"}
                }))
            })
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v1/repos/o/r/raw/.auto_review.yaml"))
            .and(query_param("ref", "main"))
            .respond_with(ResponseTemplate::new(200).set_body_string(policy))
            .mount(&server)
            .await;
        let recorder = RecordingDispatcher::new();
        let client = Arc::new(Client::new(&server.uri(), "forgejo-token").unwrap());
        let state = AppState::new("s", Arc::<RecordingDispatcher>::clone(&recorder))
            .with_forgejo(Arc::clone(&client))
            .with_webhook_dedup(Arc::new(RecentDeliveries::new(8)))
            .with_ci_review_endpoint("action-token", client);
        Self {
            app: build_router(state.clone()),
            recorder,
            server,
            state,
        }
    }

    async fn post_event(&self, action: &str, draft: bool, delivery: &str) -> Response {
        let mut payload: Value = serde_json::from_slice(&pr_payload(action, draft)).unwrap();
        if action == "edited" {
            payload["changes"] = json!({"title": {"from": "custom-prefixed title"}});
        }
        let body = serde_json::to_vec(&payload).unwrap();
        let request = Request::post("/webhooks/forgejo")
            .header(EVENT_HEADER, "pull_request")
            .header(DELIVERY_HEADER, delivery)
            .header(SIG_HEADER, sign("s", &body))
            .body(Body::from(body))
            .unwrap();
        self.app.clone().oneshot(request).await.unwrap()
    }

    async fn requests(&self) -> Vec<MockRequest> {
        self.server.received_requests().await.unwrap()
    }

    async fn waiting_status(&self) {
        let pending = Arc::new(AtomicBool::new(false));
        let posted = Arc::clone(&pending);
        Mock::given(method("GET"))
            .and(path(STATUSES))
            .and(query_param("page", "1"))
            .respond_with(move |_: &MockRequest| {
                let rows = if posted.load(Ordering::SeqCst) {
                    json!([{"id":WAIT,"context":"auto_review","status":"pending"}])
                } else {
                    json!([])
                };
                ResponseTemplate::new(200).set_body_json(rows)
            })
            .mount(&self.server)
            .await;
        Mock::given(method("POST"))
            .and(path(STATUSES))
            .respond_with(move |request: &MockRequest| {
                let body: Value = serde_json::from_slice(&request.body).unwrap();
                if body["state"] == "pending" {
                    pending.store(true, Ordering::SeqCst);
                }
                ResponseTemplate::new(201).set_body_json(json!({"id":WAIT}))
            })
            .mount(&self.server)
            .await;
    }
}

async fn eventually<F, P>(mut predicate: F)
where
    F: FnMut() -> P,
    P: Future<Output = bool>,
{
    let milestone = timeout(Duration::from_secs(2), async {
        while !predicate().await {
            sleep(Duration::from_millis(5)).await;
        }
    })
    .await;
    assert!(milestone.is_ok(), "expected Forgejo watch milestone");
}

#[tokio::test]
async fn invalid_named_gate_does_not_reenable_legacy_callback() {
    let fixture = Fixture::new(
        "automatic_review:\n  forgejo_trigger: webhook\n  required_checks:\n    Test: typo\n",
        "deadbeef",
        Arc::new(AtomicBool::new(false)),
    )
    .await;
    let response = fixture
        .app
        .clone()
        .oneshot(legacy_ci_request(PR))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    fixture.event("opened", false, "bad-policy").await;
    fixture.idle().await;
    assert!(
        fixture.recorder.jobs().is_empty(),
        "invalid gates must not restore legacy dispatch"
    );
}

#[tokio::test]
async fn signed_reopen_and_sync_each_start_review_on_their_own() {
    for action in ["reopened", "synchronized"] {
        let fixture = Fixture::new(POLICY, "deadbeef", Arc::default()).await;
        fixture.event(action, false, action).await;
        fixture.idle().await;
        let jobs = fixture.recorder.jobs();
        assert_eq!(jobs.len(), 1, "{action} should start its own review");
        assert_eq!(jobs[0].pr_title, "live title");
    }
}

#[tokio::test]
async fn duplicate_signed_delivery_does_not_start_a_second_review() {
    let fixture = Fixture::new(POLICY, "deadbeef", Arc::default()).await;
    fixture.event("opened", false, "same-delivery").await;
    fixture.idle().await;
    assert_eq!(fixture.recorder.jobs().len(), 1);

    // Replay after the first watch exits so watch coalescing cannot hide missing dedup.
    let response = fixture.post_event("opened", false, "same-delivery").await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = response.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(body, "duplicate");
    fixture.idle().await;
    assert_eq!(fixture.recorder.jobs().len(), 1);
}

#[tokio::test]
async fn draft_title_edit_rechecks_live_state_and_admits_same_head() {
    let draft = Arc::new(AtomicBool::new(true));
    let fixture = Fixture::new(POLICY, "deadbeef", Arc::clone(&draft)).await;
    fixture.event("opened", true, "draft").await;
    fixture.idle().await;
    assert!(fixture.recorder.jobs().is_empty());
    draft.store(false, Ordering::SeqCst);
    fixture.event("edited", false, "ready").await;
    fixture.idle().await;
    assert_eq!(fixture.recorder.jobs().len(), 1);
}

#[tokio::test]
async fn allows_live_drafts_only_with_opt_in_and_rejects_stale_heads() {
    let policy = format!("{POLICY}  allow_drafts: true\n");
    for (live_sha, expected) in [("deadbeef", 1), ("aaaa", 0)] {
        let fixture = Fixture::new(&policy, live_sha, Arc::new(AtomicBool::new(true))).await;
        fixture.event("synchronized", true, "sync").await;
        fixture.idle().await;
        assert_eq!(fixture.recorder.jobs().len(), expected);
    }
}

#[tokio::test]
async fn gates_require_exact_names_and_publish_pending_before_ci() {
    let fixture = Fixture::new(GATED, "deadbeef", Arc::default()).await;
    let finish = Arc::new(AtomicBool::new(false));
    fixture.waiting_status().await;
    fixture.jobs_api(Arc::clone(&finish)).await;
    fixture.event("opened", false, "open").await;
    eventually(|| async {
        fixture
            .requests()
            .await
            .iter()
            .any(|request| request.url.path() == JOBS)
    })
    .await;
    fixture.event("reopened", false, "reopen").await;
    // A ready result can trigger a second read during worker admission.
    // A third read proves the watcher kept polling the wrong-case result.
    eventually(|| async {
        fixture
            .requests()
            .await
            .iter()
            .filter(|request| request.url.path() == JOBS)
            .count()
            >= 3
    })
    .await;
    assert!(
        fixture.recorder.jobs().is_empty(),
        "lowercase Test must not satisfy the gate"
    );
    finish.store(true, Ordering::SeqCst);
    fixture.idle().await;
    assert_eq!(fixture.recorder.jobs().len(), 1);
    let requests = fixture.requests().await;
    let pending = requests
        .iter()
        .position(|request| request.method.as_str() == "POST" && request.url.path() == STATUSES)
        .unwrap();
    let ci = requests
        .iter()
        .position(|request| request.url.path() == RUNS)
        .unwrap();
    assert!(pending < ci);
}

#[tokio::test]
async fn unsupported_actions_gates_time_out_without_dispatch() {
    let fixture = Fixture::new(GATED, "deadbeef", Arc::default()).await;
    fixture.waiting_status().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/version"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"version":"15.0.0"})))
        .mount(&fixture.server)
        .await;
    fixture.event("opened", false, "open").await;
    eventually(|| async {
        fixture
            .requests()
            .await
            .iter()
            .any(|request| request.url.path() == "/api/v1/version")
    })
    .await;
    pause();
    advance(Duration::from_hours(2)).await;
    resume();
    fixture.idle().await;
    let requests = fixture.requests().await;
    assert!(requests.iter().all(|request| request.url.path() != RUNS));
    let terminal = requests
        .iter()
        .filter(|request| request.method.as_str() == "POST" && request.url.path() == STATUSES)
        .map(|request| serde_json::from_slice::<Value>(&request.body).unwrap())
        .find(|body| body["state"] == "error");
    assert!(terminal.is_some(), "timeout should retire owned pending");
    assert!(terminal.unwrap()["description"]
        .as_str()
        .unwrap()
        .contains("re-review"));
    assert!(fixture.recorder.jobs().is_empty());
}

#[tokio::test]
async fn unreadable_required_actions_jobs_time_out_without_dispatch() {
    let fixture = Fixture::new(GATED, "deadbeef", Arc::default()).await;
    fixture.waiting_status().await;
    fixture.actions_run().await;
    Mock::given(method("GET"))
        .and(path(JOBS))
        .respond_with(ResponseTemplate::new(200).set_body_raw("[", "application/json"))
        .mount(&fixture.server)
        .await;

    fixture.event("opened", false, "unreadable-jobs").await;
    eventually(|| async {
        fixture
            .requests()
            .await
            .iter()
            .filter(|request| request.url.path() == JOBS)
            .count()
            >= 2
    })
    .await;
    assert!(fixture.recorder.jobs().is_empty());

    pause();
    advance(Duration::from_hours(2)).await;
    resume();
    fixture.idle().await;

    let statuses: Vec<Value> = fixture
        .requests()
        .await
        .iter()
        .filter(|request| request.method.as_str() == "POST" && request.url.path() == STATUSES)
        .map(|request| serde_json::from_slice(&request.body).unwrap())
        .collect();
    assert_eq!(statuses.len(), 2);
    assert_eq!(statuses[0]["state"], "pending");
    assert_eq!(statuses[1]["state"], "error");
    let description = statuses[1]["description"].as_str().unwrap();
    assert!(description.contains("timed out") && description.contains("re-review"));
    assert!(fixture.recorder.jobs().is_empty());
}

//! End-to-end review of a GitHub pull request whose diff GitHub refuses.
//!
//! GitHub answers the whole-PR diff media type with `406 too_large` for a
//! pull request over 300 files or its line/byte limits (issue #260). This
//! drives `run_review_job` against a wiremock GitHub through the real
//! `ar-github` review host and a canned LLM: the review must still post its
//! findings and a final commit status instead of failing on the diff fetch.
//!
//! The wiremock server is not a git host, so the workspace clone fails and
//! the review takes the host fallback: `get_pr_diff` assembles the diff
//! from the per-file patches of `pulls/{n}/files`. The local merge-base
//! path is covered by the dispatcher's own tests with a local repository.

extern crate alloc;

use alloc::sync::Arc;
use ar_github::{Client, InstallationReviewHost};
use ar_llm::{
    types::{CompleteRequest, CompleteResponse, Error as LlmError, LlmProvider, ModelTier},
    Router as LlmRouter,
};
use ar_orchestrator::{run_review_job, InMemoryReviewHistory, ReviewJob};
use async_trait::async_trait;
use std::sync::Mutex;
use wiremock::http::Method;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

/// GitHub's 406 body for a pull request over the 300-file diff limit.
const TOO_MANY_FILES: &str = r#"{"message":"Sorry, the diff exceeded the maximum number of files (300). Consider using 'List pull requests files' API or locally cloning the repository instead.","errors":[{"resource":"PullRequest","field":"diff","code":"too_large"}],"documentation_url":"https://docs.github.com/rest/pulls/pulls#list-pull-requests-files","status":"406"}"#;

/// GitHub's 406 body for a pull request over the diff line limit.
const TOO_MANY_LINES: &str = r#"{"message":"Sorry, the diff exceeded the maximum number of lines (20000). Consider using 'List pull requests files' API or locally cloning the repository instead.","errors":[{"resource":"PullRequest","field":"diff","code":"too_large"}],"documentation_url":"https://docs.github.com/rest/pulls/pulls#list-pull-requests-files","status":"406"}"#;

/// Head commit of the pull request under review.
const HEAD_SHA: &str = "0123456789abcdef0123456789abcdef01234567";

/// Answers the review prompt with one finding and the verifier prompt by
/// keeping it, so a posted review carries an inline comment.
struct CannedProvider {
    /// Review responses still to hand out, popped last first.
    responses: Mutex<Vec<String>>,
}

#[async_trait]
impl LlmProvider for CannedProvider {
    #[expect(
        clippy::unwrap_used,
        reason = "the trait fixes the error type to LlmError; a poisoned lock means another test \
                  thread already panicked, so panicking here is the honest test failure"
    )]
    async fn complete(&self, _req: CompleteRequest) -> Result<CompleteResponse, LlmError> {
        let next = self
            .responses
            .lock()
            .unwrap()
            .pop()
            .unwrap_or_else(|| r#"{"summary":"","findings":[]}"#.to_owned());
        Ok(CompleteResponse {
            content: next,
            input_tokens: 0,
            output_tokens: 0,
        })
    }
}

/// Mount a GitHub that refuses the diff media type with `too_large_body`
/// and serves everything else a review needs.
#[expect(
    clippy::default_numeric_fallback,
    reason = "json! fixture numbers take their type from serde_json; a literal suffix would \
              trip separated_literal_suffix or unseparated_literal_suffix instead"
)]
async fn mount_github(server: &MockServer, too_large_body: &str) {
    Mock::given(method("GET"))
        .and(path("/repos/o/r/pulls/7"))
        .and(header("accept", "application/vnd.github.v3.diff"))
        .respond_with(ResponseTemplate::new(406).set_body_string(too_large_body))
        .with_priority(1)
        .mount(server)
        .await;
    Mock::given(method("GET"))
        .and(path("/repos/o/r/pulls/7"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "number": 7,
            "title": "feat: add a widget",
            "body": "Adds a widget.",
            "draft": false,
            "state": "open",
            "head": {"ref": "topic", "sha": HEAD_SHA},
            "base": {"ref": "main", "sha": "cafef00dcafef00dcafef00dcafef00dcafef00d"}
        })))
        .mount(server)
        .await;
    Mock::given(method("GET"))
        .and(path("/repos/o/r/pulls/7/files"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
            {
                "filename": "src/x.rs",
                "status": "modified",
                "additions": 1,
                "deletions": 1,
                "changes": 2,
                "patch": "@@ -1 +1 @@\n-old\n+new"
            },
            {
                "filename": "assets/logo.png",
                "status": "added",
                "additions": 0,
                "deletions": 0,
                "changes": 0
            }
        ])))
        .mount(server)
        .await;
    Mock::given(method("GET"))
        .and(path("/repos/o/r/issues/7/comments"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([])))
        .mount(server)
        .await;
    Mock::given(method("POST"))
        .and(path(format!("/repos/o/r/statuses/{HEAD_SHA}")))
        .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({})))
        .mount(server)
        .await;
    Mock::given(method("POST"))
        .and(path("/repos/o/r/pulls/7/reviews"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "id": 99,
            "state": "COMMENTED"
        })))
        .expect(1)
        .mount(server)
        .await;
}

/// Review the pull request against `server` and return what GitHub
/// received.
#[expect(
    clippy::expect_used,
    reason = "a failed client build should fail this test at that step"
)]
async fn review_against(server: &MockServer) -> Vec<Request> {
    let client = Client::new(&server.uri(), "app-jwt").expect("GitHub client");
    let host = InstallationReviewHost::new(client, "installation-token");
    let provider = Arc::new(CannedProvider {
        responses: Mutex::new(vec![
            r#"{"verdicts":[{"finding_index":0,"keep":true,"reasoning":"real"}]}"#.to_owned(),
            r#"{"summary":"one issue","findings":[{"path":"src/x.rs","line_start":1,"severity":"warning","message":"WIDGET-FINDING"}]}"#.to_owned(),
        ]),
    });
    let reasoning: Arc<CannedProvider> = Arc::clone(&provider);
    let llm = LlmRouter::new()
        .with(ModelTier::Reasoning, reasoning)
        .with(ModelTier::Cheap, provider);

    run_review_job(
        &host,
        &llm,
        "",
        "",
        &InMemoryReviewHistory::new(),
        None,
        None,
        None,
        ReviewJob {
            base_sha: None,
            force: false,
            head_sha: HEAD_SHA.to_owned(),
            owner: "o".to_owned(),
            pr_body: "Adds a widget.".to_owned(),
            pr_number: 7,
            pr_title: "feat: add a widget".to_owned(),
            repo: "r".to_owned(),
        },
    )
    .await;

    server.received_requests().await.unwrap_or_default()
}

/// Assert the review posted the canned finding and the final status is a
/// review outcome rather than the review-failed status.
#[expect(
    clippy::expect_used,
    reason = "a missing request or undecodable body should fail this test at that step"
)]
fn assert_reviewed(requests: &[Request]) {
    let review = requests
        .iter()
        .find(|request| {
            request.method == Method::POST && request.url.path() == "/repos/o/r/pulls/7/reviews"
        })
        .expect("a review was posted");
    let review_body = String::from_utf8_lossy(&review.body);
    assert!(review_body.contains("WIDGET-FINDING"), "{review_body}");

    let last_status = requests
        .iter()
        .rev()
        .find(|request| request.method == Method::POST && request.url.path().contains("/statuses/"))
        .expect("a commit status was posted");
    let status: serde_json::Value =
        serde_json::from_slice(&last_status.body).expect("status body is JSON");
    assert!(
        matches!(
            status.get("state").and_then(serde_json::Value::as_str),
            Some("success" | "failure")
        ),
        "the final status reports the review, not a failed one: {status}"
    );
}

#[tokio::test]
#[expect(
    clippy::tests_outside_test_module,
    reason = "files under tests/ are separate integration-test crates built only by cargo test; \
              there is no #[cfg(test)] module to place this in"
)]
async fn pull_request_over_the_file_limit_is_reviewed_end_to_end() {
    let server = MockServer::start().await;
    mount_github(&server, TOO_MANY_FILES).await;

    let requests = review_against(&server).await;

    assert_reviewed(&requests);
}

#[tokio::test]
#[expect(
    clippy::tests_outside_test_module,
    reason = "files under tests/ are separate integration-test crates built only by cargo test; \
              there is no #[cfg(test)] module to place this in"
)]
async fn pull_request_over_the_line_limit_is_reviewed_end_to_end() {
    let server = MockServer::start().await;
    mount_github(&server, TOO_MANY_LINES).await;

    let requests = review_against(&server).await;

    assert_reviewed(&requests);
}

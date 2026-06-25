use ar_chat::{parse_chat_command, ChatCommand, ChatContext, ChatHandler};
use ar_forgejo::Client as ForgejoClient;
use ar_index::{InMemoryLearningsStore, LearningSource, LearningsStore};
use ar_llm::{
    CompleteRequest, CompleteResponse, Error as LlmError, LlmProvider, ModelTier, ResponseFormat,
    Router,
};
use async_trait::async_trait;
use std::sync::Arc;
use wiremock::matchers::{body_partial_json, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

struct ConstantEmbedder;

/// Cheap-tier stub that answers the handler's two structured calls by schema
/// name: `directed_intent` (what does the user want?) and `override_reason`
/// (did they explain why?). Anything else returns a generic freeform reply.
struct ScriptedCheapProvider {
    intent: &'static str,
    has_explanation: bool,
    explanation: &'static str,
}

#[async_trait]
impl LlmProvider for ConstantEmbedder {
    async fn complete(&self, _: CompleteRequest) -> Result<CompleteResponse, LlmError> {
        unimplemented!()
    }

    async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, LlmError> {
        Ok(texts.iter().map(|_| vec![0.5, 0.5]).collect())
    }
}

#[async_trait]
impl LlmProvider for ScriptedCheapProvider {
    async fn complete(&self, req: CompleteRequest) -> Result<CompleteResponse, LlmError> {
        let schema_name = match &req.response_format {
            Some(ResponseFormat::JsonSchema { name, .. }) => name.as_str(),
            _ => "",
        };
        let content = match schema_name {
            "directed_intent" => format!(r#"{{"intent":"{}"}}"#, self.intent),
            "override_reason" => format!(
                r#"{{"has_explanation":{},"explanation":"{}"}}"#,
                self.has_explanation, self.explanation
            ),
            _ => "generic freeform reply".to_string(),
        };
        Ok(CompleteResponse {
            content,
            input_tokens: 0,
            output_tokens: 0,
        })
    }

    async fn embed(&self, _: &[String]) -> Result<Vec<Vec<f32>>, LlmError> {
        unimplemented!()
    }
}

fn quoted_metadata_failure_correction() -> &'static str {
    "\
@auto-review wrote in https://git.johnwilger.com/jwilger/auto_review/pulls/203#issuecomment-8125:\n\
> PR metadata quality: failed\n\
> The PR title and description are too vague.\n\
\n\
The title and body are adequate for this change; please accept them."
}

fn ctx_with(commenter: &'static str) -> ChatContext<'static> {
    ChatContext {
        owner: "alice",
        repo: "widgets",
        issue_number: 42,
        commenter_login: commenter,
        bot_login: "auto-review",
    }
}

fn ctx() -> ChatContext<'static> {
    ctx_with("carol")
}

/// Mount the standard open-PR GET. `title`/`body` let override tests assert the
/// marker is stamped onto them.
async fn mount_open_pr(server: &MockServer, title: &str, body: &str) {
    Mock::given(method("GET"))
        .and(path("/api/v1/repos/alice/widgets/pulls/42"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "number": 42,
            "state": "open",
            "title": title,
            "body": body,
            "draft": false,
            "head": {"ref": "topic", "sha": "deadbeef"},
            "base": {"ref": "main", "sha": "feedbeef"}
        })))
        .mount(server)
        .await;
}

/// Mount the bot's latest review as REQUEST_CHANGES (the "blocked" state) with
/// the given inline finding comments.
async fn mount_blocking_bot_review(server: &MockServer, comments: serde_json::Value) {
    Mock::given(method("GET"))
        .and(path("/api/v1/repos/alice/widgets/pulls/42/reviews"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
            {"id": 5, "state": "REQUEST_CHANGES", "user": {"login": "auto-review"}}
        ])))
        .mount(server)
        .await;
    Mock::given(method("GET"))
        .and(path(
            "/api/v1/repos/alice/widgets/pulls/42/reviews/5/comments",
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(comments))
        .mount(server)
        .await;
}

#[test]
fn forgejo_quote_of_metadata_failure_with_user_correction_routes_to_review_correction() {
    assert_eq!(
        parse_chat_command(quoted_metadata_failure_correction(), "auto-review"),
        ChatCommand::ReviewCorrection(
            "The title and body are adequate for this change; please accept them.".into()
        )
    );
}

#[tokio::test]
async fn directed_approval_with_nothing_outstanding_approves_and_stores_learning() {
    let server = MockServer::start().await;
    let forgejo = ForgejoClient::new(&server.uri(), "tok").expect("client");
    let learnings = InMemoryLearningsStore::new();
    let cheap = Arc::new(ScriptedCheapProvider {
        intent: "approval_request",
        has_explanation: true,
        explanation: "n/a",
    });
    let llm = Router::new()
        .with(ModelTier::Embedding, Arc::new(ConstantEmbedder))
        .with(ModelTier::Cheap, cheap);

    mount_open_pr(&server, "fix: a thing", "Because reasons.").await;
    // No blocking bot review -> nothing to override.
    Mock::given(method("GET"))
        .and(path("/api/v1/repos/alice/widgets/pulls/42/reviews"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([])))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/v1/repos/alice/widgets/pulls/42/reviews"))
        .and(body_partial_json(serde_json::json!({
            "commit_id": "deadbeef",
            "event": "APPROVED"
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"id": 99})))
        .mount(&server)
        .await;

    let handler = ChatHandler {
        host: &forgejo,
        llm: &llm,
        learnings: &learnings,
        dispatcher: None,
    };
    handler
        .handle(
            ctx(),
            ChatCommand::ReviewCorrection("Looks good, please approve.".into()),
        )
        .await
        .expect("ok");

    let stored = learnings.list().await.expect("list");
    assert_eq!(stored.len(), 1);
    assert_eq!(stored[0].source, LearningSource::Chat);

    let received = server.received_requests().await.expect("requests");
    let approvals = received
        .iter()
        .filter(|r| {
            r.method.as_str() == "POST"
                && r.url.path() == "/api/v1/repos/alice/widgets/pulls/42/reviews"
        })
        .count();
    assert_eq!(approvals, 1, "must approve when nothing is outstanding");
}

#[tokio::test]
async fn metadata_override_by_authorized_user_with_reason_approves_and_stamps_marker() {
    let server = MockServer::start().await;
    let forgejo = ForgejoClient::new(&server.uri(), "tok").expect("client");
    let learnings = InMemoryLearningsStore::new();
    let cheap = Arc::new(ScriptedCheapProvider {
        intent: "approval_request",
        has_explanation: true,
        explanation: "Release PRs use terse titles by convention here.",
    });
    let llm = Router::new()
        .with(ModelTier::Embedding, Arc::new(ConstantEmbedder))
        .with(ModelTier::Cheap, cheap);

    mount_open_pr(&server, "chore(release): v0.8.1", "chore(release): v0.8.1").await;
    // Metadata-only block: blocking review with no inline Error comments.
    mount_blocking_bot_review(&server, serde_json::json!([])).await;
    // carol is on the allow-list.
    Mock::given(method("GET"))
        .and(path("/api/v1/repos/alice/widgets/raw/.auto_review.yaml"))
        .respond_with(
            ResponseTemplate::new(200).set_body_string("override_approvers:\n  - carol\n"),
        )
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/v1/repos/alice/widgets/pulls/42/reviews"))
        .and(body_partial_json(
            serde_json::json!({ "event": "APPROVED" }),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"id": 199})))
        .mount(&server)
        .await;
    Mock::given(method("PATCH"))
        .and(path("/api/v1/repos/alice/widgets/pulls/42"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"number": 42})))
        .mount(&server)
        .await;

    let handler = ChatHandler {
        host: &forgejo,
        llm: &llm,
        learnings: &learnings,
        dispatcher: None,
    };
    handler
        .handle(
            ctx_with("carol"),
            ChatCommand::ReviewCorrection(
                "This is a release PR, the terse body is expected; please approve.".into(),
            ),
        )
        .await
        .expect("ok");

    let received = server.received_requests().await.expect("requests");
    let approvals = received
        .iter()
        .filter(|r| {
            r.method.as_str() == "POST"
                && r.url.path() == "/api/v1/repos/alice/widgets/pulls/42/reviews"
        })
        .count();
    assert_eq!(approvals, 1, "authorized override with reason must approve");

    let patch = received
        .iter()
        .find(|r| r.method.as_str() == "PATCH")
        .expect("must stamp the override marker via PATCH");
    let patch_body: serde_json::Value =
        serde_json::from_slice(&patch.body).expect("patch body json");
    let title = patch_body
        .get("title")
        .and_then(serde_json::Value::as_str)
        .expect("patched title");
    assert!(
        title.starts_with("[override-approved] "),
        "title must carry the override marker, got: {title}"
    );
    let body = patch_body
        .get("body")
        .and_then(serde_json::Value::as_str)
        .expect("patched body");
    assert!(
        body.contains("## Approval override") && body.contains("Reason:"),
        "body must carry the override section with the reason, got: {body}"
    );
}

#[tokio::test]
async fn override_by_unauthorized_user_declines_without_approving() {
    let server = MockServer::start().await;
    let forgejo = ForgejoClient::new(&server.uri(), "tok").expect("client");
    let learnings = InMemoryLearningsStore::new();
    let cheap = Arc::new(ScriptedCheapProvider {
        intent: "approval_request",
        has_explanation: true,
        explanation: "whatever",
    });
    let llm = Router::new()
        .with(ModelTier::Embedding, Arc::new(ConstantEmbedder))
        .with(ModelTier::Cheap, cheap);

    mount_open_pr(&server, "chore(release): v0.8.1", "chore(release): v0.8.1").await;
    mount_blocking_bot_review(&server, serde_json::json!([])).await;
    // No `.auto_review.yaml` (default 404) -> empty allow-list -> nobody may override.
    Mock::given(method("POST"))
        .and(path("/api/v1/repos/alice/widgets/issues/42/comments"))
        .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({"id": 7})))
        .mount(&server)
        .await;

    let handler = ChatHandler {
        host: &forgejo,
        llm: &llm,
        learnings: &learnings,
        dispatcher: None,
    };
    handler
        .handle(
            ctx_with("mallory"),
            ChatCommand::ReviewCorrection("Just approve it.".into()),
        )
        .await
        .expect("ok");

    let received = server.received_requests().await.expect("requests");
    let approvals = received
        .iter()
        .filter(|r| {
            r.method.as_str() == "POST"
                && r.url.path() == "/api/v1/repos/alice/widgets/pulls/42/reviews"
        })
        .count();
    assert_eq!(approvals, 0, "unauthorized user must not get an approval");
    let declines = received
        .iter()
        .filter(|r| {
            r.method.as_str() == "POST"
                && r.url.path() == "/api/v1/repos/alice/widgets/issues/42/comments"
        })
        .count();
    assert_eq!(declines, 1, "must post a decline comment");
}

#[tokio::test]
async fn authorized_override_without_a_reason_asks_for_why_and_does_not_approve() {
    let server = MockServer::start().await;
    let forgejo = ForgejoClient::new(&server.uri(), "tok").expect("client");
    let learnings = InMemoryLearningsStore::new();
    let cheap = Arc::new(ScriptedCheapProvider {
        intent: "approval_request",
        has_explanation: false,
        explanation: "",
    });
    let llm = Router::new()
        .with(ModelTier::Embedding, Arc::new(ConstantEmbedder))
        .with(ModelTier::Cheap, cheap);

    mount_open_pr(&server, "feat: thing", "does the thing").await;
    mount_blocking_bot_review(
        &server,
        serde_json::json!([
            {"id": 1, "body": "🔴 **Error:** possible panic on empty input", "user": {"login": "auto-review"}}
        ]),
    )
    .await;
    Mock::given(method("GET"))
        .and(path("/api/v1/repos/alice/widgets/raw/.auto_review.yaml"))
        .respond_with(
            ResponseTemplate::new(200).set_body_string("override_approvers:\n  - carol\n"),
        )
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/v1/repos/alice/widgets/issues/42/comments"))
        .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({"id": 8})))
        .mount(&server)
        .await;

    let handler = ChatHandler {
        host: &forgejo,
        llm: &llm,
        learnings: &learnings,
        dispatcher: None,
    };
    handler
        .handle(
            ctx_with("carol"),
            ChatCommand::ReviewCorrection("approve please".into()),
        )
        .await
        .expect("ok");

    let received = server.received_requests().await.expect("requests");
    let approvals = received
        .iter()
        .filter(|r| {
            r.method.as_str() == "POST"
                && r.url.path() == "/api/v1/repos/alice/widgets/pulls/42/reviews"
        })
        .count();
    assert_eq!(approvals, 0, "must not approve without a stated reason");
    let asks = received
        .iter()
        .filter(|r| {
            r.method.as_str() == "POST"
                && r.url.path() == "/api/v1/repos/alice/widgets/issues/42/comments"
        })
        .count();
    assert_eq!(asks, 1, "must ask the user for a reason");
}

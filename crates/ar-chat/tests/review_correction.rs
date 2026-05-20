use ar_chat::{parse_chat_command, ChatCommand, ChatContext, ChatHandler};
use ar_forgejo::Client as ForgejoClient;
use ar_index::{InMemoryLearningsStore, LearningSource, LearningsStore};
use ar_llm::{
    CompleteRequest, CompleteResponse, Error as LlmError, LlmProvider, ModelTier, Router,
};
use async_trait::async_trait;
use std::sync::{Arc, Mutex};
use wiremock::matchers::{body_partial_json, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

struct ConstantEmbedder;

struct RecordingCheapProvider {
    requests: Arc<Mutex<Vec<CompleteRequest>>>,
    reply: String,
}

struct FailingCheapProvider;

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
impl LlmProvider for RecordingCheapProvider {
    async fn complete(&self, req: CompleteRequest) -> Result<CompleteResponse, LlmError> {
        self.requests.lock().expect("lock").push(req);
        Ok(CompleteResponse {
            content: self.reply.clone(),
            input_tokens: 0,
            output_tokens: 0,
        })
    }

    async fn embed(&self, _: &[String]) -> Result<Vec<Vec<f32>>, LlmError> {
        unimplemented!()
    }
}

#[async_trait]
impl LlmProvider for FailingCheapProvider {
    async fn complete(&self, _: CompleteRequest) -> Result<CompleteResponse, LlmError> {
        Err(LlmError::Provider {
            status: 503,
            body: "cheap classification unavailable".into(),
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

fn quoted_review_finding_correction() -> &'static str {
    "\
@auto-review wrote in https://git.johnwilger.com/alice/widgets/pulls/42#issuecomment-8130:\n\
> The diff introduces a panic when the widget list is empty.\n\
> Consider returning an empty response instead.\n\
\n\
That finding is wrong; the code already handles an empty widget list correctly."
}

fn ctx() -> ChatContext<'static> {
    ChatContext {
        owner: "alice",
        repo: "widgets",
        issue_number: 42,
    }
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

#[test]
fn forgejo_quote_of_review_finding_with_user_correction_routes_to_review_correction() {
    assert_eq!(
        parse_chat_command(quoted_review_finding_correction(), "auto-review"),
        ChatCommand::ReviewCorrection(
            "That finding is wrong; the code already handles an empty widget list correctly."
                .into()
        )
    );
}

#[tokio::test]
async fn review_correction_stores_chat_learning_and_approves_current_head() {
    let server = MockServer::start().await;
    let forgejo = ForgejoClient::new(&server.uri(), "tok").expect("client");
    let learnings = InMemoryLearningsStore::new();
    let llm = Router::new().with(ModelTier::Embedding, Arc::new(ConstantEmbedder));

    Mock::given(method("GET"))
        .and(path("/api/v1/repos/alice/widgets/pulls/42"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "number": 42,
            "state": "open",
            "title": "Process deferred pi-lens formatting guardrail",
            "body": "Keeps the existing pi-lens deferred formatting guardrail explicit.",
            "draft": false,
            "head": {"ref": "topic", "sha": "deadbeef"},
            "base": {"ref": "main", "sha": "feedbeef"}
        })))
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
        forgejo: &forgejo,
        llm: &llm,
        learnings: &learnings,
        dispatcher: None,
    };

    handler
        .handle(
            ctx(),
            ChatCommand::ReviewCorrection(
                "The title and body are adequate for this change; please accept them.".into(),
            ),
        )
        .await
        .expect("ok");

    let stored = learnings.list().await.expect("list");
    assert_eq!(stored.len(), 1);
    assert_eq!(stored[0].source, LearningSource::Chat);
    assert!(stored[0].text.contains("title and body are adequate"));
    assert!(
        stored[0].text.contains("Repository alice/widgets only"),
        "learning must be scoped to the current repository: {}",
        stored[0].text
    );

    let received = server.received_requests().await.expect("requests");
    let approval_posts = received
        .iter()
        .filter(|request| {
            request.method.as_str() == "POST"
                && request.url.path() == "/api/v1/repos/alice/widgets/pulls/42/reviews"
        })
        .count();
    assert_eq!(approval_posts, 1, "must post exactly one approving review");
}

#[tokio::test]
async fn quoted_feedback_with_approval_request_uses_structured_cheap_classification_and_approval_path(
) {
    let server = MockServer::start().await;
    let forgejo = ForgejoClient::new(&server.uri(), "tok").expect("client");
    let learnings = InMemoryLearningsStore::new();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let cheap = Arc::new(RecordingCheapProvider {
        requests: Arc::clone(&requests),
        reply: "generic freeform reply".into(),
    });
    let llm = Router::new()
        .with(ModelTier::Embedding, Arc::new(ConstantEmbedder))
        .with(ModelTier::Cheap, cheap);

    Mock::given(method("GET"))
        .and(path("/api/v1/repos/alice/widgets/pulls/42"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "number": 42,
            "state": "open",
            "title": "Process deferred pi-lens formatting guardrail",
            "body": "Keeps the existing pi-lens deferred formatting guardrail explicit.",
            "draft": false,
            "head": {"ref": "topic", "sha": "deadbeef"},
            "base": {"ref": "main", "sha": "feedbeef"}
        })))
        .mount(&server)
        .await;

    Mock::given(method("POST"))
        .and(path("/api/v1/repos/alice/widgets/pulls/42/reviews"))
        .and(body_partial_json(serde_json::json!({
            "commit_id": "deadbeef",
            "event": "APPROVED"
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"id": 199})))
        .mount(&server)
        .await;

    Mock::given(method("POST"))
        .and(path("/api/v1/repos/alice/widgets/issues/42/comments"))
        .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({"id": 300})))
        .mount(&server)
        .await;

    let handler = ChatHandler {
        forgejo: &forgejo,
        llm: &llm,
        learnings: &learnings,
        dispatcher: None,
    };

    let body = "@auto-review wrote in https://git.johnwilger.com/jwilger/auto_review/pulls/203#issuecomment-8125:\n\
> PR metadata quality: failed\n\
> The PR title and description are too vague.\n\
\n\
OK, but since the pr metadata isn't actually a problem, I'd sure appreciate an approval. @auto-review";

    let command = parse_chat_command(body, "auto-review");
    handler.handle(ctx(), command).await.expect("ok");

    let captured_response_format = {
        let captured = requests.lock().expect("lock");
        assert_eq!(
            captured.len(),
            1,
            "must make one cheap-tier classification call"
        );
        captured[0].response_format.clone()
    };
    assert!(
        captured_response_format.is_some(),
        "must request structured classification output, not generic freeform text"
    );

    let received = server.received_requests().await.expect("requests");
    let approval_posts = received
        .iter()
        .filter(|request| {
            request.method.as_str() == "POST"
                && request.url.path() == "/api/v1/repos/alice/widgets/pulls/42/reviews"
        })
        .count();
    assert_eq!(approval_posts, 1, "must post exactly one approving review");
}

#[tokio::test]
async fn cheap_classification_failure_still_stores_learning_and_posts_approval() {
    let server = MockServer::start().await;
    let forgejo = ForgejoClient::new(&server.uri(), "tok").expect("client");
    let learnings = InMemoryLearningsStore::new();
    let llm = Router::new()
        .with(ModelTier::Embedding, Arc::new(ConstantEmbedder))
        .with(ModelTier::Cheap, Arc::new(FailingCheapProvider));

    Mock::given(method("GET"))
        .and(path("/api/v1/repos/alice/widgets/pulls/42"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "number": 42,
            "state": "open",
            "title": "Process deferred pi-lens formatting guardrail",
            "body": "Keeps the existing pi-lens deferred formatting guardrail explicit.",
            "draft": false,
            "head": {"ref": "topic", "sha": "deadbeef"},
            "base": {"ref": "main", "sha": "feedbeef"}
        })))
        .mount(&server)
        .await;

    Mock::given(method("POST"))
        .and(path("/api/v1/repos/alice/widgets/pulls/42/reviews"))
        .and(body_partial_json(serde_json::json!({
            "commit_id": "deadbeef",
            "event": "APPROVED"
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"id": 211})))
        .mount(&server)
        .await;

    let handler = ChatHandler {
        forgejo: &forgejo,
        llm: &llm,
        learnings: &learnings,
        dispatcher: None,
    };

    let body = "@auto-review wrote in https://git.johnwilger.com/jwilger/auto_review/pulls/203#issuecomment-8125:\n\
> PR metadata quality: failed\n\
> The PR title and description are too vague.\n\
\n\
Please accept this and approve. @auto-review";

    let command = parse_chat_command(body, "auto-review");
    handler.handle(ctx(), command).await.expect("ok");

    let stored = learnings.list().await.expect("list");
    assert_eq!(
        stored.len(),
        1,
        "must persist correction learning despite classification failure"
    );

    let received = server.received_requests().await.expect("requests");
    let approval_posts = received
        .iter()
        .filter(|request| {
            request.method.as_str() == "POST"
                && request.url.path() == "/api/v1/repos/alice/widgets/pulls/42/reviews"
        })
        .count();
    assert_eq!(
        approval_posts, 1,
        "must still post approval when classification is unavailable"
    );
}

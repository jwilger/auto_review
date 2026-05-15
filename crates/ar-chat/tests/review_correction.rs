use ar_chat::{parse_chat_command, ChatCommand, ChatContext, ChatHandler};
use ar_forgejo::Client as ForgejoClient;
use ar_index::{InMemoryLearningsStore, LearningSource, LearningsStore};
use ar_llm::{
    CompleteRequest, CompleteResponse, Error as LlmError, LlmProvider, ModelTier, Router,
};
use async_trait::async_trait;
use std::sync::Arc;
use wiremock::matchers::{body_partial_json, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

struct ConstantEmbedder;

#[async_trait]
impl LlmProvider for ConstantEmbedder {
    async fn complete(&self, _: CompleteRequest) -> Result<CompleteResponse, LlmError> {
        unimplemented!()
    }

    async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, LlmError> {
        Ok(texts.iter().map(|_| vec![0.5, 0.5]).collect())
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

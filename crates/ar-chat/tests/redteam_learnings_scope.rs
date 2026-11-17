//! Red-team: `@auto-review forget <id>` must not reach across
//! repositories (THREAT-MODEL T6).
//!
//! Learnings are scoped to the repository they were taught in. A user
//! commenting on a PR in repository B must not be able to delete a
//! learning that belongs to repository A, and the reply must not let
//! them tell another repository's id apart from an id that does not
//! exist.

use ar_chat::{ChatCommand, ChatContext, ChatHandler};
use ar_forgejo::Client as ForgejoClient;
use ar_index::{
    InMemoryLearningsStore, LearningRecord, LearningSource, LearningsFilter, LearningsStore,
    RepoScope,
};
use ar_llm::Router;
use wiremock::matchers::{body_partial_json, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn ctx_in(owner: &'static str, repo: &'static str) -> ChatContext<'static> {
    ChatContext {
        owner,
        repo,
        issue_number: 7,
        commenter_login: "mallory",
        bot_login: "auto-review",
    }
}

/// Expect exactly one comment on `owner/repo` PR 7 with `body`.
async fn expect_reply(server: &MockServer, owner: &str, repo: &str, body: &str) {
    Mock::given(method("POST"))
        .and(path(format!(
            "/api/v1/repos/{owner}/{repo}/issues/7/comments"
        )))
        .and(body_partial_json(serde_json::json!({ "body": body })))
        .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({"id": 1})))
        .expect(1)
        .mount(server)
        .await;
}

async fn forget(
    learnings: &InMemoryLearningsStore,
    server: &MockServer,
    ctx: ChatContext<'_>,
    id: u64,
) {
    let forgejo = ForgejoClient::new(&server.uri(), "tok").expect("client");
    let llm = Router::new();
    let handler = ChatHandler {
        host: &forgejo,
        llm: &llm,
        learnings,
        dispatcher: None,
    };
    handler
        .handle(ctx, ChatCommand::Forget(id))
        .await
        .expect("forget replies instead of erroring");
}

async fn repo_a_learning(learnings: &InMemoryLearningsStore) -> LearningRecord {
    learnings
        .add(
            "Repo A allows unwrap in build scripts.".into(),
            LearningSource::Chat,
            vec![1.0],
            0,
            &RepoScope::new("acme", "repo-a"),
            Some(3),
        )
        .await
        .expect("add")
}

#[tokio::test]
async fn forget_from_another_repository_leaves_the_learning_in_place() {
    let learnings = InMemoryLearningsStore::new();
    let learning = repo_a_learning(&learnings).await;
    let server = MockServer::start().await;
    let reply = format!("No learning with id {} in this repository", learning.id);
    expect_reply(&server, "acme", "repo-b", &reply).await;

    forget(&learnings, &server, ctx_in("acme", "repo-b"), learning.id).await;

    let all = learnings.list(LearningsFilter::All).await.expect("list");
    assert_eq!(all, vec![learning]);
}

#[tokio::test]
async fn forget_from_the_same_repository_removes_the_learning() {
    let learnings = InMemoryLearningsStore::new();
    let learning = repo_a_learning(&learnings).await;
    let server = MockServer::start().await;
    let reply = format!("Forgotten learning #{}.", learning.id);
    expect_reply(&server, "Acme", "Repo-A", &reply).await;

    forget(&learnings, &server, ctx_in("Acme", "Repo-A"), learning.id).await;

    assert!(learnings
        .list(LearningsFilter::All)
        .await
        .expect("list")
        .is_empty());
}

#[tokio::test]
async fn unscoped_legacy_learning_cannot_be_forgotten_from_chat() {
    let legacy = LearningRecord {
        id: 5,
        text: "Legacy guidance from before scoping.".into(),
        source: LearningSource::Chat,
        embedding: vec![1.0],
        created_at: 0,
        repo: None,
        origin_pr: None,
    };
    let learnings = InMemoryLearningsStore::from_records(vec![legacy.clone()]);
    let server = MockServer::start().await;
    expect_reply(
        &server,
        "acme",
        "repo-b",
        "No learning with id 5 in this repository",
    )
    .await;

    forget(&learnings, &server, ctx_in("acme", "repo-b"), 5).await;

    let all = learnings.list(LearningsFilter::All).await.expect("list");
    assert_eq!(all, vec![legacy]);
}

#[tokio::test]
async fn missing_id_gets_the_same_reply_as_another_repositorys_id() {
    let learnings = InMemoryLearningsStore::new();
    let server = MockServer::start().await;
    expect_reply(
        &server,
        "acme",
        "repo-b",
        "No learning with id 999 in this repository",
    )
    .await;

    forget(&learnings, &server, ctx_in("acme", "repo-b"), 999).await;
}

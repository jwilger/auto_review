//! Per-command chat handlers.
//!
//! [`ChatHandler`] dispatches a parsed [`ChatCommand`] to the right
//! action and posts a reply on the PR. Currently implemented:
//!
//! - `Help`: posts a list of supported commands.
//! - `Remember(text)`: embeds the text via the LLM router and adds
//!   it as a `Chat`-source learning. Replies confirming with the
//!   new id.
//! - `Forget(id)`: removes the learning. Replies confirming, or
//!   surfaces "not found" when the id is unknown.
//! - `ReReview`: posts an acknowledgement; the orchestrator
//!   integration that actually re-runs the review is a follow-up.
//! - `Freeform(text)`: posts a placeholder reply that we received
//!   the message; the chat-tier LLM call is a follow-up.
//! - `NotMentioned`: silently returns; the gateway shouldn't have
//!   called us in this case.

use crate::command::ChatCommand;
use ar_forgejo::Client as ForgejoClient;
use ar_index::{LearningSource, LearningsStore};
use ar_llm::{ModelTier, Router as LlmRouter};

#[derive(Debug, thiserror::Error)]
pub enum ChatError {
    #[error("forgejo: {0}")]
    Forgejo(#[from] ar_forgejo::Error),
    #[error("learnings store: {0}")]
    Learnings(#[from] ar_index::LearningsError),
    #[error("LLM error: {0}")]
    Llm(#[from] ar_llm::Error),
    #[error("system time: {0}")]
    Time(String),
}

const HELP_TEXT: &str = "\
**auto_review chat commands** (mention me with `@auto_review`):

- `remember <text>` — store project-specific guidance. I'll inject \
matching learnings into future review prompts.
- `forget <id>` — drop a previously-remembered learning by its id \
(printed on `remember`).
- `re-review` — re-run a full review on the current head SHA, \
ignoring my recorded review history.
- `help` — print this message.

Anything else after the mention is treated as a freeform question.";

/// Wire-up for the chat handler. Holds the dependencies it needs to
/// dispatch a [`ChatCommand`].
pub struct ChatHandler<'a> {
    pub forgejo: &'a ForgejoClient,
    pub llm: &'a LlmRouter,
    pub learnings: &'a (dyn LearningsStore + Sync),
}

/// Coordinates that locate a PR comment thread on Forgejo.
#[derive(Debug, Clone, Copy)]
pub struct ChatContext<'a> {
    pub owner: &'a str,
    pub repo: &'a str,
    pub issue_number: u64,
}

impl ChatHandler<'_> {
    pub async fn handle(
        &self,
        ctx: ChatContext<'_>,
        command: ChatCommand,
    ) -> Result<(), ChatError> {
        match command {
            ChatCommand::Help => {
                self.post(ctx, HELP_TEXT).await?;
            }
            ChatCommand::Remember(text) => {
                self.handle_remember(ctx, &text).await?;
            }
            ChatCommand::Forget(id) => {
                self.handle_forget(ctx, id).await?;
            }
            ChatCommand::ReReview => {
                self.post(
                    ctx,
                    "Acknowledged — re-review will be triggered on the next dispatch \
                     cycle. (Direct orchestrator dispatch from chat is a follow-up.)",
                )
                .await?;
            }
            ChatCommand::Freeform(_text) => {
                self.post(
                    ctx,
                    "I see your question. Conversational replies are a follow-up — \
                     for now I only act on the structured commands. Try \
                     `@auto_review help`.",
                )
                .await?;
            }
            ChatCommand::NotMentioned => {}
        }
        Ok(())
    }

    async fn handle_remember(&self, ctx: ChatContext<'_>, text: &str) -> Result<(), ChatError> {
        let embedding = self.embed(text).await?;
        let now = current_unix_seconds()?;
        let record = self
            .learnings
            .add(text.to_string(), LearningSource::Chat, embedding, now)
            .await?;
        let reply = format!(
            "Remembered as learning #{}. To revoke later: `@auto_review forget {}`.",
            record.id, record.id
        );
        self.post(ctx, &reply).await
    }

    async fn handle_forget(&self, ctx: ChatContext<'_>, id: u64) -> Result<(), ChatError> {
        let reply = match self.learnings.remove(id).await {
            Ok(()) => format!("Forgotten learning #{id}."),
            Err(ar_index::LearningsError::NotFound(_)) => {
                format!("No learning with id {id}; nothing to forget.")
            }
            Err(e) => return Err(ChatError::Learnings(e)),
        };
        self.post(ctx, &reply).await
    }

    async fn embed(&self, text: &str) -> Result<Vec<f32>, ChatError> {
        // Embedding tier may not be configured; in that case, store a
        // zero vector. The learning is still searchable by exact
        // content in `list()`, just not by similarity.
        if self.llm.provider(ModelTier::Embedding).is_err() {
            return Ok(Vec::new());
        }
        let mut vecs = self
            .llm
            .embed(ModelTier::Embedding, &[text.to_string()])
            .await?;
        Ok(vecs.pop().unwrap_or_default())
    }

    async fn post(&self, ctx: ChatContext<'_>, body: &str) -> Result<(), ChatError> {
        self.forgejo
            .post_issue_comment(ctx.owner, ctx.repo, ctx.issue_number, body)
            .await?;
        Ok(())
    }
}

fn current_unix_seconds() -> Result<i64, ChatError> {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .map_err(|e| ChatError::Time(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ar_index::InMemoryLearningsStore;
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

    fn ctx() -> ChatContext<'static> {
        ChatContext {
            owner: "alice",
            repo: "widgets",
            issue_number: 42,
        }
    }

    async fn setup() -> (MockServer, ForgejoClient, InMemoryLearningsStore, Router) {
        let server = MockServer::start().await;
        let forgejo = ForgejoClient::new(&server.uri(), "tok").expect("client");
        let learnings = InMemoryLearningsStore::new();
        let router = Router::new().with(ModelTier::Embedding, Arc::new(ConstantEmbedder));
        (server, forgejo, learnings, router)
    }

    #[tokio::test]
    async fn help_posts_help_text() {
        let (server, forgejo, learnings, llm) = setup().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/repos/alice/widgets/issues/42/comments"))
            .and(body_partial_json(serde_json::json!({})))
            .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({"id": 1})))
            .mount(&server)
            .await;
        let handler = ChatHandler {
            forgejo: &forgejo,
            llm: &llm,
            learnings: &learnings,
        };
        handler.handle(ctx(), ChatCommand::Help).await.expect("ok");
    }

    #[tokio::test]
    async fn remember_stores_learning_and_replies_with_id() {
        let (server, forgejo, learnings, llm) = setup().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/repos/alice/widgets/issues/42/comments"))
            .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({"id": 1})))
            .mount(&server)
            .await;
        let handler = ChatHandler {
            forgejo: &forgejo,
            llm: &llm,
            learnings: &learnings,
        };
        handler
            .handle(ctx(), ChatCommand::Remember("prefer Result".into()))
            .await
            .expect("ok");
        let stored = learnings.list().await.expect("list");
        assert_eq!(stored.len(), 1);
        assert_eq!(stored[0].text, "prefer Result");
        assert_eq!(stored[0].source, LearningSource::Chat);
        assert_eq!(stored[0].embedding, vec![0.5, 0.5]);
    }

    #[tokio::test]
    async fn forget_removes_learning_when_id_exists() {
        let (server, forgejo, learnings, llm) = setup().await;
        // Pre-populate a learning to forget.
        let added = learnings
            .add(
                "old guidance".into(),
                LearningSource::Chat,
                vec![1.0],
                1700000000,
            )
            .await
            .unwrap();

        Mock::given(method("POST"))
            .and(path("/api/v1/repos/alice/widgets/issues/42/comments"))
            .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({"id": 1})))
            .mount(&server)
            .await;
        let handler = ChatHandler {
            forgejo: &forgejo,
            llm: &llm,
            learnings: &learnings,
        };
        handler
            .handle(ctx(), ChatCommand::Forget(added.id))
            .await
            .expect("ok");
        assert!(learnings.list().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn forget_with_unknown_id_replies_not_found_without_error() {
        let (server, forgejo, learnings, llm) = setup().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/repos/alice/widgets/issues/42/comments"))
            .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({"id": 1})))
            .mount(&server)
            .await;
        let handler = ChatHandler {
            forgejo: &forgejo,
            llm: &llm,
            learnings: &learnings,
        };
        handler
            .handle(ctx(), ChatCommand::Forget(999))
            .await
            .expect("ok"); // not-found is reported in the comment, not as an error
    }

    #[tokio::test]
    async fn not_mentioned_command_does_nothing() {
        let (_server, forgejo, learnings, llm) = setup().await;
        // No mock mounted: any POST would fail. Verifies we don't try.
        let handler = ChatHandler {
            forgejo: &forgejo,
            llm: &llm,
            learnings: &learnings,
        };
        handler
            .handle(ctx(), ChatCommand::NotMentioned)
            .await
            .expect("ok");
    }

    #[tokio::test]
    async fn remember_uses_zero_vector_when_no_embedding_tier() {
        let server = MockServer::start().await;
        let forgejo = ForgejoClient::new(&server.uri(), "tok").expect("client");
        let learnings = InMemoryLearningsStore::new();
        let router = Router::new(); // no Embedding tier
        Mock::given(method("POST"))
            .and(path("/api/v1/repos/alice/widgets/issues/42/comments"))
            .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({"id": 1})))
            .mount(&server)
            .await;
        let handler = ChatHandler {
            forgejo: &forgejo,
            llm: &router,
            learnings: &learnings,
        };
        handler
            .handle(ctx(), ChatCommand::Remember("guidance".into()))
            .await
            .expect("ok");
        let stored = learnings.list().await.expect("list");
        assert_eq!(stored[0].embedding, Vec::<f32>::new());
    }
}

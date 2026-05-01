//! Background poller for inline review-thread mentions.
//!
//! Forgejo doesn't fire the `pull_request_review_comment` webhook
//! reliably for replies inside an inline review thread (gitea#26023).
//! The `issue_comment` webhook covers top-level PR comments but not
//! these threaded replies. So: a periodic background task that, for
//! every PR we've already reviewed, lists the PR's review comments
//! and dispatches any new `@auto_review` mentions through the chat
//! handler.
//!
//! Cursor: per-(repo, pr) highest-seen comment id. Forgejo issues
//! comment ids from a single sequence so id-based monotonicity is
//! reliable; comments with id ≤ cursor have been processed (or
//! existed before the bot started). Cursors are in-memory; a gateway
//! restart starts every PR's cursor at the highest current comment
//! id (so we don't reprocess history) — `seed_cursor` handles that.
//!
//! Bot's own comments are filtered out by login, so reading a comment
//! the bot itself posted doesn't trigger an infinite reply loop.

use crate::metrics::Metrics;
use ar_chat::command::parse_chat_command;
use ar_chat::{ChatContext, ChatError, ChatHandler};
use ar_forgejo::Client as ForgejoClient;
use ar_index::LearningsStore;
use ar_llm::Router as LlmRouter;
use ar_orchestrator::review_history::{PrKey, ReviewHistory};
use ar_orchestrator::JobDispatcher;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;

/// Default interval between polling runs. Operators can override
/// via `AR_POLL_INTERVAL_SECS`.
pub const DEFAULT_POLL_INTERVAL: Duration = Duration::from_secs(60);

/// Owns the per-PR comment-id cursor and the dependencies needed to
/// run one polling pass. Cheap to clone (everything is `Arc`).
#[derive(Clone)]
pub struct ChatPoller {
    forgejo: Arc<ForgejoClient>,
    llm: Arc<LlmRouter>,
    learnings: Arc<dyn LearningsStore>,
    history: Arc<dyn ReviewHistory>,
    dispatcher: Arc<dyn JobDispatcher>,
    bot_login: Arc<String>,
    bot_name: Arc<String>,
    cursors: Arc<Mutex<HashMap<PrKey, u64>>>,
    /// Optional. When wired, the poller increments cycle / failure /
    /// dispatch counters that the gateway exposes on `/metrics`.
    metrics: Option<Arc<Metrics>>,
}

#[allow(clippy::too_many_arguments)]
impl ChatPoller {
    pub fn new(
        forgejo: Arc<ForgejoClient>,
        llm: Arc<LlmRouter>,
        learnings: Arc<dyn LearningsStore>,
        history: Arc<dyn ReviewHistory>,
        dispatcher: Arc<dyn JobDispatcher>,
        bot_login: impl Into<String>,
        bot_name: impl Into<String>,
    ) -> Self {
        Self {
            forgejo,
            llm,
            learnings,
            history,
            dispatcher,
            bot_login: Arc::new(bot_login.into()),
            bot_name: Arc::new(bot_name.into()),
            cursors: Arc::new(Mutex::new(HashMap::new())),
            metrics: None,
        }
    }

    /// Wire in the shared Metrics handle so poll outcomes flow to
    /// `/metrics`. Without it, the poller still functions but its
    /// progress is invisible to Prometheus.
    pub fn with_metrics(mut self, metrics: Arc<Metrics>) -> Self {
        self.metrics = Some(metrics);
        self
    }

    /// Spawn the polling loop on the current tokio runtime. Runs
    /// forever; cancel the runtime to stop.
    pub fn spawn(self, interval: Duration) {
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(interval);
            // Skip the immediate fire — wait one full interval so
            // gateway startup isn't doing webhook + poll work
            // simultaneously.
            ticker.tick().await;
            loop {
                ticker.tick().await;
                if let Err(e) = self.poll_once().await {
                    tracing::warn!(error = %e, "poll_once errored; retrying next interval");
                }
            }
        });
    }

    /// Run one polling pass: list every reviewed PR, fetch its
    /// review comments, dispatch new `@<bot_name>` mentions through
    /// the chat handler.
    ///
    /// Errors propagate per PR — one failing PR doesn't abort the
    /// pass. The `Result` here is reserved for failures fetching the
    /// PR list itself.
    pub async fn poll_once(&self) -> Result<(), PollerError> {
        let known = match self.history.list_known().await {
            Ok(k) => k,
            Err(e) => {
                if let Some(m) = &self.metrics {
                    m.record_poll_history_failure();
                }
                return Err(PollerError::History(e.to_string()));
            }
        };
        for key in known {
            if let Err(e) = self.poll_pr(&key).await {
                if let Some(m) = &self.metrics {
                    m.record_poll_pr_failure();
                }
                tracing::warn!(
                    repo = format!("{}/{}", key.owner, key.repo),
                    pr = key.pr_number,
                    error = %e,
                    "poll_pr failed; continuing with next PR",
                );
            }
        }
        if let Some(m) = &self.metrics {
            m.record_poll_cycle();
        }
        Ok(())
    }

    async fn poll_pr(&self, key: &PrKey) -> Result<(), PollerError> {
        let comments = self
            .forgejo
            .list_pr_review_comments(&key.owner, &key.repo, key.pr_number)
            .await
            .map_err(|e| PollerError::Forgejo(e.to_string()))?;

        let cursor = self.cursors.lock().await.get(key).copied().unwrap_or(0);
        let mut max_seen = cursor;
        let mut to_dispatch: Vec<u64> = Vec::new();
        for c in &comments {
            if c.id > max_seen {
                max_seen = c.id;
            }
            if c.id <= cursor {
                continue; // already processed (or pre-existed when we started)
            }
            if c.user.login == *self.bot_login {
                continue; // never reply to ourselves
            }
            // Cheap pre-filter: only dispatch comments that even
            // mention us. The handler will parse properly.
            if c.body.contains(&format!("@{}", self.bot_name)) {
                to_dispatch.push(c.id);
            }
        }
        // Update cursor first; even if dispatch fails we don't want
        // to retry endlessly on the same comment.
        self.cursors.lock().await.insert(key.clone(), max_seen);

        for id in &to_dispatch {
            let body = comments
                .iter()
                .find(|c| c.id == *id)
                .map(|c| c.body.clone())
                .unwrap_or_default();
            let command = parse_chat_command(&body, &self.bot_name);
            let handler = ChatHandler {
                forgejo: &self.forgejo,
                llm: &self.llm,
                learnings: self.learnings.as_ref(),
                dispatcher: Some(self.dispatcher.clone()),
            };
            let ctx = ChatContext {
                owner: &key.owner,
                repo: &key.repo,
                issue_number: key.pr_number,
            };
            match handler.handle(ctx, command).await {
                Ok(()) => {
                    if let Some(m) = &self.metrics {
                        m.record_poll_mention_dispatched();
                    }
                }
                Err(e) => {
                    if let Some(m) = &self.metrics {
                        m.record_poll_chat_failure();
                    }
                    tracing::warn!(
                        repo = format!("{}/{}", key.owner, key.repo),
                        pr = key.pr_number,
                        comment = id,
                        error = %e,
                        "chat dispatch from poller failed",
                    );
                }
            }
        }
        Ok(())
    }

    /// For tests: peek at the cursor a PR is parked on.
    #[cfg(test)]
    pub(crate) async fn cursor_for(&self, key: &PrKey) -> Option<u64> {
        self.cursors.lock().await.get(key).copied()
    }

    /// For tests: bypass the spawn loop and run one pass synchronously.
    #[cfg(test)]
    pub(crate) async fn run_once_for_tests(&self) -> Result<(), PollerError> {
        self.poll_once().await
    }
}

#[derive(Debug, thiserror::Error)]
pub enum PollerError {
    #[error("history: {0}")]
    History(String),
    #[error("forgejo: {0}")]
    Forgejo(String),
    #[error("chat: {0}")]
    Chat(String),
}

impl From<ChatError> for PollerError {
    fn from(e: ChatError) -> Self {
        PollerError::Chat(e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ar_index::InMemoryLearningsStore;
    use ar_llm::Router;
    use ar_orchestrator::review_history::InMemoryReviewHistory;
    use ar_orchestrator::{NoOpDispatcher, ReviewJob};
    use async_trait::async_trait;
    use std::sync::Mutex as StdMutex;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    /// Records every job dispatched via the `JobDispatcher` trait.
    /// Used by the re-review test to confirm a poll-driven `@... re-review`
    /// queues a fresh review job.
    struct RecordingDispatcher {
        seen: StdMutex<Vec<ReviewJob>>,
    }

    #[async_trait]
    impl JobDispatcher for RecordingDispatcher {
        async fn dispatch(&self, job: ReviewJob) {
            self.seen.lock().unwrap().push(job);
        }
    }

    fn key(owner: &str, repo: &str, pr: u64) -> PrKey {
        PrKey {
            owner: owner.into(),
            repo: repo.into(),
            pr_number: pr,
        }
    }

    async fn poller_for(
        server: &MockServer,
        history: Arc<InMemoryReviewHistory>,
        dispatcher: Arc<dyn JobDispatcher>,
    ) -> ChatPoller {
        let forgejo = Arc::new(ForgejoClient::new(&server.uri(), "tok").expect("client"));
        let llm = Arc::new(Router::new());
        let learnings: Arc<dyn LearningsStore> = Arc::new(InMemoryLearningsStore::new());
        ChatPoller::new(
            forgejo,
            llm,
            learnings,
            history,
            dispatcher,
            "auto_review",
            "auto_review",
        )
    }

    #[tokio::test]
    async fn poll_once_with_no_known_prs_does_nothing() {
        let server = MockServer::start().await;
        let history = Arc::new(InMemoryReviewHistory::new());
        let poller = poller_for(&server, history.clone(), Arc::new(NoOpDispatcher)).await;
        poller.run_once_for_tests().await.expect("ok");
    }

    #[tokio::test]
    async fn first_poll_seeds_cursor_to_max_id_without_dispatching_old_comments() {
        let server = MockServer::start().await;
        let history = Arc::new(InMemoryReviewHistory::new());
        let k = key("alice", "widgets", 1);
        history.record(&k, "deadbeef").await.unwrap();

        // Two pre-existing comments; one even mentions the bot. On
        // the first poll we should set the cursor to max(id) = 7
        // and NOT dispatch — first run = "discover what already
        // exists, mark as seen".
        Mock::given(method("GET"))
            .and(path("/api/v1/repos/alice/widgets/pulls/1/comments"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {"id": 3, "body": "looks good", "user": {"login": "carol"}},
                {"id": 7, "body": "@auto_review remember always Result", "user": {"login": "bob"}}
            ])))
            .mount(&server)
            .await;

        let dispatcher = Arc::new(NoOpDispatcher);
        let poller = poller_for(&server, history, dispatcher).await;
        poller.run_once_for_tests().await.expect("ok");
        // Cursor advanced past every existing comment.
        assert_eq!(poller.cursor_for(&k).await, Some(7));
    }

    #[tokio::test]
    async fn second_poll_dispatches_new_mentions_only() {
        let server = MockServer::start().await;
        let history = Arc::new(InMemoryReviewHistory::new());
        let k = key("alice", "widgets", 1);
        history.record(&k, "deadbeef").await.unwrap();

        // Round 1 returns one pre-existing mention; round 2 returns
        // one *new* mention plus the same old one.
        Mock::given(method("GET"))
            .and(path("/api/v1/repos/alice/widgets/pulls/1/comments"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {"id": 5, "body": "old chatter, no mention", "user": {"login": "carol"}}
            ])))
            .up_to_n_times(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v1/repos/alice/widgets/pulls/1/comments"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {"id": 5, "body": "old chatter, no mention", "user": {"login": "carol"}},
                {"id": 9, "body": "@auto_review re-review", "user": {"login": "bob"}}
            ])))
            .mount(&server)
            .await;
        // Round 2 → re-review handler fetches the PR.
        Mock::given(method("GET"))
            .and(path("/api/v1/repos/alice/widgets/pulls/1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "number": 1,
                "title": "x",
                "body": "",
                "draft": false,
                "head": {"ref": "t", "sha": "newsha"},
                "base": {"ref": "main", "sha": "ms"}
            })))
            .mount(&server)
            .await;
        // Round 2 → re-review handler posts a confirmation comment.
        Mock::given(method("POST"))
            .and(path("/api/v1/repos/alice/widgets/issues/1/comments"))
            .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({"id": 99})))
            .mount(&server)
            .await;

        let dispatcher = Arc::new(RecordingDispatcher {
            seen: StdMutex::new(Vec::new()),
        });
        let dispatcher_dyn: Arc<dyn JobDispatcher> = dispatcher.clone();
        let poller = poller_for(&server, history, dispatcher_dyn).await;

        // First pass: cursor seeds to 5, no dispatch.
        poller.run_once_for_tests().await.expect("ok");
        assert_eq!(poller.cursor_for(&k).await, Some(5));
        assert!(dispatcher.seen.lock().unwrap().is_empty());

        // Second pass: comment 9 is new → dispatched as re-review.
        poller.run_once_for_tests().await.expect("ok");
        assert_eq!(poller.cursor_for(&k).await, Some(9));
        let seen = dispatcher.seen.lock().unwrap();
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].pr_number, 1);
        assert_eq!(seen[0].head_sha, "newsha");
        assert!(seen[0].force);
    }

    #[tokio::test]
    async fn comments_authored_by_the_bot_itself_are_skipped() {
        let server = MockServer::start().await;
        let history = Arc::new(InMemoryReviewHistory::new());
        let k = key("alice", "widgets", 1);
        history.record(&k, "deadbeef").await.unwrap();

        // First poll seeds cursor; no comments yet.
        Mock::given(method("GET"))
            .and(path("/api/v1/repos/alice/widgets/pulls/1/comments"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([])))
            .up_to_n_times(1)
            .mount(&server)
            .await;
        // Second poll: a bot-authored comment that mentions itself.
        // Must not loop.
        Mock::given(method("GET"))
            .and(path("/api/v1/repos/alice/widgets/pulls/1/comments"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {"id": 11, "body": "@auto_review autofix posted 1 patch", "user": {"login": "auto_review"}}
            ])))
            .mount(&server)
            .await;

        let dispatcher = Arc::new(RecordingDispatcher {
            seen: StdMutex::new(Vec::new()),
        });
        let dispatcher_dyn: Arc<dyn JobDispatcher> = dispatcher.clone();
        let poller = poller_for(&server, history, dispatcher_dyn).await;
        poller.run_once_for_tests().await.expect("seed");
        poller.run_once_for_tests().await.expect("ok");

        // Cursor advanced past the bot's own comment.
        assert_eq!(poller.cursor_for(&k).await, Some(11));
        // But no dispatch occurred — we filtered the bot out.
        assert!(dispatcher.seen.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn poll_metrics_count_cycles_and_pr_failures() {
        let server = MockServer::start().await;
        let history = Arc::new(InMemoryReviewHistory::new());
        let k_bad = key("alice", "broken", 1);
        let k_good = key("alice", "widgets", 2);
        history.record(&k_bad, "x").await.unwrap();
        history.record(&k_good, "y").await.unwrap();

        Mock::given(method("GET"))
            .and(path("/api/v1/repos/alice/broken/pulls/1/comments"))
            .respond_with(ResponseTemplate::new(500))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v1/repos/alice/widgets/pulls/2/comments"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([])))
            .mount(&server)
            .await;

        let metrics = Arc::new(Metrics::new());
        let dispatcher = Arc::new(NoOpDispatcher);
        let poller = poller_for(&server, history, dispatcher)
            .await
            .with_metrics(metrics.clone());
        poller.run_once_for_tests().await.expect("ok");
        poller.run_once_for_tests().await.expect("ok");

        let out = metrics.render();
        // Two complete cycles ran.
        assert!(
            out.contains("auto_review_poll_cycles_total 2\n"),
            "{out}"
        );
        // The broken PR failed both cycles.
        assert!(
            out.contains("auto_review_poll_pr_failures_total 2\n"),
            "{out}"
        );
        // Nothing dispatched (no mentions).
        assert!(
            out.contains("auto_review_poll_mentions_dispatched_total 0\n"),
            "{out}"
        );
    }

    #[tokio::test]
    async fn poll_metrics_count_dispatched_mentions() {
        let server = MockServer::start().await;
        let history = Arc::new(InMemoryReviewHistory::new());
        let k = key("alice", "widgets", 1);
        history.record(&k, "deadbeef").await.unwrap();

        // First poll: empty, seeds cursor to 0.
        Mock::given(method("GET"))
            .and(path("/api/v1/repos/alice/widgets/pulls/1/comments"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([])))
            .up_to_n_times(1)
            .mount(&server)
            .await;
        // Second poll: a `help` mention. The chat handler posts a
        // confirmation comment to /issues/<n>/comments.
        Mock::given(method("GET"))
            .and(path("/api/v1/repos/alice/widgets/pulls/1/comments"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {"id": 9, "body": "@auto_review help", "user": {"login": "bob"}}
            ])))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/api/v1/repos/alice/widgets/issues/1/comments"))
            .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({"id": 99})))
            .mount(&server)
            .await;

        let metrics = Arc::new(Metrics::new());
        let dispatcher = Arc::new(NoOpDispatcher);
        let poller = poller_for(&server, history, dispatcher)
            .await
            .with_metrics(metrics.clone());
        poller.run_once_for_tests().await.expect("seed");
        poller.run_once_for_tests().await.expect("dispatch");

        let out = metrics.render();
        assert!(out.contains("auto_review_poll_cycles_total 2\n"));
        assert!(out.contains("auto_review_poll_pr_failures_total 0\n"));
        assert!(out.contains("auto_review_poll_mentions_dispatched_total 1\n"));
    }

    #[tokio::test]
    async fn forgejo_error_on_one_pr_does_not_abort_the_pass() {
        let server = MockServer::start().await;
        let history = Arc::new(InMemoryReviewHistory::new());
        let k_bad = key("alice", "broken", 1);
        let k_good = key("alice", "widgets", 2);
        history.record(&k_bad, "x").await.unwrap();
        history.record(&k_good, "y").await.unwrap();

        Mock::given(method("GET"))
            .and(path("/api/v1/repos/alice/broken/pulls/1/comments"))
            .respond_with(ResponseTemplate::new(500))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v1/repos/alice/widgets/pulls/2/comments"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {"id": 3, "body": "no mention", "user": {"login": "carol"}}
            ])))
            .mount(&server)
            .await;

        let dispatcher = Arc::new(NoOpDispatcher);
        let poller = poller_for(&server, history, dispatcher).await;
        // Top-level call returns Ok despite the per-PR failure on
        // the broken repo; the good PR's cursor still advances.
        poller.run_once_for_tests().await.expect("ok");
        assert_eq!(poller.cursor_for(&k_good).await, Some(3));
        assert_eq!(poller.cursor_for(&k_bad).await, None);
    }
}

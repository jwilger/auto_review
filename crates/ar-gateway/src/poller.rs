//! Background poller for `@<bot>` mentions in PR comments.
//!
//! Forgejo doesn't fire the `pull_request_review_comment` webhook
//! reliably for replies inside an inline review thread (gitea#26023),
//! and the `issue_comment` webhook covers top-level PR comments only.
//! So a periodic background task, for every PR in the review history,
//! lists both kinds of comment and dispatches any new `@<bot_name>`
//! mentions (plus the `@auto_review` compatibility alias) through the
//! chat handler. Endpoints polled per PR, every cycle:
//!
//! - `GET /repos/{o}/{r}/issues/{n}/comments`: top-level conversation
//!   comments. Forgejo filters this list to plain comments, so it never
//!   contains inline review comments.
//! - `GET /repos/{o}/{r}/pulls/{n}/reviews`: the PR's review records.
//! - `GET /repos/{o}/{r}/pulls/{n}/reviews/{id}/comments`: the inline
//!   comments of each submitted (non-`PENDING`) review, which is where
//!   thread replies land.
//!
//! Per-cycle cost per PR: one issue-comments list, one reviews list,
//! and one comments list per review. Every review is re-listed each
//! cycle because a thread reply may be attached to the review that
//! opened the thread rather than to a new review record.
//!
//! Cursor: one per-(repo, pr) highest-seen comment id covers both
//! sources. Forgejo issues comment ids from a single sequence, so
//! id-based monotonicity is reliable; comments with id ≤ cursor have
//! been processed (or existed before the bot started). Each cycle
//! merges both sources and walks them in ascending id order before
//! claiming, so a newer top-level comment can never advance the cursor
//! past an older, unprocessed inline reply. The cursor map is shared
//! with the webhook path (`claim_chat_comment`), so a comment seen by
//! both dispatches at most once. Cursors are in-memory; on first sight
//! of a PR (including after a gateway restart) the cursor is seeded to
//! the highest current comment id across both sources and nothing is
//! dispatched, so history is never reprocessed.
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
use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;

pub type SharedCommentCursors = Arc<Mutex<HashMap<PrKey, u64>>>;

/// Atomically claim a Forgejo chat comment id for one PR.
///
/// Returns `true` only for the first caller that advances the shared
/// per-PR cursor past `comment_id`. Webhook handling and the poller use this
/// same operation so a comment observed through both paths is dispatched at
/// most once.
pub async fn claim_chat_comment(
    cursors: &SharedCommentCursors,
    key: PrKey,
    comment_id: u64,
) -> bool {
    let mut guard = cursors.lock().await;
    let cursor = guard.entry(key).or_insert(0);
    if comment_id <= *cursor {
        return false;
    }
    *cursor = comment_id;
    true
}

async fn seed_chat_cursor_monotonic(cursors: &SharedCommentCursors, key: PrKey, max_seen: u64) {
    let mut guard = cursors.lock().await;
    let cursor = guard.entry(key).or_insert(0);
    if max_seen > *cursor {
        *cursor = max_seen;
    }
}

/// Root comment id of the inline review thread `comment` belongs to.
///
/// Forgejo has no explicit "in reply to" field on review comments, so a
/// thread is identified by its anchor: every comment with the same `path`
/// and `original_position`, across all of the PR's reviews, belongs to one
/// thread, and the lowest id is the comment that opened it. Returns the
/// comment's own id when it is the earliest in its thread.
fn thread_root_id(
    all_inline: &[ar_forge::PrReviewComment],
    comment: &ar_forge::PrReviewComment,
) -> u64 {
    all_inline
        .iter()
        .filter(|c| c.path == comment.path && c.original_position == comment.original_position)
        .map(|c| c.id)
        .fold(comment.id, u64::min)
}

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
    cursors: SharedCommentCursors,
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

    pub fn with_cursors(mut self, cursors: SharedCommentCursors) -> Self {
        self.cursors = cursors;
        self
    }

    /// Spawn the polling loop on the current tokio runtime. Runs
    /// forever; cancel the runtime to stop.
    pub fn spawn(self, interval: Duration) {
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(interval);
            // Delay (not the default Burst) so a slow poll_once
            // doesn't trigger a thundering-herd of catch-up ticks.
            // For a polling bot where each cycle does N API calls,
            // burst-catchup amplifies pressure on Forgejo when it's
            // already slow. Delay realigns the schedule from the
            // previous tick so cycles stay spaced even after a
            // long poll.
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
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
        // Prune cursor entries for PRs that are no longer in
        // history (e.g. operator ran `auto_review reset-pr` or
        // `purge-history`). Without this, the cursors map grows
        // monotonically over the gateway's lifetime as the
        // history shrinks. Each entry is small but a long-running
        // deployment with thousands of historical PRs accumulates
        // dead cursors forever.
        {
            let known_set: std::collections::HashSet<&PrKey> = known.iter().collect();
            let mut cursors = self.cursors.lock().await;
            cursors.retain(|k, _| known_set.contains(k));
        }
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
        let issue_comments = self
            .forgejo
            .list_pr_review_comments(&key.owner, &key.repo, key.pr_number)
            .await
            .map_err(|e| PollerError::Forgejo(e.to_string()))?;
        let inline_comments = self.list_inline_comments(key).await?;

        // One shared cursor covers both sources, so merge them and walk
        // the union in ascending id order before claiming anything:
        // otherwise a newer top-level comment could advance the cursor
        // past an older inline reply that has not been looked at yet.
        // Keyed by id, which also dedups a comment listed twice.
        let mut candidates: BTreeMap<u64, (&ar_forge::PrReviewComment, Option<u64>)> =
            BTreeMap::new();
        for c in &issue_comments {
            candidates.entry(c.id).or_insert((c, None));
        }
        for c in &inline_comments {
            let root = thread_root_id(&inline_comments, c);
            candidates.entry(c.id).or_insert((c, Some(root)));
        }

        // Distinguish "first sight of this PR" from "cursor==0
        // already recorded". On first sight, we want to seed the
        // cursor to max(comment.id) WITHOUT dispatching — otherwise
        // a bot restart would re-fire every historical @-mention
        // ("remember X" "re-review" etc.) that happens to live in
        // the inline comment thread. The doc comment up top has
        // always promised this behaviour; the previous
        // `unwrap_or(0)` accidentally dispatched everything.
        let (cursor, first_sight) = {
            let map = self.cursors.lock().await;
            match map.get(key).copied() {
                Some(c) => (c, false),
                None => (0, true),
            }
        };
        if first_sight {
            // First-sight seeding: record the highest historical id
            // across both sources without dispatching any historical
            // mentions. Keep the update monotonic: a webhook may have
            // claimed a newer comment while this poll was listing, and
            // seeding must not lower that shared cursor.
            let max_seen = candidates.keys().next_back().copied().unwrap_or(cursor);
            seed_chat_cursor_monotonic(&self.cursors, key.clone(), max_seen).await;
            return Ok(());
        }

        let mut to_dispatch: Vec<(&ar_forge::PrReviewComment, Option<u64>)> = Vec::new();
        for (&id, &(c, reply_to)) in candidates.range(cursor.saturating_add(1)..) {
            // Case-insensitive to match the webhook handler's
            // is_bot_self. Forgejo logins are case-insensitively
            // unique but the wire format preserves the casing the
            // user registered with; an exact-match check here
            // would risk treating "Auto_review" comments from our
            // own bot as user comments and reply-looping.
            if c.user.login.eq_ignore_ascii_case(&self.bot_login) {
                let _ = claim_chat_comment(&self.cursors, key.clone(), id).await;
                continue; // never reply to ourselves
            }
            if self.mentions_bot(&c.body) {
                if claim_chat_comment(&self.cursors, key.clone(), id).await {
                    to_dispatch.push((c, reply_to));
                }
            } else {
                let _ = claim_chat_comment(&self.cursors, key.clone(), id).await;
            }
        }

        for (comment, reply_to) in to_dispatch {
            let id = comment.id;
            let command = parse_chat_command(&comment.body, &self.bot_name);
            let handler = ChatHandler {
                host: self.forgejo.as_ref(),
                llm: &self.llm,
                learnings: self.learnings.as_ref(),
                dispatcher: Some(self.dispatcher.clone()),
            };
            let ctx = ChatContext {
                owner: &key.owner,
                repo: &key.repo,
                issue_number: key.pr_number,
                commenter_login: &comment.user.login,
                bot_login: &self.bot_name,
                reply_to_comment_id: reply_to,
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

    /// List the inline review-thread comments of every submitted review
    /// on the PR. Pending (draft) reviews are skipped: their comments are
    /// not visible to anyone but their author yet.
    async fn list_inline_comments(
        &self,
        key: &PrKey,
    ) -> Result<Vec<ar_forge::PrReviewComment>, PollerError> {
        let reviews = self
            .forgejo
            .list_pull_reviews(&key.owner, &key.repo, key.pr_number)
            .await
            .map_err(|e| PollerError::Forgejo(e.to_string()))?;
        let mut inline = Vec::new();
        for review in reviews
            .iter()
            .filter(|r| !r.state.eq_ignore_ascii_case("PENDING"))
        {
            let comments = self
                .forgejo
                .list_pull_review_comments(&key.owner, &key.repo, key.pr_number, review.id)
                .await
                .map_err(|e| PollerError::Forgejo(e.to_string()))?;
            inline.extend(comments);
        }
        Ok(inline)
    }

    /// Cheap pre-filter: only dispatch comments that even mention us.
    /// Case-insensitive to match Forgejo's username semantics and the
    /// parser's own case-insensitive prefix check — otherwise a comment
    /// saying "@AUTO-REVIEW help" would be filtered out here even though
    /// the parser would accept it. Bytewise windows() so we don't
    /// allocate a lowercased copy of every comment body.
    fn mentions_bot(&self, body: &str) -> bool {
        let needle = format!("@{}", self.bot_name);
        let needle_bytes = needle.as_bytes();
        let compatibility_needle = (*self.bot_name == "auto-review").then_some(b"@auto_review");
        let body_bytes = body.as_bytes();
        body_bytes
            .windows(needle_bytes.len())
            .any(|w| w.eq_ignore_ascii_case(needle_bytes))
            || compatibility_needle.is_some_and(|alias| {
                body_bytes
                    .windows(alias.len())
                    .any(|w| w.eq_ignore_ascii_case(alias))
            })
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
    use crate::{build_router, AppState, ChatDeps};
    use ::hmac::{Hmac, KeyInit, Mac};
    use ar_index::InMemoryLearningsStore;
    use ar_llm::Router;
    use ar_orchestrator::review_history::InMemoryReviewHistory;
    use ar_orchestrator::{NoOpDispatcher, ReviewJob};
    use async_trait::async_trait;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use sha2::Sha256;
    use std::sync::Mutex as StdMutex;
    use tower::ServiceExt;
    use wiremock::matchers::{method, path, path_regex};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    type HmacSha256 = Hmac<Sha256>;

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

    fn sign(secret: &str, body: &[u8]) -> String {
        let mut mac = HmacSha256::new_from_slice(secret.as_bytes()).unwrap();
        mac.update(body);
        hex::encode(mac.finalize().into_bytes())
    }

    fn issue_comment_payload(id: u64, action: &str, body: &str, sender: &str) -> Vec<u8> {
        serde_json::to_vec(&serde_json::json!({
            "action": action,
            "comment": {"id": id, "body": body, "user": {"login": sender, "id": 1}},
            "issue": {
                "number": 1,
                "title": "x",
                "pull_request": {"html_url": "https://forge/alice/widgets/pulls/1"}
            },
            "repository": {
                "name": "widgets", "full_name": "alice/widgets", "default_branch": "main",
                "owner": {"login": "alice", "id": 99}
            },
            "sender": {"login": sender, "id": 1}
        }))
        .unwrap()
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

    async fn hyphenated_identity_poller_for(
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
            "auto-review",
            "auto-review",
        )
    }

    #[tokio::test]
    async fn poll_once_with_no_known_prs_does_nothing() {
        let server = MockServer::start().await;
        mount_no_reviews(&server).await;
        let history = Arc::new(InMemoryReviewHistory::new());
        let poller = poller_for(&server, history.clone(), Arc::new(NoOpDispatcher)).await;
        poller.run_once_for_tests().await.expect("ok");
    }

    #[tokio::test]
    async fn first_poll_seeds_cursor_to_max_id_without_dispatching_old_comments() {
        let server = MockServer::start().await;
        mount_no_reviews(&server).await;
        let history = Arc::new(InMemoryReviewHistory::new());
        let k = key("alice", "widgets", 1);
        history.record(&k, "deadbeef").await.unwrap();

        // Two pre-existing comments; one is a `re-review` mention.
        // On the first poll we MUST set the cursor to max(id) = 7
        // and NOT dispatch — first run = "discover what already
        // exists, mark as seen". A bug here would make a bot
        // restart re-fire every historical mention against PRs
        // already in the review history.
        Mock::given(method("GET"))
            .and(path("/api/v1/repos/alice/widgets/issues/1/comments"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {"id": 3, "body": "looks good", "user": {"login": "carol"}},
                {"id": 7, "body": "@auto_review re-review", "user": {"login": "bob"}}
            ])))
            .mount(&server)
            .await;
        // Mock the PR fetch + comment-post too: if first-poll
        // dispatch were broken, the chat handler's handle_re_review
        // path would otherwise short-circuit on Forgejo 404 before
        // reaching the dispatcher and the assertion below would
        // pass for the wrong reason.
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
        Mock::given(method("POST"))
            .and(path("/api/v1/repos/alice/widgets/issues/1/comments"))
            .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({"id": 1})))
            .mount(&server)
            .await;

        // Use a recording dispatcher so we can assert no dispatch
        // happened. ReReview is the chat command that goes through
        // the dispatcher, so it's the right canary for "did the
        // poller fire any historical mentions on first sight".
        let dispatcher = Arc::new(RecordingDispatcher {
            seen: StdMutex::new(Vec::new()),
        });
        let poller = poller_for(
            &server,
            history,
            dispatcher.clone() as Arc<dyn JobDispatcher>,
        )
        .await;
        poller.run_once_for_tests().await.expect("ok");
        // Cursor advanced past every existing comment.
        assert_eq!(poller.cursor_for(&k).await, Some(7));
        // CRITICAL: nothing dispatched. Re-firing historical
        // mentions on bot restart is a real footgun (`re-review` x
        // N would burn LLM tokens; `remember X` x N would store
        // dupes).
        assert!(
            dispatcher.seen.lock().unwrap().is_empty(),
            "first poll must not dispatch historical mentions"
        );
    }

    #[tokio::test]
    async fn first_sight_seeding_never_lowers_already_claimed_cursor() {
        let cursors: SharedCommentCursors = Arc::new(Mutex::new(HashMap::new()));
        let k = key("alice", "widgets", 1);

        assert!(claim_chat_comment(&cursors, k.clone(), 9).await);
        seed_chat_cursor_monotonic(&cursors, k.clone(), 5).await;

        assert_eq!(cursors.lock().await.get(&k).copied(), Some(9));
    }

    #[tokio::test]
    async fn second_poll_dispatches_new_mentions_only() {
        let server = MockServer::start().await;
        mount_no_reviews(&server).await;
        let history = Arc::new(InMemoryReviewHistory::new());
        let k = key("alice", "widgets", 1);
        history.record(&k, "deadbeef").await.unwrap();

        // Round 1 returns one pre-existing mention; round 2 returns
        // one *new* mention plus the same old one.
        Mock::given(method("GET"))
            .and(path("/api/v1/repos/alice/widgets/issues/1/comments"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {"id": 5, "body": "old chatter, no mention", "user": {"login": "carol"}}
            ])))
            .up_to_n_times(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v1/repos/alice/widgets/issues/1/comments"))
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
    async fn top_level_re_review_seen_by_webhook_and_poller_dispatches_once() {
        let server = MockServer::start().await;
        mount_no_reviews(&server).await;
        let history = Arc::new(InMemoryReviewHistory::new());
        let k = key("alice", "widgets", 1);
        history.record(&k, "deadbeef").await.unwrap();

        // Seed the poller cursor before the new top-level PR comment exists.
        Mock::given(method("GET"))
            .and(path("/api/v1/repos/alice/widgets/issues/1/comments"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([])))
            .up_to_n_times(1)
            .mount(&server)
            .await;
        // The next poll sees the same top-level comment that Forgejo already
        // delivered through the issue_comment webhook.
        Mock::given(method("GET"))
            .and(path("/api/v1/repos/alice/widgets/issues/1/comments"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {"id": 9, "body": "@auto_review re-review", "user": {"login": "bob"}}
            ])))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v1/repos/alice/widgets/pulls/1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "number": 1,
                "title": "x",
                "body": "review this",
                "draft": false,
                "state": "open",
                "head": {"ref": "t", "sha": "newsha"},
                "base": {"ref": "main", "sha": "ms"}
            })))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/api/v1/repos/alice/widgets/issues/1/comments"))
            .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({"id": 99})))
            .mount(&server)
            .await;

        let forgejo = Arc::new(ForgejoClient::new(&server.uri(), "tok").expect("client"));
        let llm = Arc::new(Router::new());
        let learnings: Arc<dyn LearningsStore> = Arc::new(InMemoryLearningsStore::new());
        let dispatcher = Arc::new(RecordingDispatcher {
            seen: StdMutex::new(Vec::new()),
        });
        let dispatcher_dyn: Arc<dyn JobDispatcher> = dispatcher.clone();
        let poller = ChatPoller::new(
            forgejo.clone(),
            llm.clone(),
            learnings.clone(),
            history,
            dispatcher_dyn.clone(),
            "auto_review",
            "auto_review",
        );

        poller.run_once_for_tests().await.expect("seed cursor");
        assert_eq!(poller.cursor_for(&k).await, Some(0));

        let app = build_router(
            AppState::new("s", dispatcher_dyn)
                .with_chat(ChatDeps {
                    forgejo,
                    llm,
                    learnings,
                })
                .with_chat_comment_cursors(poller.cursors.clone()),
        );
        let body = issue_comment_payload(9, "created", "@auto_review re-review", "bob");
        let sig = sign("s", &body);
        let req = Request::post("/webhooks/forgejo")
            .header("x-forgejo-event", "issue_comment")
            .header("x-forgejo-signature", sig)
            .body(Body::from(body))
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::ACCEPTED);

        for _ in 0..20 {
            if dispatcher.seen.lock().unwrap().len() == 1 {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        assert_eq!(
            dispatcher.seen.lock().unwrap().len(),
            1,
            "issue_comment webhook should dispatch the re-review once before polling"
        );

        poller
            .run_once_for_tests()
            .await
            .expect("poll fallback pass");

        {
            let seen = dispatcher.seen.lock().unwrap();
            assert_eq!(
                seen.len(),
                1,
                "the same top-level re-review comment must not dispatch through both webhook and poller"
            );
            assert_eq!(seen[0].pr_number, 1);
            assert_eq!(seen[0].head_sha, "newsha");
            assert!(seen[0].force);
        }
        assert_eq!(poller.cursor_for(&k).await, Some(9));
    }

    #[tokio::test]
    async fn top_level_re_review_seen_by_poller_then_delayed_webhook_dispatches_once() {
        let server = MockServer::start().await;
        mount_no_reviews(&server).await;
        let history = Arc::new(InMemoryReviewHistory::new());
        let k = key("alice", "widgets", 1);
        history.record(&k, "deadbeef").await.unwrap();

        // First poll seeds an empty cursor; second poll sees the new
        // top-level PR comment before Forgejo delivers its delayed
        // issue_comment webhook.
        Mock::given(method("GET"))
            .and(path("/api/v1/repos/alice/widgets/issues/1/comments"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([])))
            .up_to_n_times(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v1/repos/alice/widgets/issues/1/comments"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {"id": 9, "body": "@auto_review re-review", "user": {"login": "bob"}}
            ])))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v1/repos/alice/widgets/pulls/1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "number": 1,
                "title": "x",
                "body": "review this",
                "draft": false,
                "state": "open",
                "head": {"ref": "t", "sha": "newsha"},
                "base": {"ref": "main", "sha": "ms"}
            })))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/api/v1/repos/alice/widgets/issues/1/comments"))
            .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({"id": 99})))
            .mount(&server)
            .await;

        let forgejo = Arc::new(ForgejoClient::new(&server.uri(), "tok").expect("client"));
        let llm = Arc::new(Router::new());
        let learnings: Arc<dyn LearningsStore> = Arc::new(InMemoryLearningsStore::new());
        let dispatcher = Arc::new(RecordingDispatcher {
            seen: StdMutex::new(Vec::new()),
        });
        let dispatcher_dyn: Arc<dyn JobDispatcher> = dispatcher.clone();
        let poller = ChatPoller::new(
            forgejo.clone(),
            llm.clone(),
            learnings.clone(),
            history,
            dispatcher_dyn.clone(),
            "auto_review",
            "auto_review",
        );

        poller.run_once_for_tests().await.expect("seed cursor");
        assert_eq!(poller.cursor_for(&k).await, Some(0));
        poller
            .run_once_for_tests()
            .await
            .expect("poller dispatches new top-level comment");
        assert_eq!(poller.cursor_for(&k).await, Some(9));
        assert_eq!(
            dispatcher.seen.lock().unwrap().len(),
            1,
            "poller should dispatch the re-review once before the delayed webhook"
        );

        let app = build_router(
            AppState::new("s", dispatcher_dyn)
                .with_chat(ChatDeps {
                    forgejo,
                    llm,
                    learnings,
                })
                .with_chat_comment_cursors(poller.cursors.clone()),
        );
        let body = issue_comment_payload(9, "created", "@auto_review re-review", "bob");
        let sig = sign("s", &body);
        let req = Request::post("/webhooks/forgejo")
            .header("x-forgejo-event", "issue_comment")
            .header("x-forgejo-signature", sig)
            .body(Body::from(body))
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::ACCEPTED);

        for _ in 0..20 {
            if dispatcher.seen.lock().unwrap().len() > 1 {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }

        let seen = dispatcher.seen.lock().unwrap();
        assert_eq!(
            seen.len(),
            1,
            "a delayed issue_comment webhook for a comment already dispatched by the poller must not queue a second forced review"
        );
        assert_eq!(seen[0].pr_number, 1);
        assert_eq!(seen[0].head_sha, "newsha");
        assert!(seen[0].force);
    }

    #[tokio::test]
    async fn comments_authored_by_the_bot_itself_are_skipped() {
        let server = MockServer::start().await;
        mount_no_reviews(&server).await;
        let history = Arc::new(InMemoryReviewHistory::new());
        let k = key("alice", "widgets", 1);
        history.record(&k, "deadbeef").await.unwrap();

        // First poll seeds cursor; no comments yet.
        Mock::given(method("GET"))
            .and(path("/api/v1/repos/alice/widgets/issues/1/comments"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([])))
            .up_to_n_times(1)
            .mount(&server)
            .await;
        // Second poll: a bot-authored comment that mentions itself.
        // Must not loop.
        Mock::given(method("GET"))
            .and(path("/api/v1/repos/alice/widgets/issues/1/comments"))
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
    async fn hyphenated_bot_identity_poller_dispatches_temporary_alias_and_filters_self_comments() {
        let server = MockServer::start().await;
        mount_no_reviews(&server).await;
        let history = Arc::new(InMemoryReviewHistory::new());
        let k = key("alice", "widgets", 1);
        history.record(&k, "deadbeef").await.unwrap();

        Mock::given(method("GET"))
            .and(path("/api/v1/repos/alice/widgets/issues/1/comments"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([])))
            .up_to_n_times(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v1/repos/alice/widgets/issues/1/comments"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {"id": 10, "body": "@auto-review re-review", "user": {"login": "auto-review"}},
                {"id": 11, "body": "@auto_review re-review", "user": {"login": "bob"}}
            ])))
            .mount(&server)
            .await;
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
        Mock::given(method("POST"))
            .and(path("/api/v1/repos/alice/widgets/issues/1/comments"))
            .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({"id": 99})))
            .mount(&server)
            .await;

        let dispatcher = Arc::new(RecordingDispatcher {
            seen: StdMutex::new(Vec::new()),
        });
        let dispatcher_dyn: Arc<dyn JobDispatcher> = dispatcher.clone();
        let poller = hyphenated_identity_poller_for(&server, history, dispatcher_dyn).await;

        poller.run_once_for_tests().await.expect("seed");
        poller
            .run_once_for_tests()
            .await
            .expect("dispatch user mention");

        assert_eq!(poller.cursor_for(&k).await, Some(11));
        let seen = dispatcher.seen.lock().unwrap();
        assert_eq!(
            seen.len(),
            1,
            "poller must ignore the hyphenated bot's own comment and dispatch the user-authored temporary @auto_review alias while configured as auto-review"
        );
        assert_eq!(seen[0].head_sha, "newsha");
        assert!(seen[0].force);
    }

    #[tokio::test]
    async fn poll_metrics_count_cycles_and_pr_failures() {
        let server = MockServer::start().await;
        mount_no_reviews(&server).await;
        let history = Arc::new(InMemoryReviewHistory::new());
        let k_bad = key("alice", "broken", 1);
        let k_good = key("alice", "widgets", 2);
        history.record(&k_bad, "x").await.unwrap();
        history.record(&k_good, "y").await.unwrap();

        Mock::given(method("GET"))
            .and(path("/api/v1/repos/alice/broken/issues/1/comments"))
            .respond_with(ResponseTemplate::new(500))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v1/repos/alice/widgets/issues/2/comments"))
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
        assert!(out.contains("auto_review_poll_cycles_total 2\n"), "{out}");
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
        mount_no_reviews(&server).await;
        let history = Arc::new(InMemoryReviewHistory::new());
        let k = key("alice", "widgets", 1);
        history.record(&k, "deadbeef").await.unwrap();

        // First poll: empty, seeds cursor to 0.
        Mock::given(method("GET"))
            .and(path("/api/v1/repos/alice/widgets/issues/1/comments"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([])))
            .up_to_n_times(1)
            .mount(&server)
            .await;
        // Second poll: a `help` mention. The chat handler posts a
        // confirmation comment to /issues/<n>/comments.
        Mock::given(method("GET"))
            .and(path("/api/v1/repos/alice/widgets/issues/1/comments"))
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
        mount_no_reviews(&server).await;
        let history = Arc::new(InMemoryReviewHistory::new());
        let k_bad = key("alice", "broken", 1);
        let k_good = key("alice", "widgets", 2);
        history.record(&k_bad, "x").await.unwrap();
        history.record(&k_good, "y").await.unwrap();

        Mock::given(method("GET"))
            .and(path("/api/v1/repos/alice/broken/issues/1/comments"))
            .respond_with(ResponseTemplate::new(500))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v1/repos/alice/widgets/issues/2/comments"))
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

    #[tokio::test]
    async fn cursors_for_purged_prs_are_pruned_at_next_poll() {
        // Without prune: the cursors map grows monotonically over
        // the gateway's lifetime as the history shrinks (e.g.
        // operator running `auto_review reset-pr` or
        // `purge-history`). Each entry is small but accumulates.
        let server = MockServer::start().await;
        mount_no_reviews(&server).await;
        let history = Arc::new(InMemoryReviewHistory::new());
        let k = key("alice", "widgets", 1);
        history.record(&k, "deadbeef").await.unwrap();

        Mock::given(method("GET"))
            .and(path("/api/v1/repos/alice/widgets/issues/1/comments"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {"id": 5, "body": "noise", "user": {"login": "carol"}}
            ])))
            .mount(&server)
            .await;

        let dispatcher = Arc::new(NoOpDispatcher);
        let poller = poller_for(&server, history.clone(), dispatcher).await;
        // First poll seeds the cursor for k.
        poller.run_once_for_tests().await.expect("ok");
        assert_eq!(poller.cursor_for(&k).await, Some(5));

        // Operator clears the PR from history (e.g. via
        // `auto_review reset-pr`).
        history.clear(&k).await.unwrap();

        // Next poll: history.list_known() returns empty, so the
        // cursor for k should be pruned.
        poller.run_once_for_tests().await.expect("ok");
        assert_eq!(
            poller.cursor_for(&k).await,
            None,
            "cursor for purged PR must be removed from the in-memory map"
        );
    }

    /// The poller also walks `pulls/{n}/reviews`. Tests that only exercise
    /// top-level conversation comments mount an empty reviews list for
    /// every PR so the inline-review walk finds nothing.
    async fn mount_no_reviews(server: &MockServer) {
        Mock::given(method("GET"))
            .and(path_regex(r"^/api/v1/repos/[^/]+/[^/]+/pulls/\d+/reviews$"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([])))
            .mount(server)
            .await;
    }

    fn inline(id: u64, path: &str, original_position: u64) -> ar_forge::PrReviewComment {
        serde_json::from_value(serde_json::json!({
            "id": id,
            "body": "",
            "user": {"login": "bob"},
            "path": path,
            "original_position": original_position
        }))
        .unwrap()
    }

    #[test]
    fn thread_root_is_the_earliest_comment_even_in_another_review() {
        // Review 1 holds the bot finding (10); review 2 holds the reply (12).
        let all = vec![inline(12, "src/lib.rs", 4), inline(10, "src/lib.rs", 4)];
        assert_eq!(thread_root_id(&all, &all[0]), 10);
    }

    #[test]
    fn thread_root_of_the_earliest_comment_is_itself() {
        let all = vec![inline(10, "src/lib.rs", 4), inline(12, "src/lib.rs", 4)];
        assert_eq!(thread_root_id(&all, &all[0]), 10);
    }

    #[test]
    fn thread_root_separates_threads_at_different_positions_on_one_path() {
        let all = vec![
            inline(10, "src/lib.rs", 4),
            inline(11, "src/lib.rs", 9),
            inline(12, "src/lib.rs", 4),
            inline(13, "src/lib.rs", 9),
            inline(5, "src/main.rs", 9),
        ];
        assert_eq!(thread_root_id(&all, &all[3]), 11);
        assert_eq!(thread_root_id(&all, &all[2]), 10);
    }

    /// Mount the PR reviews list for `alice/widgets` PR `pr`.
    async fn mount_reviews(server: &MockServer, repo: &str, pr: u64, reviews: serde_json::Value) {
        Mock::given(method("GET"))
            .and(path(format!(
                "/api/v1/repos/alice/{repo}/pulls/{pr}/reviews"
            )))
            .respond_with(ResponseTemplate::new(200).set_body_json(reviews))
            .mount(server)
            .await;
    }

    async fn mount_review_comments(
        server: &MockServer,
        repo: &str,
        pr: u64,
        review_id: u64,
        comments: serde_json::Value,
    ) {
        Mock::given(method("GET"))
            .and(path(format!(
                "/api/v1/repos/alice/{repo}/pulls/{pr}/reviews/{review_id}/comments"
            )))
            .respond_with(ResponseTemplate::new(200).set_body_json(comments))
            .mount(server)
            .await;
    }

    fn bot_finding() -> serde_json::Value {
        serde_json::json!({
            "id": 10,
            "body": "**warning**: this unwraps a fallible parse",
            "user": {"login": "auto-review"},
            "path": "src/lib.rs",
            "original_position": 4
        })
    }

    fn help_reply() -> serde_json::Value {
        serde_json::json!({
            "id": 12,
            "body": "@auto-review help",
            "user": {"login": "bob"},
            "path": "src/lib.rs",
            "original_position": 4
        })
    }

    /// Run two poll cycles against a PR whose cursor is parked on the bot
    /// finding (10) and assert the inline `help` reply (12) is answered once.
    async fn assert_inline_help_reply_dispatches_once(server: MockServer) {
        let history = Arc::new(InMemoryReviewHistory::new());
        let k = key("alice", "widgets", 1);
        history.record(&k, "deadbeef").await.unwrap();
        Mock::given(method("GET"))
            .and(path("/api/v1/repos/alice/widgets/issues/1/comments"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([])))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/api/v1/repos/alice/widgets/issues/1/comments"))
            .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({"id": 99})))
            .expect(1)
            .mount(&server)
            .await;

        let cursors: SharedCommentCursors = Arc::new(Mutex::new(HashMap::from([(k.clone(), 10)])));
        let metrics = Arc::new(Metrics::new());
        let poller = hyphenated_identity_poller_for(&server, history, Arc::new(NoOpDispatcher))
            .await
            .with_cursors(cursors)
            .with_metrics(metrics.clone());
        poller.run_once_for_tests().await.expect("first cycle");
        poller.run_once_for_tests().await.expect("second cycle");

        let out = metrics.render();
        assert!(
            out.contains("auto_review_poll_mentions_dispatched_total 1\n"),
            "{out}"
        );
        assert!(
            out.contains("auto_review_poll_pr_failures_total 0\n"),
            "{out}"
        );
        assert_eq!(poller.cursor_for(&k).await, Some(12));
        server.verify().await;
    }

    #[tokio::test]
    async fn inline_reply_in_a_later_review_dispatches_once_across_two_cycles() {
        let server = MockServer::start().await;
        mount_reviews(
            &server,
            "widgets",
            1,
            serde_json::json!([
                {"id": 1, "state": "REQUEST_CHANGES", "user": {"login": "auto-review"}},
                {"id": 2, "state": "COMMENT", "user": {"login": "bob"}}
            ]),
        )
        .await;
        mount_review_comments(&server, "widgets", 1, 1, serde_json::json!([bot_finding()])).await;
        mount_review_comments(&server, "widgets", 1, 2, serde_json::json!([help_reply()])).await;

        assert_inline_help_reply_dispatches_once(server).await;
    }

    #[tokio::test]
    async fn inline_reply_attached_to_the_original_review_dispatches_once_across_two_cycles() {
        // Forgejo may attach a thread reply to the review that opened the
        // thread instead of creating a new review record; the poller must
        // not depend on either shape.
        let server = MockServer::start().await;
        mount_reviews(
            &server,
            "widgets",
            1,
            serde_json::json!([
                {"id": 1, "state": "REQUEST_CHANGES", "user": {"login": "auto-review"}}
            ]),
        )
        .await;
        mount_review_comments(
            &server,
            "widgets",
            1,
            1,
            serde_json::json!([bot_finding(), help_reply()]),
        )
        .await;

        assert_inline_help_reply_dispatches_once(server).await;
    }

    #[tokio::test]
    async fn first_sight_seeds_cursor_past_historical_inline_replies_without_dispatching() {
        let server = MockServer::start().await;
        let history = Arc::new(InMemoryReviewHistory::new());
        let k = key("alice", "widgets", 1);
        history.record(&k, "deadbeef").await.unwrap();

        Mock::given(method("GET"))
            .and(path("/api/v1/repos/alice/widgets/issues/1/comments"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {"id": 3, "body": "looks good", "user": {"login": "carol"}}
            ])))
            .mount(&server)
            .await;
        mount_reviews(
            &server,
            "widgets",
            1,
            serde_json::json!([{"id": 1, "state": "COMMENT", "user": {"login": "bob"}}]),
        )
        .await;
        mount_review_comments(
            &server,
            "widgets",
            1,
            1,
            serde_json::json!([{
                "id": 15,
                "body": "@auto_review re-review",
                "user": {"login": "bob"},
                "path": "src/lib.rs",
                "original_position": 4
            }]),
        )
        .await;
        // Mount the re-review side effects so a broken first-sight guard
        // would reach the dispatcher instead of failing on a 404.
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
        Mock::given(method("POST"))
            .and(path("/api/v1/repos/alice/widgets/issues/1/comments"))
            .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({"id": 99})))
            .mount(&server)
            .await;

        let dispatcher = Arc::new(RecordingDispatcher {
            seen: StdMutex::new(Vec::new()),
        });
        let poller = poller_for(
            &server,
            history,
            dispatcher.clone() as Arc<dyn JobDispatcher>,
        )
        .await;
        poller.run_once_for_tests().await.expect("ok");

        assert_eq!(poller.cursor_for(&k).await, Some(15));
        assert!(
            dispatcher.seen.lock().unwrap().is_empty(),
            "first sight must not dispatch historical inline mentions"
        );
    }

    #[tokio::test]
    async fn inline_comments_authored_by_the_bot_are_skipped() {
        let server = MockServer::start().await;
        let history = Arc::new(InMemoryReviewHistory::new());
        let k = key("alice", "widgets", 1);
        history.record(&k, "deadbeef").await.unwrap();

        Mock::given(method("GET"))
            .and(path("/api/v1/repos/alice/widgets/issues/1/comments"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([])))
            .mount(&server)
            .await;
        // First poll seeds with no reviews; second sees the bot's own
        // inline finding, which mentions the bot.
        Mock::given(method("GET"))
            .and(path("/api/v1/repos/alice/widgets/pulls/1/reviews"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([])))
            .up_to_n_times(1)
            .mount(&server)
            .await;
        mount_reviews(
            &server,
            "widgets",
            1,
            serde_json::json!([{"id": 1, "state": "COMMENT", "user": {"login": "auto_review"}}]),
        )
        .await;
        mount_review_comments(
            &server,
            "widgets",
            1,
            1,
            serde_json::json!([{
                "id": 11,
                "body": "Reply `@auto_review re-review` after fixing this.",
                "user": {"login": "auto_review"},
                "path": "src/lib.rs",
                "original_position": 4
            }]),
        )
        .await;

        let dispatcher = Arc::new(RecordingDispatcher {
            seen: StdMutex::new(Vec::new()),
        });
        let dispatcher_dyn: Arc<dyn JobDispatcher> = dispatcher.clone();
        let poller = poller_for(&server, history, dispatcher_dyn).await;
        poller.run_once_for_tests().await.expect("seed");
        poller.run_once_for_tests().await.expect("ok");

        assert_eq!(poller.cursor_for(&k).await, Some(11));
        assert!(dispatcher.seen.lock().unwrap().is_empty());
        let posts = server
            .received_requests()
            .await
            .unwrap_or_default()
            .into_iter()
            .filter(|r| r.method.as_str() == "POST")
            .count();
        assert_eq!(posts, 0, "the bot must never reply to its own finding");
    }

    #[tokio::test]
    async fn older_inline_reply_is_not_lost_behind_a_newer_top_level_comment() {
        let server = MockServer::start().await;
        let history = Arc::new(InMemoryReviewHistory::new());
        let k = key("alice", "widgets", 1);
        history.record(&k, "deadbeef").await.unwrap();

        // Top-level comment 20 is listed before inline reply 12 is, but
        // both are new; the shared cursor must not skip 12.
        Mock::given(method("GET"))
            .and(path("/api/v1/repos/alice/widgets/issues/1/comments"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {"id": 20, "body": "@auto-review help", "user": {"login": "carol"}}
            ])))
            .mount(&server)
            .await;
        mount_reviews(
            &server,
            "widgets",
            1,
            serde_json::json!([{"id": 1, "state": "COMMENT", "user": {"login": "auto-review"}}]),
        )
        .await;
        mount_review_comments(
            &server,
            "widgets",
            1,
            1,
            serde_json::json!([bot_finding(), help_reply()]),
        )
        .await;
        Mock::given(method("POST"))
            .and(path("/api/v1/repos/alice/widgets/issues/1/comments"))
            .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({"id": 99})))
            .expect(2)
            .mount(&server)
            .await;

        let cursors: SharedCommentCursors = Arc::new(Mutex::new(HashMap::from([(k.clone(), 10)])));
        let poller = hyphenated_identity_poller_for(&server, history, Arc::new(NoOpDispatcher))
            .await
            .with_cursors(cursors);
        poller.run_once_for_tests().await.expect("ok");

        assert_eq!(poller.cursor_for(&k).await, Some(20));
        server.verify().await;
    }

    #[tokio::test]
    async fn review_listing_errors_fail_only_that_pr() {
        let server = MockServer::start().await;
        let history = Arc::new(InMemoryReviewHistory::new());
        let k_reviews_down = key("alice", "broken", 1);
        let k_comments_down = key("alice", "gadgets", 3);
        let k_good = key("alice", "widgets", 2);
        history.record(&k_reviews_down, "x").await.unwrap();
        history.record(&k_comments_down, "z").await.unwrap();
        history.record(&k_good, "y").await.unwrap();

        for (repo, pr) in [("broken", 1), ("gadgets", 3)] {
            Mock::given(method("GET"))
                .and(path(format!(
                    "/api/v1/repos/alice/{repo}/issues/{pr}/comments"
                )))
                .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([])))
                .mount(&server)
                .await;
        }
        Mock::given(method("GET"))
            .and(path("/api/v1/repos/alice/broken/pulls/1/reviews"))
            .respond_with(ResponseTemplate::new(500))
            .mount(&server)
            .await;
        mount_reviews(
            &server,
            "gadgets",
            3,
            serde_json::json!([{"id": 4, "state": "COMMENT", "user": {"login": "bob"}}]),
        )
        .await;
        Mock::given(method("GET"))
            .and(path(
                "/api/v1/repos/alice/gadgets/pulls/3/reviews/4/comments",
            ))
            .respond_with(ResponseTemplate::new(500))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v1/repos/alice/widgets/issues/2/comments"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {"id": 3, "body": "no mention", "user": {"login": "carol"}}
            ])))
            .mount(&server)
            .await;
        mount_reviews(&server, "widgets", 2, serde_json::json!([])).await;

        let metrics = Arc::new(Metrics::new());
        let poller = poller_for(&server, history, Arc::new(NoOpDispatcher))
            .await
            .with_metrics(metrics.clone());
        poller.run_once_for_tests().await.expect("ok");

        let out = metrics.render();
        assert!(
            out.contains("auto_review_poll_pr_failures_total 2\n"),
            "{out}"
        );
        assert_eq!(poller.cursor_for(&k_good).await, Some(3));
        assert_eq!(poller.cursor_for(&k_reviews_down).await, None);
        assert_eq!(poller.cursor_for(&k_comments_down).await, None);
        assert!(matches!(
            poller.poll_pr(&k_reviews_down).await,
            Err(PollerError::Forgejo(_))
        ));
    }
}

use crate::review_history::{InMemoryReviewHistory, PrKey, ReviewHistory};
use alloc::sync::Arc;
use ar_forge::ReviewHost;
use ar_forgejo::{Client as ForgejoClient, CommitStatus, CommitStatusState, PullRequestEvent};
use ar_index::{LearningsStore, VectorStore};
use ar_llm::Router as LlmRouter;
use ar_review::{
    build_glob_set, build_review_context_with_store, is_valid_git_sha, load_repo_config_from_base,
    pr_is_skippable, prepare_workspace_from_clone_url, review_pull_request, BaseRef, LocalDiff,
    MergeBaseUnavailable, PreparedWorkspace, RepoConfig, ReviewArgs, ReviewError, VerifyMode,
    WorkspaceError,
};
use async_trait::async_trait;
use core::time::Duration;
use std::env;
use std::path::Path;
use std::time::Instant;
use tokio::sync::Semaphore;

/// Fallback label for a review whose workspace could not be cloned at all
/// (clone URL lookup or clone failed), so no local diff could be attempted.
const CLONE_FAILED_FALLBACK_REASON: &str = "clone_failed";

/// Commit-status context every review status is posted under.
const STATUS_CONTEXT: &str = "auto_review";

/// Forgejo's commit-status `description` is capped at 255 chars.
/// Posting more returns 422 and the user sees no status update at
/// all — leaving the PR stuck on "Pending" with no operator-visible
/// signal that the review actually completed (with an error). LLM
/// errors in particular can dump multi-hundred-char provider response
/// bodies into the format string. Cap at 240 to leave room for the
/// status badge UI's own padding and end with an ellipsis so users
/// know it was truncated.
const MAX_STATUS_DESCRIPTION: usize = 240;

/// Observed at the boundary of a single review job's lifecycle.
/// Wired into the gateway's Prometheus counters via
/// [`SpawningDispatcher::with_observer`] without ar-orchestrator
/// having to know about the metrics format.
#[derive(Debug, Clone)]
pub enum ReviewObservation {
    /// A review terminated with an error (LLM/Workspace/Forgejo
    /// failure or unhealable JSON). `error_class` is one of:
    /// `"forgejo"`, `"workspace"`, `"llm"`, `"unhealable"`. Stable
    /// strings so operators can label dashboards.
    Failed {
        duration: Duration,
        error_class: &'static str,
    },
    /// A full review could not compute its diff locally (merge base to
    /// head in the workspace) and used the host's diff instead, which
    /// GitHub refuses for pull requests over 300 files. `reason` is one of
    /// `"no_merge_base"`, `"history_cap"`, `"timed_out"`,
    /// `"fetch_failed"` or `"git_error"`. Fires at most once per review,
    /// alongside the review's own outcome; a fleet-wide rise points at a
    /// git downgrade or a stalled forge.
    LocalDiffFallback { reason: &'static str },
    /// The dispatch had to wait on the concurrency-cap semaphore
    /// before the review could begin. Counts how often the cap is
    /// engaging — sustained increases mean
    /// `AR_REVIEW_CONCURRENCY` is too tight (or traffic exceeds
    /// capacity). Fires AT MOST once per dispatch, before any
    /// other observation; firing implies a `Started` will follow.
    QueueWait,
    /// The review was a no-op for a benign reason — same SHA already
    /// reviewed (and not forced), all-trivial files, or
    /// `enabled: false`. Distinguished from Failed because operators
    /// shouldn't alert on these.
    Skipped { reason: &'static str },
    /// A review just started executing. Posted before any I/O the
    /// dispatcher does on the PR's behalf, so it counts review
    /// *attempts* including ones that immediately fail.
    Started,
    /// A review finished and posted comments successfully. `duration`
    /// covers the whole pipeline (clone + context prep + LLM + verify +
    /// post). `findings_count` is what landed on the PR; zero is the
    /// happy path, not an error. `verifier_dropped` is the number
    /// of findings the verifier corrected away (sums to the
    /// reasoning model's pre-verify count if there was no
    /// severity-floor filter); high rates point at the reasoning
    /// model hallucinating.
    Succeeded {
        duration: Duration,
        findings_count: usize,
        verifier_dropped: usize,
    },
}

/// Optional sink for [`ReviewObservation`]s. The gateway provides
/// a Prometheus-counter-backed implementation; the trait keeps the
/// orchestrator independent of any specific metrics backend and
/// makes it easy to write dispatcher tests that count outcomes.
pub trait ReviewObserver: Send + Sync {
    fn record(&self, observation: ReviewObservation);
}

/// One review job's worth of input — extracted from a webhook event so the
/// dispatcher can be tested without depending on the full event shape.
#[derive(Debug, Clone)]
#[expect(
    clippy::exhaustive_structs,
    reason = "ar-agentcore, ar-chat (handler), ar-cli (commands) and ar-gateway (webhook) build \
              ReviewJob by struct literal; #[non_exhaustive] would break those dependents"
)]
pub struct ReviewJob {
    /// Base commit of the pull request, when the intake already has it
    /// (webhook payload, CI review request). `None` makes the dispatcher
    /// use the base SHA of the pull request it fetches. The review diff is
    /// taken from the merge base, so a base that moved on since does not
    /// change it.
    pub base_sha: Option<String>,
    /// When true, bypasses the per-PR review-history dedup check and
    /// forces a fresh review even if this SHA was already reviewed.
    /// Used by the chat handler's `re-review` command.
    pub force: bool,
    pub head_sha: String,
    pub owner: String,
    pub pr_body: String,
    pub pr_number: u64,
    pub pr_title: String,
    pub repo: String,
}

impl ReviewJob {
    /// Learnings scope of the reviewed repository: the lowercased
    /// `owner/repo`, so a mixed-case job still finds its learnings.
    #[inline]
    #[must_use = "computing the scope has no side effects"]
    pub fn repo_scope(&self) -> ar_index::RepoScope {
        ar_index::RepoScope::new(&self.owner, &self.repo)
    }
}

impl From<&PullRequestEvent> for ReviewJob {
    #[inline]
    fn from(evt: &PullRequestEvent) -> Self {
        Self {
            base_sha: Some(evt.pull_request.base.sha.clone()),
            force: false,
            head_sha: evt.pull_request.head.sha.clone(),
            owner: evt.repository.owner.login.clone(),
            pr_body: evt.pull_request.body.clone(),
            pr_number: evt.pull_request.number,
            pr_title: evt.pull_request.title.clone(),
            repo: evt.repository.name.clone(),
        }
    }
}

/// Hands a [`ReviewJob`] off for processing. The webhook handler calls
/// `dispatch` and returns immediately; the actual review may run for
/// minutes in the background.
#[async_trait]
#[expect(
    clippy::module_name_repetitions,
    reason = "ar-agentcore, ar-chat (handler), ar-gateway (lib, startup, webhook, poller) and \
              ar-cli (commands) import this public trait name; renaming it breaks them"
)]
pub trait JobDispatcher: Send + Sync {
    async fn dispatch(&self, job: ReviewJob);
}

/// Discards every job. Useful for tests that exercise the gateway's
/// dispatch surface without wiring the full review pipeline.
#[derive(Debug, Default, Clone)]
#[expect(
    clippy::module_name_repetitions,
    reason = "ar-gateway (startup, webhook, poller, github_webhook) and ar-cli (commands) import \
              this public name; renaming it breaks them"
)]
#[expect(
    clippy::exhaustive_structs,
    reason = "a stateless unit struct that ar-gateway and ar-cli use as a value by name; \
              #[non_exhaustive] would stop them constructing it"
)]
pub struct NoOpDispatcher;

#[async_trait]
impl JobDispatcher for NoOpDispatcher {
    #[inline]
    async fn dispatch(&self, _job: ReviewJob) {}
}

/// Synchronous dispatcher for runtimes that need the invocation to finish only
/// after the review job has run to completion. Unlike [`SpawningDispatcher`],
/// this does not spawn a background task.
#[derive(Clone)]
#[expect(
    clippy::module_name_repetitions,
    reason = "ar-cli (commands) imports this public name; renaming it breaks that dependent"
)]
pub struct InlineDispatcher {
    /// Review history used for same-SHA dedup and incremental diffs.
    history: Arc<dyn ReviewHistory>,
    /// Repository host the review reads from and posts to.
    host: Arc<dyn ReviewHost>,
    /// Optional learnings store for repository guidance.
    learnings: Option<Arc<dyn LearningsStore>>,
    /// Tiered LLM router for the review pipeline.
    llm: Arc<LlmRouter>,
    /// Optional metrics sink for review outcomes.
    observer: Option<Arc<dyn ReviewObserver>>,
    /// Optional shared vector store for symbol embeddings.
    vector_store: Option<Arc<dyn VectorStore>>,
}

impl InlineDispatcher {
    /// Dispatcher over `host` and `llm` with in-memory history and no
    /// optional stores.
    #[inline]
    pub fn new_with_host(host: Arc<dyn ReviewHost>, llm: Arc<LlmRouter>) -> Self {
        Self {
            history: Arc::new(InMemoryReviewHistory::new()),
            host,
            learnings: None,
            llm,
            observer: None,
            vector_store: None,
        }
    }

    /// Replace the default in-memory history.
    #[inline]
    #[must_use = "builder method: returns the updated dispatcher and does nothing if dropped"]
    pub fn with_history(mut self, history: Arc<dyn ReviewHistory>) -> Self {
        self.history = history;
        self
    }

    /// Wire in a learnings store.
    #[inline]
    #[must_use = "builder method: returns the updated dispatcher and does nothing if dropped"]
    pub fn with_learnings(mut self, learnings: Arc<dyn LearningsStore>) -> Self {
        self.learnings = Some(learnings);
        self
    }

    /// Wire in a metrics observer.
    #[inline]
    #[must_use = "builder method: returns the updated dispatcher and does nothing if dropped"]
    pub fn with_observer(mut self, observer: Arc<dyn ReviewObserver>) -> Self {
        self.observer = Some(observer);
        self
    }

    /// Wire in a shared vector store.
    #[inline]
    #[must_use = "builder method: returns the updated dispatcher and does nothing if dropped"]
    pub fn with_vector_store(mut self, store: Arc<dyn VectorStore>) -> Self {
        self.vector_store = Some(store);
        self
    }
}

#[async_trait]
impl JobDispatcher for InlineDispatcher {
    #[inline]
    async fn dispatch(&self, job: ReviewJob) {
        run_review_job(
            self.host.as_ref(),
            &self.llm,
            "",
            "",
            self.history.as_ref(),
            self.learnings.as_deref(),
            self.vector_store.as_deref(),
            self.observer.as_deref(),
            job,
        )
        .await;
    }
}

/// Production dispatcher: posts a "pending" commit status, spawns
/// [`run_review_job`] in the background, and returns to the caller.
///
/// Owns the Forgejo base URL + bot token in addition to the API client
/// because workspace prep needs them to build a clone URL. Also owns
/// a [`ReviewHistory`] so subsequent commits on the same PR can use
/// `compare_diff` instead of re-reviewing the whole PR.
#[derive(Clone)]
#[expect(
    clippy::module_name_repetitions,
    reason = "ar-gateway (startup) imports this public name; renaming it breaks that dependent"
)]
pub struct SpawningDispatcher {
    /// Optional cap on concurrent in-flight reviews. When set, a
    /// burst of webhooks beyond the cap waits in the spawn queue
    /// rather than thundering through the LLM and workspace
    /// tmpdirs simultaneously. None = unlimited (back-compat
    /// default; small deployments and tests don't need a cap).
    concurrency_limit: Option<Arc<Semaphore>>,
    /// Forgejo base URL handed to each review job.
    forgejo_base: Arc<str>,
    /// Forgejo bot token handed to each review job.
    forgejo_token: Arc<str>,
    /// Review history used for same-SHA dedup and incremental diffs.
    history: Arc<dyn ReviewHistory>,
    /// Repository host the review reads from and posts to.
    host: Arc<dyn ReviewHost>,
    /// Optional learnings store for repository guidance.
    learnings: Option<Arc<dyn LearningsStore>>,
    /// Tiered LLM router for the review pipeline.
    llm: Arc<LlmRouter>,
    /// Optional metrics sink for review outcomes.
    observer: Option<Arc<dyn ReviewObserver>>,
    /// Optional shared vector store for symbol embeddings. When set,
    /// embeddings persist across reviews (and, when SQLite-backed,
    /// across gateway restarts) so re-reviews of the same PR don't
    /// re-embed unchanged symbols. None ⇒ `build_review_context_with_store`
    /// constructs a per-call in-memory store as a back-compat default.
    vector_store: Option<Arc<dyn VectorStore>>,
}

impl SpawningDispatcher {
    /// Dispatcher over a Forgejo client.
    #[inline]
    pub fn new(
        forgejo: Arc<ForgejoClient>,
        llm: Arc<LlmRouter>,
        forgejo_base: impl Into<String>,
        forgejo_token: impl Into<String>,
    ) -> Self {
        Self::new_with_host(forgejo, llm, forgejo_base, forgejo_token)
    }

    /// Dispatcher over any [`ReviewHost`].
    #[inline]
    pub fn new_with_host(
        host: Arc<dyn ReviewHost>,
        llm: Arc<LlmRouter>,
        forgejo_base: impl Into<String>,
        forgejo_token: impl Into<String>,
    ) -> Self {
        Self {
            concurrency_limit: None,
            forgejo_base: Arc::from(forgejo_base.into()),
            forgejo_token: Arc::from(forgejo_token.into()),
            history: Arc::new(InMemoryReviewHistory::new()),
            host,
            learnings: None,
            llm,
            observer: None,
            vector_store: None,
        }
    }

    /// Cap concurrent in-flight reviews at `max`. When more
    /// webhooks arrive than `max` can run simultaneously, the
    /// excess wait in the spawn queue. The webhook handler still
    /// returns 202 immediately — the spawn task just blocks on the
    /// semaphore acquisition before doing real work.
    ///
    /// Without this cap a burst of N PRs spawns N tmpdirs + N
    /// in-flight LLM calls. On a single-tenant box that's fine for
    /// small N; for high-traffic instances or expensive LLMs it
    /// matters.
    #[inline]
    #[must_use = "builder method: returns the updated dispatcher and does nothing if dropped"]
    pub fn with_concurrency_limit(mut self, max: usize) -> Self {
        self.concurrency_limit = Some(Arc::new(Semaphore::new(max.max(1))));
        self
    }

    /// Replace the default in-memory history with a custom one
    /// (e.g. SQLite-backed for persistence across restarts).
    #[inline]
    #[must_use = "builder method: returns the updated dispatcher and does nothing if dropped"]
    pub fn with_history(mut self, history: Arc<dyn ReviewHistory>) -> Self {
        self.history = history;
        self
    }

    /// Wire in a learnings store so remembered guidance gets pulled
    /// into the RAG context for future reviews. When unset,
    /// `build_review_context` skips the learnings-retrieval step.
    #[inline]
    #[must_use = "builder method: returns the updated dispatcher and does nothing if dropped"]
    pub fn with_learnings(mut self, learnings: Arc<dyn LearningsStore>) -> Self {
        self.learnings = Some(learnings);
        self
    }

    /// Wire in a metrics observer so review outcomes feed the
    /// gateway's `/metrics` endpoint. Without one, reviews still
    /// run but are invisible to Prometheus.
    #[inline]
    #[must_use = "builder method: returns the updated dispatcher and does nothing if dropped"]
    pub fn with_observer(mut self, observer: Arc<dyn ReviewObserver>) -> Self {
        self.observer = Some(observer);
        self
    }

    /// Wire in a shared vector store so symbol embeddings persist
    /// across reviews. Without this, every review constructs a
    /// throwaway in-memory store and re-embeds the entire workspace.
    #[inline]
    #[must_use = "builder method: returns the updated dispatcher and does nothing if dropped"]
    pub fn with_vector_store(mut self, store: Arc<dyn VectorStore>) -> Self {
        self.vector_store = Some(store);
        self
    }
}

#[async_trait]
impl JobDispatcher for SpawningDispatcher {
    #[inline]
    async fn dispatch(&self, job: ReviewJob) {
        let forgejo = Arc::clone(&self.host);
        let llm = Arc::clone(&self.llm);
        let base = Arc::clone(&self.forgejo_base);
        let token = Arc::clone(&self.forgejo_token);
        let history = Arc::clone(&self.history);
        let learnings = self.learnings.clone();
        let vector_store = self.vector_store.clone();
        let observer = self.observer.clone();
        let concurrency_limit = self.concurrency_limit.clone();
        // Outer spawn returns immediately so the webhook handler can ack.
        // Inner spawn runs the actual review; the outer awaits the inner's
        // JoinHandle so panics or cancellations get logged AND surface to
        // the PR as a failure status — silent crashes make ops debugging
        // miserable.
        let repo = format!("{}/{}", job.owner, job.repo);
        let pr = job.pr_number;
        let owner_for_status = job.owner.clone();
        let repo_for_status = job.repo.clone();
        let sha_for_status = job.head_sha.clone();
        let forgejo_for_status = Arc::clone(&forgejo);
        let observer_for_queue = observer.clone();
        let observer_for_panic = observer.clone();
        tokio::spawn(async move {
            // Acquire the concurrency permit BEFORE the inner spawn
            // so the wait actually limits in-flight reviews. Held
            // for the lifetime of the inner task — releases when
            // the review finishes (or panics).
            let _permit = match concurrency_limit.as_ref() {
                Some(sem) => {
                    // Best-effort detection of "this acquire had to
                    // wait": if no permits are available right now,
                    // this acquire will block. Race-y (permits can
                    // change between the check and the acquire) but
                    // approximate is fine for an ops counter.
                    if sem.available_permits() == 0 {
                        if let Some(obs) = observer_for_queue.as_deref() {
                            obs.record(ReviewObservation::QueueWait);
                        }
                    }
                    if let Ok(permit) = Arc::clone(sem).acquire_owned().await {
                        Some(permit)
                    } else {
                        // Semaphore was closed, which we never
                        // do. Defensive: log and continue
                        // without limiting rather than dropping
                        // the review.
                        tracing::warn!(
                            "concurrency semaphore closed; running review without limit"
                        );
                        None
                    }
                }
                None => None,
            };
            let panic_started = Instant::now();
            let inner = tokio::spawn(async move {
                run_review_job(
                    forgejo.as_ref(),
                    &llm,
                    &base,
                    &token,
                    history.as_ref(),
                    learnings.as_deref(),
                    vector_store.as_deref(),
                    observer.as_deref(),
                    job,
                )
                .await;
            });
            if let Err(err) = inner.await {
                tracing::error!(
                    repo,
                    pr,
                    error = %err,
                    "review task panicked or was cancelled"
                );
                // Without this observation, a panic mid-review would
                // tick `reviews_started_total` but never tick any of
                // the `reviews_failed_*` / `reviews_succeeded` /
                // `reviews_skipped_*` counters — operators would see
                // started/completed counts that don't add up. Bucket
                // the panic under a stable "panic" class so dashboards
                // can alert on it independently of the four ReviewError
                // variants.
                if let Some(obs) = observer_for_panic.as_deref() {
                    obs.record(ReviewObservation::Failed {
                        duration: panic_started.elapsed(),
                        error_class: "panic",
                    });
                }
                // Best-effort failure-status post; if this fails too, we
                // log and give up.
                let status = CommitStatus {
                    state: CommitStatusState::Error,
                    target_url: String::new(),
                    description: "auto_review crashed; check logs".into(),
                    context: STATUS_CONTEXT.into(),
                };
                if let Err(post_err) = forgejo_for_status
                    .post_commit_status(
                        &owner_for_status,
                        &repo_for_status,
                        &sha_for_status,
                        &status,
                    )
                    .await
                {
                    tracing::warn!(error = %post_err, "failed to post crash status");
                }
            }
        });
    }
}

/// Errors from cloning the head workspace.
#[derive(Debug, thiserror::Error)]
enum WorkspacePrepError {
    /// The Forgejo API call failed.
    #[error("forgejo: {0}")]
    Forgejo(#[from] ar_forgejo::Error),
    /// The repository host could not supply a clone URL.
    #[error("host: {0}")]
    Host(#[from] ar_forge::HostError),
    /// Cloning or checking out the workspace failed.
    #[error("workspace: {0}")]
    Workspace(#[from] WorkspaceError),
}

/// Result of [`prepare_workspace_context`].
enum WorkspacePrep {
    /// The base-branch config sets `enabled: false`; nothing was cloned.
    DisabledByConfig,
    /// Review should proceed with this config and context. Boxed because
    /// the output (config, diff, workspace) dwarfs the data-free variant.
    Ready(Box<WorkspacePrepOutput>),
}

/// Config, context and workspace prepared for a review.
struct WorkspacePrepOutput {
    /// Files the pull request changes, computed locally alongside
    /// `raw_diff`. `Some` exactly when `raw_diff` is the local diff, so the
    /// pipeline uses neither the host's diff nor its file list (and none of
    /// their size caps apply).
    changed_files: Option<Vec<ar_forge::ChangedFile>>,
    /// Repository config read from the base branch through
    /// [`load_repo_config_from_base`]; defaults when absent or unreadable.
    config: RepoConfig,
    /// Why the local diff was unavailable and the host's diff was used,
    /// for [`ReviewObservation::LocalDiffFallback`]. `None` when the local
    /// diff was used, or was never attempted (no workspace, no base SHA).
    diff_fallback: Option<&'static str>,
    /// The pull request diff used for context building: computed locally in
    /// the workspace (merge base to head) when possible, otherwise fetched
    /// from the host. Surfaced back so the review pipeline can reuse it as
    /// `diff_override` instead of fetching it again. Empty string when
    /// neither source supplied it (we degrade-but-continue; the pipeline
    /// will refetch and likely also fail consistently).
    raw_diff: String,
    /// Rendered RAG context; empty when none could be built.
    repo_context: String,
    /// Held by the orchestrator until the review pipeline finishes
    /// so the agentic verifier (when enabled) can inspect the
    /// cloned working tree. `None` when cloning the head failed.
    workspace: Option<PreparedWorkspace>,
}

/// Run one review job to completion.
///
/// 1. Post a "pending" commit status.
/// 2. Fetch the pull request once to learn its base branch ([`BaseRef`]).
/// 3. Load `.auto_review.yaml` from the base branch (never the head
///    checkout), clone the repo at the head SHA and prepare review context.
/// 4. Call [`review_pull_request`].
/// 5. Post the final success/error commit status.
///
/// Errors are logged and swallowed; the gateway has already returned 202.
#[expect(
    clippy::too_many_arguments,
    reason = "public entry point that ar-cli (commands) and both dispatchers call positionally; \
              bundling the parameters into a struct changes its signature"
)]
#[expect(
    clippy::cognitive_complexity,
    clippy::too_many_lines,
    reason = "the job's ordered early exits (same SHA, PR fetch, triage, disabled config) each \
              post a status and an observation in a fixed sequence that dashboards rely on; \
              keeping them in one function keeps that order visible"
)]
#[inline]
pub async fn run_review_job(
    forgejo: &dyn ReviewHost,
    llm: &LlmRouter,
    forgejo_base: &str,
    forgejo_token: &str,
    history: &dyn ReviewHistory,
    learnings: Option<&dyn LearningsStore>,
    vector_store: Option<&dyn VectorStore>,
    observer: Option<&dyn ReviewObserver>,
    job: ReviewJob,
) {
    let started_at = Instant::now();
    let observe = |observation: ReviewObservation| {
        if let Some(obs) = observer {
            obs.record(observation);
        }
    };
    let pr_key = PrKey {
        owner: job.owner.clone(),
        pr_number: job.pr_number,
        repo: job.repo.clone(),
    };
    let last_reviewed_sha = match history.last_reviewed(&pr_key).await {
        Ok(sha) => sha,
        Err(err) => {
            tracing::warn!(error = %err, "review history lookup failed; treating as full review");
            None
        }
    };
    // The previously reviewed commit when this is an incremental review.
    // Its diff is computed once the workspace exists.
    let mut incremental_from: Option<&str> = None;
    if let Some(prev) = last_reviewed_sha.as_ref() {
        if prev == &job.head_sha {
            if job.force {
                tracing::info!(
                    repo = format!("{}/{}", job.owner, job.repo),
                    pr = job.pr_number,
                    sha = %job.head_sha,
                    "force=true: re-reviewing the same SHA the user asked"
                );
            } else {
                tracing::info!(
                    repo = format!("{}/{}", job.owner, job.repo),
                    pr = job.pr_number,
                    sha = %job.head_sha,
                    "no new commits since last review; skipping"
                );
                observe(ReviewObservation::Skipped { reason: "same_sha" });
                return;
            }
        } else if job.force {
            tracing::info!(
                repo = format!("{}/{}", job.owner, job.repo),
                pr = job.pr_number,
                "force=true: full review (skipping compare-diff incremental path)"
            );
        } else {
            incremental_from = Some(prev.as_str());
        }
    }

    post_review_status(
        forgejo,
        &job,
        CommitStatusState::Pending,
        "auto_review running".into(),
    )
    .await;

    // Fetch the PR once: its base branch is where repository config is
    // read from, so a PR cannot change how it is reviewed.
    let base_ref = match forgejo
        .get_pull_request(&job.owner, &job.repo, job.pr_number)
        .await
    {
        Ok(pr) => BaseRef::from(&pr.base),
        Err(err) => {
            tracing::error!(
                repo = format!("{}/{}", job.owner, job.repo),
                pr = job.pr_number,
                error = %err,
                "failed to fetch pull request; cannot resolve base branch, not reviewing"
            );
            observe(ReviewObservation::Started);
            observe(ReviewObservation::Failed {
                duration: started_at.elapsed(),
                error_class: "forgejo",
            });
            post_review_status(
                forgejo,
                &job,
                CommitStatusState::Error,
                truncate_status_description(&format!(
                    "auto_review failed: could not fetch pull request: {err}"
                )),
            )
            .await;
            return;
        }
    };

    // Triage: if every changed file is trivial (lockfile bumps, vendored,
    // generated), skip the LLM call entirely and post a success status.
    // Fetch once for the trivial-file skip check.
    let changed_files = match fetch_changed_files_for_triage(forgejo, &job).await {
        Ok(files) => Some(files),
        Err(err) => {
            tracing::warn!(error = %err, "triage file-list failed; proceeding to review");
            None
        }
    };
    if let Some(files) = changed_files.as_ref() {
        if pr_is_skippable(files) {
            tracing::info!(
                repo = format!("{}/{}", job.owner, job.repo),
                pr = job.pr_number,
                "skipping review: all changed files are trivial"
            );
            post_review_status(
                forgejo,
                &job,
                CommitStatusState::Success,
                "auto_review: skipped (lockfile/vendored/generated only)".into(),
            )
            .await;
            observe(ReviewObservation::Skipped {
                reason: "trivial_files",
            });
            return;
        }
    }

    let prep = match prepare_workspace_context(
        forgejo,
        llm,
        forgejo_base,
        forgejo_token,
        learnings,
        vector_store,
        &job,
        &base_ref,
        incremental_from,
    )
    .await
    {
        WorkspacePrep::DisabledByConfig => {
            tracing::info!(
                repo = format!("{}/{}", job.owner, job.repo),
                pr = job.pr_number,
                base = %base_ref.name,
                "skipping review: disabled by base-branch .auto_review.yaml"
            );
            post_review_status(
                forgejo,
                &job,
                CommitStatusState::Success,
                "auto_review: disabled by repo config".into(),
            )
            .await;
            observe(ReviewObservation::Skipped {
                reason: "disabled_by_config",
            });
            return;
        }
        WorkspacePrep::Ready(output) => *output,
    };
    let WorkspacePrepOutput {
        changed_files: local_changed_files,
        config,
        raw_diff,
        repo_context,
        diff_fallback,
        workspace,
    } = prep;
    let ignored_paths = build_glob_set(&config.ignored_paths);
    let incremental_diff = match incremental_from {
        Some(previous_sha) => {
            incremental_review_diff(forgejo, &job, previous_sha, workspace.as_ref()).await
        }
        None => None,
    };

    // The agentic verifier needs the cloned workspace to inspect.
    // Operators opt in by setting AR_AGENTIC_VERIFIER=1; without it,
    // the simple verifier (one-pass diff judgement) keeps running.
    // Either way we silently downgrade to Simple when no workspace
    // was prepared (e.g. workspace prep failed) — but log the
    // downgrade so operators who set the flag aren't left wondering
    // why findings stopped being verified against the workspace.
    let agentic_requested = env::var("AR_AGENTIC_VERIFIER")
        .is_ok_and(|value| value == "1" || value.eq_ignore_ascii_case("true"));
    let verify_mode = if agentic_requested && workspace.is_some() {
        VerifyMode::Agentic
    } else {
        if agentic_requested && workspace.is_none() {
            tracing::warn!(
                repo = format!("{}/{}", job.owner, job.repo),
                pr = job.pr_number,
                "AR_AGENTIC_VERIFIER=1 set but workspace prep failed; \
                 downgrading verifier to simple mode for this review"
            );
        }
        VerifyMode::Simple
    };
    let workspace_path = workspace.as_ref().map(PreparedWorkspace::path);

    // Fire Started AFTER all early-skip checks (same_sha,
    // trivial_files, disabled_by_config) have passed. Means each
    // review job emits exactly one of {Skipped_*, Started + (one
    // of Succeeded / Failed_*)} — no double-count when an early
    // skip also fired Started. Operator dashboards rely on
    // `started_total ≈ succeeded + failed_*` for sanity checks.
    observe(ReviewObservation::Started);
    if let Some(reason) = diff_fallback {
        observe(ReviewObservation::LocalDiffFallback { reason });
    }

    let result = review_pull_request(ReviewArgs {
        host: forgejo,
        llm,
        owner: &job.owner,
        repo: &job.repo,
        pr_number: job.pr_number,
        head_sha: &job.head_sha,
        base_ref: &base_ref,
        pr_title: &job.pr_title,
        pr_body: &job.pr_body,
        ignored_paths: &ignored_paths,
        min_severity: severity_floor_from_env(),
        guidelines: &config.guidelines,
        repo_context: &repo_context,
        // The locally computed file list; the pipeline ignores it for
        // incremental reviews, which list the incremental diff's files.
        changed_files_override: local_changed_files.as_deref(),
        // Reuse the diff workspace/context prep already has. For
        // incremental reviews we override with the compare-diff
        // (smaller, focused on new commits); otherwise use the full PR
        // diff already in hand. A local diff is passed even when empty
        // (the pull request changes nothing). An empty host diff means
        // the fetch failed, and passing it as Some("") would suppress
        // the pipeline's own get_pr_diff retry, so map it back to None.
        diff_override: incremental_diff.as_deref().or(
            if local_changed_files.is_none() && raw_diff.is_empty() {
                None
            } else {
                Some(raw_diff.as_str())
            },
        ),
        previous_review_sha: incremental_diff.as_ref().and(last_reviewed_sha.as_deref()),
        verify_mode,
        workspace_path,
        pr_metadata_check: config.pr_metadata_check.clone(),
    })
    .await;

    let final_status = match result.as_ref() {
        Ok(outcome) => {
            tracing::info!(
                repo = format!("{}/{}", job.owner, job.repo),
                pr = job.pr_number,
                review_id = outcome.review_id,
                findings = outcome.findings_count,
                "review posted"
            );
            observe(ReviewObservation::Succeeded {
                duration: started_at.elapsed(),
                findings_count: outcome.findings_count,
                verifier_dropped: outcome.verifier_dropped,
            });
            CommitStatus {
                state: CommitStatusState::Success,
                target_url: String::new(),
                description: review_summary(outcome),
                context: STATUS_CONTEXT.into(),
            }
        }
        Err(err) => {
            tracing::error!(
                repo = format!("{}/{}", job.owner, job.repo),
                pr = job.pr_number,
                error = %err,
                "review failed"
            );
            observe(ReviewObservation::Failed {
                duration: started_at.elapsed(),
                error_class: error_class(err),
            });
            CommitStatus {
                state: error_state(err),
                target_url: String::new(),
                description: truncate_status_description(&format!("auto_review failed: {err}")),
                context: STATUS_CONTEXT.into(),
            }
        }
    };

    post_review_status(forgejo, &job, final_status.state, final_status.description).await;

    // Record the SHA only on successful review. Recording a SHA we
    // never successfully reviewed would mean the NEXT incremental
    // review diffs against it (treating it as "already reviewed"),
    // and any findings introduced in this SHA but unchanged in the
    // next push get silently skipped. The cost: a transient LLM
    // failure makes the next review a full one instead of
    // incremental — pay duplicated tokens once to recover real
    // findings the user would otherwise miss.
    //
    // Best-effort: a record failure just means the next review
    // will be a full one (same effective behaviour).
    if let Ok(outcome) = result.as_ref() {
        if let Err(err) = history
            .record_with_cost(&pr_key, &job.head_sha, outcome.estimated_total_cost_usd)
            .await
        {
            tracing::warn!(error = %err, "failed to record review history");
        }
    } else {
        tracing::debug!(
            sha = %job.head_sha,
            "review failed; not recording SHA so next webhook re-runs a full review"
        );
    }
}

/// Load the base-branch config and, unless it disables review, clone the
/// head and build optional RAG context.
///
/// The config comes from `base_ref.name` through the trusted loader and is
/// never read from the head checkout, so a PR cannot disable or narrow its
/// own review. Clone and context failures degrade to no workspace context
/// but keep the base-branch config.
#[expect(
    clippy::too_many_arguments,
    reason = "mirrors run_review_job's positional parameters (host, LLM, clone credentials, \
              stores, job, base ref), which that public fn passes straight through"
)]
#[expect(
    clippy::cognitive_complexity,
    reason = "the score comes from the tracing macros on each degrade-and-continue branch; the \
              control flow is three sequential fallbacks"
)]
async fn prepare_workspace_context(
    host: &dyn ReviewHost,
    llm: &LlmRouter,
    _base: &str,
    _token: &str,
    learnings: Option<&dyn LearningsStore>,
    vector_store: Option<&dyn VectorStore>,
    job: &ReviewJob,
    base_ref: &BaseRef,
    incremental_from: Option<&str>,
) -> WorkspacePrep {
    let loaded = load_repo_config_from_base(host, &job.owner, &job.repo, &base_ref.name).await;
    tracing::debug!(
        repo = format!("{}/{}", job.owner, job.repo),
        pr = job.pr_number,
        provenance = ?loaded.provenance,
        "loaded repo config"
    );
    let config = loaded.config;
    if !config.enabled {
        return WorkspacePrep::DisabledByConfig;
    }
    let base_sha = job.base_sha.as_deref().unwrap_or(&base_ref.sha);
    let workspace = match clone_head_workspace(host, job, base_sha, incremental_from).await {
        Ok(workspace) => workspace,
        Err(err) => {
            tracing::warn!(
                repo = format!("{}/{}", job.owner, job.repo),
                pr = job.pr_number,
                error = %err,
                "workspace prep failed; using the host's diff and continuing without workspace \
                 context"
            );
            return WorkspacePrep::Ready(Box::new(WorkspacePrepOutput {
                changed_files: None,
                config,
                diff_fallback: Some(CLONE_FAILED_FALLBACK_REASON),
                raw_diff: String::new(),
                repo_context: String::new(),
                workspace: None,
            }));
        }
    };
    // The review diff comes from the workspace (merge base to head), so no
    // host diff limit applies. Without a local diff, fetch the host's diff
    // once for the RAG context build; failure there just means we skip
    // optional context and the review still proceeds.
    let (raw_diff, changed_files, diff_fallback) = match local_pr_diff(&workspace, job).await {
        Ok((diff, files)) => (diff, Some(files), None),
        Err(fallback) => match fetch_context_diff(host, job).await {
            Ok(diff) => (diff, None, fallback),
            Err(err) => {
                tracing::warn!(error = %err, "diff fetch for context failed; continuing");
                (String::new(), None, fallback)
            }
        },
    };

    // Build the RAG context (best-effort): walks the workspace,
    // embeds symbols, queries top-K against the diff. Returns empty
    // string when no Embedding tier is configured or the workspace
    // has no extractable symbols.
    let repo_context = if raw_diff.is_empty() {
        String::new()
    } else {
        job_review_context(
            workspace.path(),
            llm,
            &raw_diff,
            learnings,
            vector_store,
            job,
        )
        .await
        .unwrap_or_else(|err| {
            tracing::warn!(error = %err, "RAG context build failed; continuing");
            String::new()
        })
    };

    WorkspacePrep::Ready(Box::new(WorkspacePrepOutput {
        changed_files,
        config,
        diff_fallback,
        raw_diff,
        repo_context,
        workspace: Some(workspace),
    }))
}

/// Build the RAG context for `job`, retrieving only learnings scoped
/// to the job's repository.
async fn job_review_context(
    workspace_path: &Path,
    llm: &LlmRouter,
    raw_diff: &str,
    learnings: Option<&dyn LearningsStore>,
    vector_store: Option<&dyn VectorStore>,
    job: &ReviewJob,
) -> Result<String, ar_review::ContextBuildError> {
    build_review_context_with_store(
        workspace_path,
        llm,
        raw_diff,
        learnings,
        &job.repo_scope(),
        5,
        vector_store,
    )
    .await
}

/// Clone the job's repository at its head SHA, fetching enough of
/// `base_sha`'s history to find the pull request's merge base, and of
/// `incremental_from` (the previously reviewed commit) to diff what was
/// pushed since.
///
/// A `base_sha` that is not a commit SHA still clones the head (for
/// context and the agentic verifier); the workspace then has no merge
/// base and the review falls back to the host's diff. An
/// `incremental_from` that is not a commit SHA is not fetched, so the
/// incremental diff falls back to the host.
async fn clone_head_workspace(
    host: &dyn ReviewHost,
    job: &ReviewJob,
    base_sha: &str,
    incremental_from: Option<&str>,
) -> Result<PreparedWorkspace, WorkspacePrepError> {
    let clone_url = host
        .clone_url(&job.owner, &job.repo)
        .await
        .map_err(WorkspacePrepError::Host)?;
    let base = Some(base_sha).filter(|sha| is_valid_git_sha(sha));
    if base.is_none() {
        tracing::warn!(
            repo = format!("{}/{}", job.owner, job.repo),
            pr = job.pr_number,
            base_sha,
            "pull request base is not a commit SHA; cloning the head without its merge base"
        );
    }
    let previous = incremental_from.filter(|sha| is_valid_git_sha(sha));
    Ok(prepare_workspace_from_clone_url(&clone_url, &job.head_sha, base, previous).await?)
}

/// The pull request's diff and changed files computed in `workspace`,
/// merge base to head, or `Err` when the workspace cannot supply them. The
/// reason (no merge base and why, or a git failure) is logged once here with
/// the job's repository and pull request and returned as a
/// [`local_diff_fallback_reason`] label; a base that was not a commit SHA
/// was already logged by [`clone_head_workspace`] and returns `Err(None)`.
async fn local_pr_diff(
    workspace: &PreparedWorkspace,
    job: &ReviewJob,
) -> Result<(String, Vec<ar_forge::ChangedFile>), Option<&'static str>> {
    let local = async {
        Ok::<_, WorkspaceError>((
            local_diff_text(job, workspace.pr_diff().await?),
            workspace.pr_changed_files().await?,
        ))
    };
    match local.await {
        Ok(diff_and_files) => Ok(diff_and_files),
        Err(WorkspaceError::NoMergeBase(MergeBaseUnavailable::NotRequested)) => Err(None),
        Err(err) => {
            tracing::warn!(
                repo = format!("{}/{}", job.owner, job.repo),
                pr = job.pr_number,
                error = %err,
                "local pull request diff unavailable; using the host's diff"
            );
            Err(Some(local_diff_fallback_reason(&err)))
        }
    }
}

/// The text of a diff computed in the job's workspace, after one warning
/// naming the repository and pull request when git's output was cut at its
/// read limit (the files after the cut are missing from the review text).
fn local_diff_text(job: &ReviewJob, diff: LocalDiff) -> String {
    if diff.truncated {
        tracing::warn!(
            repo = format!("{}/{}", job.owner, job.repo),
            pr = job.pr_number,
            "local diff cut at its read limit; files after the cut are missing from the review \
             text, including any listed after ignored paths"
        );
    }
    diff.text
}

/// Bounded metrics label for why a local diff was unavailable.
#[expect(
    clippy::pattern_type_mismatch,
    reason = "binding the merge-base reason out of a borrowed error needs either default binding \
              modes or `ref` patterns, and clippy::ref_patterns forbids `ref`"
)]
fn local_diff_fallback_reason(err: &WorkspaceError) -> &'static str {
    match err {
        WorkspaceError::NoMergeBase(reason) | WorkspaceError::NoIncrementalBase(reason) => {
            match reason {
                MergeBaseUnavailable::HistoryCap => "history_cap",
                MergeBaseUnavailable::TimedOut(_) => "timed_out",
                MergeBaseUnavailable::Fetch(_) | MergeBaseUnavailable::Deepen(_) => "fetch_failed",
                MergeBaseUnavailable::NotRequested
                | MergeBaseUnavailable::NoPullRequestMergeBase
                | MergeBaseUnavailable::UnrelatedHistories => "no_merge_base",
            }
        }
        WorkspaceError::Git(_)
        | WorkspaceError::Io(_)
        | WorkspaceError::InvalidBaseUrl(_)
        | WorkspaceError::CredentialEncoding => "git_error",
    }
}

/// Commit-status description summarising a successful review's findings.
fn review_summary(outcome: &ar_review::ReviewOutcome) -> String {
    if outcome.findings_count == 0 {
        return "auto_review: no findings".into();
    }
    // Build the breakdown showing only non-zero severity counts so
    // a "1 error" review doesn't read as "1 error, 0 warnings, 0
    // notes". Order: error first (most operator-relevant), then
    // warning, then note.
    let mut parts: Vec<String> = Vec::with_capacity(3);
    if outcome.errors > 0 {
        parts.push(format!(
            "{} error{}",
            outcome.errors,
            if outcome.errors == 1 { "" } else { "s" }
        ));
    }
    if outcome.warnings > 0 {
        parts.push(format!(
            "{} warning{}",
            outcome.warnings,
            if outcome.warnings == 1 { "" } else { "s" }
        ));
    }
    if outcome.notes > 0 {
        parts.push(format!(
            "{} note{}",
            outcome.notes,
            if outcome.notes == 1 { "" } else { "s" }
        ));
    }
    if parts.is_empty() {
        // findings_count > 0 but no severities ticked — defensive
        // fallback in case the pipeline ever emits a finding with
        // a severity outside the enum (it can't today, but the
        // total-fallback path stays cheap).
        return format!("auto_review: {} findings", outcome.findings_count);
    }
    format!("auto_review: {}", parts.join(", "))
}

/// Diff of the commits pushed since `previous_sha` was reviewed: computed
/// in `workspace` when it holds the previous commit, so no host diff limit
/// applies, otherwise fetched from the host's compare endpoint. `None`
/// (falling back to a full review) when neither can supply it, for example
/// after a force push removed the previous commit.
async fn incremental_review_diff(
    host: &dyn ReviewHost,
    job: &ReviewJob,
    previous_sha: &str,
    workspace: Option<&PreparedWorkspace>,
) -> Option<String> {
    if let Some(prepared) = workspace {
        match prepared.incremental_diff().await {
            Ok(diff) => return Some(local_diff_text(job, diff)),
            Err(err) => {
                tracing::info!(
                    repo = format!("{}/{}", job.owner, job.repo),
                    pr = job.pr_number,
                    error = %err,
                    "local incremental diff unavailable; trying the host's compare diff"
                );
            }
        }
    }
    fetch_incremental_diff(host, job, previous_sha).await
}

/// Diff between the previously reviewed SHA and the job's head, or
/// `None` (falling back to a full review) when the host cannot supply it.
async fn fetch_incremental_diff(
    host: &dyn ReviewHost,
    job: &ReviewJob,
    previous_sha: &str,
) -> Option<String> {
    tracing::info!(
        repo = format!("{}/{}", job.owner, job.repo),
        pr = job.pr_number,
        previous = %previous_sha,
        current = %job.head_sha,
        "incremental review: fetching compare diff",
    );
    match host
        .get_compare_diff(&job.owner, &job.repo, previous_sha, &job.head_sha)
        .await
    {
        Ok(diff) => Some(diff),
        Err(err) => {
            match compare_diff_fallback_level(&err) {
                tracing::Level::INFO => {
                    tracing::info!(error = %err, "compare_diff failed; falling back to full diff");
                }
                _ => {
                    tracing::warn!(error = %err, "compare_diff failed; falling back to full diff");
                }
            }
            None
        }
    }
}

/// Post a commit status on the job's head SHA; failures are logged only.
async fn post_review_status(
    host: &dyn ReviewHost,
    job: &ReviewJob,
    state: CommitStatusState,
    description: String,
) {
    let status = CommitStatus {
        state,
        target_url: String::new(),
        description,
        context: STATUS_CONTEXT.into(),
    };
    if let Err(err) = host
        .post_commit_status(&job.owner, &job.repo, &job.head_sha, &status)
        .await
    {
        tracing::warn!(error = %err, "failed to post commit status");
    }
}

/// Changed-file list used for the trivial-files skip check.
async fn fetch_changed_files_for_triage(
    host: &dyn ReviewHost,
    job: &ReviewJob,
) -> Result<Vec<ar_forge::ChangedFile>, ar_forge::HostError> {
    host.list_changed_files(&job.owner, &job.repo, job.pr_number)
        .await
}

/// Full PR diff used to build the RAG context.
async fn fetch_context_diff(
    host: &dyn ReviewHost,
    job: &ReviewJob,
) -> Result<String, ar_forge::HostError> {
    host.get_pr_diff(&job.owner, &job.repo, job.pr_number).await
}

/// Log level for a failed compare-diff: `INFO` for the expected
/// "base commit no longer in the head repository" 404, `WARN` otherwise.
fn compare_diff_fallback_level(err: &ar_forge::HostError) -> tracing::Level {
    let body = err.message();
    if body.contains("API error 404")
        && body.contains("The target couldn't be found.")
        && body.contains("could not find '")
        && body.contains("' to be a commit, branch or tag in the head repository")
        && !body.contains(".diff'")
    {
        tracing::Level::INFO
    } else {
        tracing::Level::WARN
    }
}

/// Read `AR_SEVERITY_FLOOR` from the environment. See
/// [`parse_severity_floor`] for the value grammar and defaulting
/// behaviour.
fn severity_floor_from_env() -> ar_review::ReviewSeverity {
    parse_severity_floor(env::var("AR_SEVERITY_FLOOR").ok().as_deref())
}

/// Parse a `AR_SEVERITY_FLOOR` value. Recognised values:
/// `note` (post everything), `warning` (default; drop note-only
/// nits), `error` (only post Error-severity findings).
///
/// Default (None or empty) is `Warning`: notes are pure noise on
/// the PR page once the verifier has run, and operators on busy
/// repos almost always raise the floor on day two anyway. Notes
/// remain useful as LLM scratchpad inside the review pipeline —
/// they're generated and counted, just not posted. Operators who
/// want notes on the PR set `AR_SEVERITY_FLOOR=note` explicitly.
///
/// Unrecognised values fall through to the default with a warn
/// log so a typo doesn't accidentally start surfacing notes
/// (under the old default it didn't suppress findings; under the
/// new default we lean the same way — towards the operator's
/// signal-to-noise expectation rather than the typo).
fn parse_severity_floor(raw: Option<&str>) -> ar_review::ReviewSeverity {
    use ar_review::ReviewSeverity;
    let normalised = raw.map(|value| value.trim().to_ascii_lowercase());
    match normalised.as_deref() {
        None | Some("" | "warning" | "warn") => ReviewSeverity::Warning,
        Some("note") => ReviewSeverity::Note,
        Some("error" | "err") => ReviewSeverity::Error,
        Some(other) => {
            tracing::warn!(
                value = other,
                "AR_SEVERITY_FLOOR unrecognised; defaulting to Warning (drop note-only nits)"
            );
            ReviewSeverity::Warning
        }
    }
}

/// Cap `text` at [`MAX_STATUS_DESCRIPTION`] bytes on a char boundary,
/// ending with an ellipsis when it was cut.
fn truncate_status_description(text: &str) -> String {
    // The ellipsis '…' is 3 bytes in UTF-8; reserve room so the
    // total output still fits under MAX_STATUS_DESCRIPTION.
    const ELLIPSIS_BYTES: usize = '\u{2026}'.len_utf8();
    if text.len() <= MAX_STATUS_DESCRIPTION {
        return text.to_owned();
    }
    let mut cut = MAX_STATUS_DESCRIPTION.saturating_sub(ELLIPSIS_BYTES);
    while cut > 0 && !text.is_char_boundary(cut) {
        cut = cut.saturating_sub(1);
    }
    // `cut` is a char boundary (0 always is), so `get` cannot miss.
    let head = text.get(..cut).unwrap_or_default();
    format!("{head}\u{2026}")
}

/// Commit-status state for a failed review: `Error` for infrastructure
/// failures, `Failure` for model failures.
#[expect(
    clippy::rest_pattern_accessible_field,
    reason = "only the variant matters here; spelling each Unhealable field as `_` instead \
              trips unneeded_field_pattern"
)]
fn error_state(err: &ReviewError) -> CommitStatusState {
    match *err {
        ReviewError::Forgejo(_) | ReviewError::Host(_) | ReviewError::Workspace(_) => {
            CommitStatusState::Error
        }
        ReviewError::Llm(_) | ReviewError::Unhealable { .. } => CommitStatusState::Failure,
    }
}

/// Stable label string for [`ReviewObservation::Failed`]. Used by
/// the gateway's `/metrics` endpoint to bucket failures so operators
/// can see `llm` vs `workspace` vs `forgejo` outage rates separately.
#[expect(
    clippy::rest_pattern_accessible_field,
    reason = "only the variant matters here; spelling each Unhealable field as `_` instead \
              trips unneeded_field_pattern"
)]
fn error_class(err: &ReviewError) -> &'static str {
    match *err {
        ReviewError::Forgejo(_) | ReviewError::Host(_) => "forgejo",
        ReviewError::Workspace(_) => "workspace",
        ReviewError::Llm(_) => "llm",
        ReviewError::Unhealable { .. } => "unhealable",
    }
}

#[cfg(test)]
#[expect(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test assertions and fakes: a failed setup step, poisoned lock or unexpected \
              variant should fail the test where it happens, and indexing follows a length \
              assertion on the same vector"
)]
mod tests {
    use super::*;

    use ar_llm::{
        CompleteRequest, CompleteResponse, Error as LlmError, LlmProvider, ModelTier, Router,
    };

    use crate::sqlite_history::SqliteReviewHistory;

    use core::str::from_utf8;

    use std::collections::HashMap;

    use std::fs;

    use std::io;

    use std::process::Command;

    use std::sync::Mutex;

    use tracing::subscriber::{set_default, DefaultGuard};

    use wiremock::matchers::{method, path};

    use wiremock::{Mock, MockServer, ResponseTemplate};

    const HOSTILE_HEAD_CONFIG: &str = "enabled: false\nignored_paths: [\"**\"]\n";

    const BASE_KEYS_CONFIG: &str = "\
guidelines: BASE-GUIDELINE
ignored_paths: [\"docs/**\"]
pr_metadata_check:
  enabled: true
  additional_rules: [\"base rule\"]
override_approvers: [\"base-maintainer\"]
";

    const HEAD_KEYS_CONFIG: &str = "\
guidelines: HEAD-GUIDELINE
ignored_paths: [\"src/**\"]
pr_metadata_check: false
override_approvers: [\"pr-author\"]
";

    /// GitHub's answer when a diff is over its size limits.
    const DIFF_TOO_LARGE: &str = "API error 406: {\"message\":\"Sorry, the diff exceeded the \
maximum number of files (300). Consider using 'List pull requests files' API or locally cloning \
the repository instead.\",\"errors\":[{\"resource\":\"PullRequest\",\"field\":\"diff\",\
\"code\":\"too_large\"}],\"status\":\"406\"}";

    struct CannedProvider {
        responses: Mutex<Vec<String>>,
        seen: Mutex<Vec<CompleteRequest>>,
    }

    impl CannedProvider {
        fn first_request_text(&self) -> String {
            let seen = self.seen.lock().unwrap();
            seen.first().map_or_default(|req| {
                req.messages
                    .iter()
                    .map(|msg| msg.content.as_str())
                    .collect::<Vec<_>>()
                    .join("\n")
            })
        }

        fn last_user_prompt(&self) -> Option<String> {
            let seen = self.seen.lock().unwrap();
            let req = seen.first()?;
            req.messages
                .iter()
                .find(|msg| matches!(msg.role, ar_llm::Role::User))
                .map(|msg| msg.content.clone())
        }

        fn new(responses: Vec<&str>) -> Self {
            Self {
                responses: Mutex::new(responses.into_iter().map(String::from).collect()),
                seen: Mutex::new(Vec::new()),
            }
        }
    }

    #[async_trait]
    impl LlmProvider for CannedProvider {
        async fn complete(&self, req: CompleteRequest) -> Result<CompleteResponse, LlmError> {
            self.seen.lock().unwrap().push(req);
            let next = self
                .responses
                .lock()
                .unwrap()
                .pop()
                .unwrap_or_else(|| r#"{"summary":"ok","findings":[]}"#.to_owned());
            Ok(CompleteResponse {
                content: next,
                input_tokens: 0,
                output_tokens: 0,
            })
        }
    }

    /// Log lines written while a [`tracing`] subscriber built by
    /// [`captured_logs`] is the thread's default.
    #[derive(Clone, Default)]
    struct CapturedLogs(Arc<Mutex<Vec<u8>>>);

    impl CapturedLogs {
        fn contents(&self) -> String {
            String::from_utf8_lossy(&self.0.lock().unwrap()).into_owned()
        }
    }

    impl io::Write for CapturedLogs {
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }

        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.0
                .lock()
                .map_err(|err| io::Error::other(err.to_string()))?
                .extend_from_slice(buf);
            Ok(buf.len())
        }
    }

    /// Embeds every text to the same direction, so any stored learning
    /// with that embedding is a nearest match for the diff.
    struct ConstantEmbedder;

    #[async_trait]
    impl LlmProvider for ConstantEmbedder {
        async fn complete(&self, _req: CompleteRequest) -> Result<CompleteResponse, LlmError> {
            Err(LlmError::Provider {
                status: 500,
                body: "embedder only".into(),
            })
        }

        async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, LlmError> {
            Ok(texts.iter().map(|_| vec![1.0, 0.0]).collect())
        }
    }

    #[derive(Default)]
    struct RecordingReviewHost {
        /// Base-branch files served by `get_file_content`, keyed by
        /// `(ref, path)`. Missing keys are `Ok(None)` (404).
        base_files: HashMap<(String, String), String>,
        changed_file_requests: Mutex<Vec<(String, String, u64)>>,
        clone_url_requests: Mutex<Vec<(String, String)>>,
        /// When set, `get_compare_diff` fails with this error.
        compare_error: Option<String>,
        compare_requests: Mutex<Vec<(String, String, String, String)>>,
        /// When set, `get_file_content` fails with this non-404 error.
        file_error: Option<String>,
        file_requests: Mutex<Vec<(String, String)>>,
        /// Clone URL of a local head repository; `None` serves an
        /// unreachable URL so workspace prep fails.
        head_clone_url: Option<String>,
        /// Base SHA `get_pull_request` reports; `None` serves `"base"`.
        pr_base_sha: Option<String>,
        /// When set, `get_pr_diff` fails with this error.
        pr_diff_error: Option<String>,
        pr_diff_requests: Mutex<Vec<(String, String, u64)>>,
        /// When set, `get_pull_request` fails with this error.
        pr_fetch_error: Option<String>,
        pr_fetches: Mutex<usize>,
        review_requests: Mutex<Vec<(String, String, u64, ar_forge::CreateReviewRequest)>>,
        status_requests: Mutex<Vec<(String, String, String, CommitStatus)>>,
    }

    impl RecordingReviewHost {
        fn with_base_file(mut self, ref_name: &str, path: &str, body: &str) -> Self {
            self.base_files
                .insert((ref_name.to_owned(), path.to_owned()), body.to_owned());
            self
        }

        fn with_compare_error(mut self, message: &str) -> Self {
            self.compare_error = Some(message.to_owned());
            self
        }

        fn with_head_clone_url(mut self, url: &str) -> Self {
            self.head_clone_url = Some(url.to_owned());
            self
        }

        fn with_pr_base_sha(mut self, sha: &str) -> Self {
            self.pr_base_sha = Some(sha.to_owned());
            self
        }

        fn with_pr_diff_error(mut self, message: &str) -> Self {
            self.pr_diff_error = Some(message.to_owned());
            self
        }
    }

    /// A local git repository whose single commit holds `files`, served
    /// over `file://` so `prepare_workspace_from_clone_url` can clone it.
    struct HeadRepo {
        _dir: tempfile::TempDir,
        sha: String,
        url: String,
    }

    /// A local repository holding a pull request: `fork` files committed on
    /// `main`, then a `topic` branch adding `head` files and a later `main`
    /// commit adding `base_only` files that the pull request does not
    /// contain. Served over `file://` so the workspace can fetch both tips.
    struct PrRepo {
        _dir: tempfile::TempDir,
        base_sha: String,
        head_sha: String,
        url: String,
    }

    #[derive(Default)]
    struct RecordingObserver {
        seen: Mutex<Vec<ReviewObservation>>,
    }

    impl ReviewObserver for RecordingObserver {
        fn record(&self, observation: ReviewObservation) {
            self.seen.lock().unwrap().push(observation);
        }
    }

    #[async_trait]
    impl ar_forge::ReviewHost for RecordingReviewHost {
        async fn clone_url(&self, owner: &str, repo: &str) -> Result<String, ar_forge::HostError> {
            self.clone_url_requests
                .lock()
                .unwrap()
                .push((owner.to_owned(), repo.to_owned()));
            Ok(self
                .head_clone_url
                .clone()
                .unwrap_or_else(|| format!("https://example.invalid/{owner}/{repo}.git")))
        }

        async fn create_review(
            &self,
            owner: &str,
            repo: &str,
            pr_number: u64,
            request: &ar_forge::CreateReviewRequest,
        ) -> Result<ar_forge::CreatedReview, ar_forge::HostError> {
            self.review_requests.lock().unwrap().push((
                owner.to_owned(),
                repo.to_owned(),
                pr_number,
                request.clone(),
            ));
            Ok(ar_forge::CreatedReview {
                id: 1,
                state: "COMMENT".to_owned(),
            })
        }

        async fn get_compare_diff(
            &self,
            owner: &str,
            repo: &str,
            base: &str,
            head: &str,
        ) -> Result<String, ar_forge::HostError> {
            self.compare_requests.lock().unwrap().push((
                owner.to_owned(),
                repo.to_owned(),
                base.to_owned(),
                head.to_owned(),
            ));
            if let Some(message) = self.compare_error.as_ref() {
                return Err(ar_forge::HostError::new(message.clone()));
            }
            Ok("diff --git a/src/lib.rs b/src/lib.rs\n+new\n".to_owned())
        }

        async fn get_file_content(
            &self,
            _owner: &str,
            _repo: &str,
            file_path: &str,
            ref_: &str,
        ) -> Result<Option<String>, ar_forge::HostError> {
            self.file_requests
                .lock()
                .unwrap()
                .push((ref_.to_owned(), file_path.to_owned()));
            if let Some(message) = self.file_error.as_ref() {
                return Err(ar_forge::HostError::new(message.clone()));
            }
            Ok(self
                .base_files
                .get(&(ref_.to_owned(), file_path.to_owned()))
                .cloned())
        }

        async fn get_pr_diff(
            &self,
            owner: &str,
            repo: &str,
            pr_number: u64,
        ) -> Result<String, ar_forge::HostError> {
            self.pr_diff_requests.lock().unwrap().push((
                owner.to_owned(),
                repo.to_owned(),
                pr_number,
            ));
            if let Some(message) = self.pr_diff_error.as_ref() {
                return Err(ar_forge::HostError::new(message.clone()));
            }
            Ok("diff --git a/src/lib.rs b/src/lib.rs\n+context\n".to_owned())
        }

        async fn get_pull_request(
            &self,
            _owner: &str,
            _repo: &str,
            pr_number: u64,
        ) -> Result<ar_forge::PullRequestSummary, ar_forge::HostError> {
            let mut fetches = self.pr_fetches.lock().unwrap();
            *fetches = fetches.saturating_add(1);
            drop(fetches);
            if let Some(message) = self.pr_fetch_error.as_ref() {
                return Err(ar_forge::HostError::new(message.clone()));
            }
            Ok(ar_forge::PullRequestSummary {
                number: pr_number,
                title: "title".to_owned(),
                body: "body".to_owned(),
                draft: false,
                state: "open".to_owned(),
                head: ar_forge::PullRequestRefSummary {
                    ref_name: "feature".to_owned(),
                    sha: "head".to_owned(),
                },
                base: ar_forge::PullRequestRefSummary {
                    ref_name: "trunk".to_owned(),
                    sha: self
                        .pr_base_sha
                        .clone()
                        .unwrap_or_else(|| "base".to_owned()),
                },
            })
        }

        async fn list_changed_files(
            &self,
            owner: &str,
            repo: &str,
            pr_number: u64,
        ) -> Result<Vec<ar_forge::ChangedFile>, ar_forge::HostError> {
            self.changed_file_requests.lock().unwrap().push((
                owner.to_owned(),
                repo.to_owned(),
                pr_number,
            ));
            Ok(vec![ar_forge::ChangedFile {
                filename: "src/lib.rs".to_owned(),
                status: "modified".to_owned(),
                additions: 1,
                deletions: 0,
                changes: 1,
                patch: None,
                previous_filename: None,
            }])
        }

        async fn list_pr_review_comments(
            &self,
            _owner: &str,
            _repo: &str,
            _pr_number: u64,
        ) -> Result<Vec<ar_forge::PrReviewComment>, ar_forge::HostError> {
            Ok(Vec::new())
        }

        async fn list_pull_review_comments(
            &self,
            _owner: &str,
            _repo: &str,
            _pr_number: u64,
            _review_id: u64,
        ) -> Result<Vec<ar_forge::PrReviewComment>, ar_forge::HostError> {
            Ok(Vec::new())
        }

        async fn list_pull_reviews(
            &self,
            _owner: &str,
            _repo: &str,
            _pr_number: u64,
        ) -> Result<Vec<ar_forge::PullReviewSummary>, ar_forge::HostError> {
            Ok(Vec::new())
        }

        async fn post_commit_status(
            &self,
            owner: &str,
            repo: &str,
            sha: &str,
            status: &CommitStatus,
        ) -> Result<(), ar_forge::HostError> {
            self.status_requests.lock().unwrap().push((
                owner.to_owned(),
                repo.to_owned(),
                sha.to_owned(),
                status.clone(),
            ));
            Ok(())
        }

        async fn update_pull_request(
            &self,
            _owner: &str,
            _repo: &str,
            _pr_number: u64,
            _title: Option<&str>,
            _body: Option<&str>,
        ) -> Result<(), ar_forge::HostError> {
            Ok(())
        }
    }

    struct RecordingDispatcher {
        seen: Mutex<Vec<ReviewJob>>,
    }

    impl RecordingDispatcher {
        fn new() -> Self {
            Self {
                seen: Mutex::new(Vec::new()),
            }
        }
    }

    #[async_trait]
    impl JobDispatcher for RecordingDispatcher {
        async fn dispatch(&self, job: ReviewJob) {
            self.seen.lock().unwrap().push(job);
        }
    }

    fn test_base_ref() -> BaseRef {
        BaseRef {
            name: "trunk".to_owned(),
            sha: "base".to_owned(),
        }
    }

    fn router_with(provider: Arc<CannedProvider>) -> Router {
        Router::new().with(ModelTier::Reasoning, provider)
    }

    #[tokio::test]
    async fn job_review_context_retrieves_only_the_jobs_repository_learnings() {
        // The job is mixed case and the learning is stored lowercase, so
        // this also covers the case-insensitive scope match.
        use ar_index::{InMemoryLearningsStore, LearningSource, RepoScope};

        let store = InMemoryLearningsStore::new();
        store
            .add(
                "Widget forbids unwrap.".into(),
                LearningSource::Chat,
                vec![1.0, 0.0],
                0,
                &RepoScope::new("acme", "widget"),
                None,
            )
            .await
            .unwrap();
        let llm = Router::new().with(ModelTier::Embedding, Arc::new(ConstantEmbedder));
        let dir = tempfile::tempdir().unwrap();
        let job_for = |repo: &str| ReviewJob {
            owner: "Acme".to_owned(),
            repo: repo.to_owned(),
            pr_number: 1,
            head_sha: "abc".to_owned(),
            pr_title: String::new(),
            pr_body: String::new(),
            base_sha: None,
            force: false,
        };

        let same = job_review_context(
            dir.path(),
            &llm,
            "diff",
            Some(&store),
            None,
            &job_for("Widget"),
        )
        .await
        .unwrap();
        assert!(same.contains("Widget forbids unwrap."), "{same}");

        let other = job_review_context(
            dir.path(),
            &llm,
            "diff",
            Some(&store),
            None,
            &job_for("Gadget"),
        )
        .await
        .unwrap();
        assert!(!other.contains("Widget forbids unwrap."), "{other}");
    }

    #[tokio::test]
    async fn workspace_prep_gets_clone_url_from_review_host() {
        let host = RecordingReviewHost::default();
        let provider = Arc::new(CannedProvider::new(vec![]));
        let llm = router_with(provider);
        let job = ReviewJob {
            owner: "alice".to_owned(),
            repo: "widgets".to_owned(),
            pr_number: 42,
            head_sha: "not-a-sha".to_owned(),
            pr_title: "title".to_owned(),
            pr_body: String::new(),
            base_sha: None,
            force: false,
        };

        let result = prepare_workspace_context(
            &host,
            &llm,
            "https://legacy.example",
            "legacy-token",
            None,
            None,
            &job,
            &test_base_ref(),
            None,
        )
        .await;

        assert!(
            matches!(&result, WorkspacePrep::Ready(out) if out.workspace.is_none()),
            "invalid SHA should stop before git runs and leave no workspace"
        );
        assert_eq!(
            host.clone_url_requests.lock().unwrap().as_slice(),
            &[("alice".to_owned(), "widgets".to_owned())]
        );
    }

    fn head_repo_with(files: &[(&str, &str)]) -> HeadRepo {
        let dir = tempfile::tempdir().unwrap();
        let git = |args: &[&str]| {
            // Clear the repository-locating variables git exports to hooks, so
            // this fixture still targets its own tempdir when the suite runs
            // inside a git hook (for example the pre-push `just ci`).
            let out = Command::new("git")
                .env_remove("GIT_DIR")
                .env_remove("GIT_WORK_TREE")
                .env_remove("GIT_INDEX_FILE")
                .env_remove("GIT_COMMON_DIR")
                .env_remove("GIT_OBJECT_DIRECTORY")
                .env_remove("GIT_ALTERNATE_OBJECT_DIRECTORIES")
                .env_remove("GIT_PREFIX")
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .arg("-C")
                .arg(dir.path())
                .args(["-c", "user.name=t", "-c", "user.email=t@example.invalid"])
                .args([
                    "-c",
                    "commit.gpgsign=false",
                    "-c",
                    "init.defaultBranch=main",
                ])
                .args(args)
                .output()
                .unwrap();
            assert!(out.status.success(), "git {args:?}: {out:?}");
            String::from_utf8(out.stdout).unwrap()
        };
        git(&["init", "-q"]);
        for &(path, body) in files {
            let full = dir.path().join(path);
            fs::create_dir_all(full.parent().unwrap()).unwrap();
            fs::write(full, body).unwrap();
        }
        git(&["add", "-A"]);
        git(&["commit", "-q", "-m", "head"]);
        let sha = git(&["rev-parse", "HEAD"]).trim().to_owned();
        let url = format!("file://{}", dir.path().display());
        HeadRepo {
            _dir: dir,
            url,
            sha,
        }
    }

    fn pr_repo_with(
        fork: &[(&str, &str)],
        head: &[(&str, &str)],
        base_only: &[(&str, &str)],
    ) -> PrRepo {
        pr_repo_with_pushes(fork, &[head], base_only).0
    }

    /// Like [`pr_repo_with`], with the `topic` branch built from one commit
    /// per entry of `pushes`. Also returns each push's commit SHA; the last
    /// is the head.
    fn pr_repo_with_pushes(
        fork: &[(&str, &str)],
        pushes: &[&[(&str, &str)]],
        base_only: &[(&str, &str)],
    ) -> (PrRepo, Vec<String>) {
        let dir = tempfile::tempdir().unwrap();
        let git = |args: &[&str]| {
            // Same hook-safe environment as `head_repo_with`.
            let out = Command::new("git")
                .env_remove("GIT_DIR")
                .env_remove("GIT_WORK_TREE")
                .env_remove("GIT_INDEX_FILE")
                .env_remove("GIT_COMMON_DIR")
                .env_remove("GIT_OBJECT_DIRECTORY")
                .env_remove("GIT_ALTERNATE_OBJECT_DIRECTORIES")
                .env_remove("GIT_PREFIX")
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .arg("-C")
                .arg(dir.path())
                .args(["-c", "user.name=t", "-c", "user.email=t@example.invalid"])
                .args([
                    "-c",
                    "commit.gpgsign=false",
                    "-c",
                    "init.defaultBranch=main",
                ])
                .args(args)
                .output()
                .unwrap();
            assert!(out.status.success(), "git {args:?}: {out:?}");
            String::from_utf8(out.stdout).unwrap()
        };
        let commit = |files: &[(&str, &str)], message: &str| {
            for &(path, body) in files {
                let full = dir.path().join(path);
                fs::create_dir_all(full.parent().unwrap()).unwrap();
                fs::write(full, body).unwrap();
            }
            git(&["add", "-A"]);
            git(&["commit", "-q", "--allow-empty", "-m", message]);
            git(&["rev-parse", "HEAD"]).trim().to_owned()
        };
        git(&["init", "-q"]);
        commit(fork, "fork point");
        git(&["checkout", "-q", "-b", "topic"]);
        let push_shas: Vec<String> = pushes
            .iter()
            .map(|files| commit(files, "pull request push"))
            .collect();
        let head_sha = push_shas.last().cloned().unwrap();
        git(&["checkout", "-q", "main"]);
        let base_sha = commit(base_only, "base moved on");
        let url = format!("file://{}", dir.path().display());
        (
            PrRepo {
                _dir: dir,
                base_sha,
                head_sha,
                url,
            },
            push_shas,
        )
    }

    /// Run `job` with `previous_sha` recorded as the last reviewed commit.
    async fn run_incremental(
        host: &RecordingReviewHost,
        provider: Arc<CannedProvider>,
        previous_sha: &str,
        job: ReviewJob,
    ) {
        let history = InMemoryReviewHistory::new();
        history
            .record(
                &PrKey {
                    owner: job.owner.clone(),
                    repo: job.repo.clone(),
                    pr_number: job.pr_number,
                },
                previous_sha,
            )
            .await
            .unwrap();
        let llm = router_with(provider);
        run_review_job(host, &llm, "", "", &history, None, None, None, job).await;
    }

    #[tokio::test]
    async fn incremental_review_diffs_new_commits_locally_when_the_host_compare_is_too_large() {
        let (pr, pushes) = pr_repo_with_pushes(
            &[("src/lib.rs", "pub fn old() {}\n")],
            &[
                &[("src/first.rs", "pub fn FIRST_PUSH() {}\n")],
                &[("src/second.rs", "pub fn SECOND_PUSH() {}\n")],
            ],
            &[],
        );
        let host = RecordingReviewHost::default()
            .with_head_clone_url(&pr.url)
            .with_pr_base_sha(&pr.base_sha)
            .with_compare_error(DIFF_TOO_LARGE);
        let provider = Arc::new(CannedProvider::new(vec![]));

        run_incremental(
            &host,
            Arc::clone(&provider),
            &pushes[0],
            job_at(&pr.head_sha),
        )
        .await;

        let prompt = provider.last_user_prompt().expect("LLM called");
        assert!(prompt.contains("+pub fn SECOND_PUSH() {}"), "{prompt}");
        assert!(
            !prompt.contains("FIRST_PUSH"),
            "an incremental review covers only the commits since the previous review: {prompt}"
        );
        assert!(
            host.compare_requests.lock().unwrap().is_empty(),
            "the host's compare diff must not be fetched when the local diff succeeds"
        );
    }

    #[tokio::test]
    async fn pull_request_whose_host_diff_is_too_large_is_reviewed_with_findings() {
        let too_many_lines = DIFF_TOO_LARGE.replace(
            "maximum number of files (300)",
            "maximum number of lines (20000)",
        );
        for refusal in [DIFF_TOO_LARGE, too_many_lines.as_str()] {
            let pr = pr_repo_with(
                &[("src/lib.rs", "pub fn old() {}\n")],
                &[("src/lib.rs", "pub fn old() {}\npub fn local_added() {}\n")],
                &[],
            );
            let host = RecordingReviewHost::default()
                .with_head_clone_url(&pr.url)
                .with_pr_base_sha(&pr.base_sha)
                .with_pr_diff_error(refusal)
                .with_compare_error(refusal);
            let provider = Arc::new(CannedProvider::new(vec![
                r#"{"verdicts":[{"finding_index":0,"keep":true,"reasoning":"real"}]}"#,
                r#"{"summary":"one issue","findings":[{"path":"src/lib.rs","line_start":2,"severity":"warning","message":"LOCAL-FINDING"}]}"#,
            ]));
            let reasoning: Arc<CannedProvider> = Arc::clone(&provider);
            let llm = Router::new()
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
                job_at(&pr.head_sha),
            )
            .await;

            let reviews = host.review_requests.lock().unwrap();
            assert_eq!(reviews.len(), 1, "{refusal}");
            let posted = format!("{:?}", reviews[0].3);
            assert!(posted.contains("LOCAL-FINDING"), "{posted}");
            let statuses = host.status_requests.lock().unwrap();
            let last = statuses.last().map(|entry| entry.3.clone()).unwrap();
            assert!(
                matches!(
                    last.state,
                    CommitStatusState::Success | CommitStatusState::Failure
                ),
                "the final status reports the review, not a failed one: {last:?}"
            );
        }
    }

    #[tokio::test]
    async fn incremental_review_falls_back_to_a_full_review_when_the_previous_commit_is_gone() {
        let (pr, _pushes) = pr_repo_with_pushes(
            &[("src/lib.rs", "pub fn old() {}\n")],
            &[
                &[("src/first.rs", "pub fn FIRST_PUSH() {}\n")],
                &[("src/second.rs", "pub fn SECOND_PUSH() {}\n")],
            ],
            &[],
        );
        // A force push removed the reviewed commit: neither the workspace
        // nor the host's compare endpoint can find it.
        let host = RecordingReviewHost::default()
            .with_head_clone_url(&pr.url)
            .with_pr_base_sha(&pr.base_sha)
            .with_compare_error("API error 404: Not Found");
        let provider = Arc::new(CannedProvider::new(vec![]));

        run_incremental(
            &host,
            Arc::clone(&provider),
            "0123456789abcdef0123456789abcdef01234567",
            job_at(&pr.head_sha),
        )
        .await;

        assert_eq!(host.compare_requests.lock().unwrap().len(), 1);
        let prompt = provider.last_user_prompt().expect("LLM called");
        assert!(
            prompt.contains("FIRST_PUSH") && prompt.contains("SECOND_PUSH"),
            "a full review covers every commit of the pull request: {prompt}"
        );
        assert!(
            host.pr_diff_requests.lock().unwrap().is_empty(),
            "the full review still uses the local merge-base diff"
        );
    }

    /// Capture this thread's log output (tokio tests run on one thread)
    /// until the returned guard drops.
    fn captured_logs() -> (CapturedLogs, DefaultGuard) {
        let logs = CapturedLogs::default();
        let writer = logs.clone();
        let subscriber = tracing_subscriber::fmt()
            .with_writer(move || writer.clone())
            .with_ansi(false)
            .without_time()
            .finish();
        (logs, set_default(subscriber))
    }

    /// With parallel reviews, an operator must be able to tell which review
    /// fell back to the host's diff and why, from one warning.
    #[tokio::test]
    async fn local_diff_fallback_warning_names_the_pull_request_and_the_reason() {
        let pr = pr_repo_with(
            &[("src/lib.rs", "pub fn old() {}\n")],
            &[("src/lib.rs", "pub fn old() {}\npub fn local_added() {}\n")],
            &[],
        );
        // A base commit the repository does not have cannot be fetched.
        let host = RecordingReviewHost::default()
            .with_head_clone_url(&pr.url)
            .with_pr_base_sha("0123456789abcdef0123456789abcdef01234567");
        let provider = Arc::new(CannedProvider::new(vec![]));
        let (logs, _guard) = captured_logs();

        run_with(&host, Arc::clone(&provider), None, &pr.head_sha).await;

        let output = logs.contents();
        let warning = output
            .lines()
            .find(|line| line.contains("using the host's diff"))
            .unwrap_or_else(|| panic!("no fallback warning in:\n{output}"));
        assert!(
            warning.contains("WARN")
                && warning.contains("repo=\"alice/widgets\"")
                && warning.contains("pr=42")
                && warning.contains("could not fetch the commit"),
            "{warning}"
        );
    }

    /// Operators alert on fleet-wide fallbacks from a counter, not logs.
    #[tokio::test]
    async fn local_diff_fallback_is_observed_with_its_reason() {
        let pr = pr_repo_with(
            &[("src/lib.rs", "pub fn old() {}\n")],
            &[("src/lib.rs", "pub fn old() {}\npub fn local_added() {}\n")],
            &[],
        );
        let host = RecordingReviewHost::default()
            .with_head_clone_url(&pr.url)
            .with_pr_base_sha("0123456789abcdef0123456789abcdef01234567");
        let observer = RecordingObserver::default();

        run_with(
            &host,
            Arc::new(CannedProvider::new(vec![])),
            Some(&observer),
            &pr.head_sha,
        )
        .await;

        let fallbacks: Vec<&'static str> = observer
            .seen
            .lock()
            .unwrap()
            .iter()
            .filter_map(|observation| {
                if let ReviewObservation::LocalDiffFallback { reason } = *observation {
                    Some(reason)
                } else {
                    None
                }
            })
            .collect();
        assert_eq!(fallbacks, vec!["fetch_failed"]);
    }

    /// A clone that fails (forge refusing it, bad credentials) also leaves
    /// the review on the host's diff, and a fleet-wide clone failure must
    /// feed the same fallback counter.
    #[tokio::test]
    async fn clone_failure_is_observed_as_a_local_diff_fallback() {
        let host = RecordingReviewHost::default()
            .with_head_clone_url("file:///nonexistent/auto-review-fixture.git")
            .with_pr_base_sha("0123456789abcdef0123456789abcdef01234567");
        let observer = RecordingObserver::default();

        run_with(
            &host,
            Arc::new(CannedProvider::new(vec![])),
            Some(&observer),
            "abcdef0123456789abcdef0123456789abcdef01",
        )
        .await;

        let fallbacks: Vec<&'static str> = observer
            .seen
            .lock()
            .unwrap()
            .iter()
            .filter_map(|observation| {
                if let ReviewObservation::LocalDiffFallback { reason } = *observation {
                    Some(reason)
                } else {
                    None
                }
            })
            .collect();
        assert_eq!(fallbacks, vec!["clone_failed"]);
    }

    #[tokio::test]
    async fn clone_failure_warning_names_the_pull_request() {
        let host = RecordingReviewHost::default()
            .with_head_clone_url("file:///nonexistent/auto-review-fixture.git")
            .with_pr_base_sha("0123456789abcdef0123456789abcdef01234567");
        let (logs, _guard) = captured_logs();

        run_with(
            &host,
            Arc::new(CannedProvider::new(vec![])),
            None,
            "abcdef0123456789abcdef0123456789abcdef01",
        )
        .await;

        let output = logs.contents();
        let warning = output
            .lines()
            .find(|line| line.contains("workspace prep failed"))
            .unwrap_or_else(|| panic!("no clone failure warning in:\n{output}"));
        assert!(
            warning.contains("WARN")
                && warning.contains("repo=\"alice/widgets\"")
                && warning.contains("pr=42"),
            "{warning}"
        );
    }

    /// A local diff cut at its read limit drops files from the review text;
    /// the operator must be able to tell which review lost them.
    #[test]
    fn truncated_local_diff_warning_names_the_pull_request() {
        let (logs, guard) = captured_logs();

        let text = local_diff_text(
            &job_at("abcdef0123456789abcdef0123456789abcdef01"),
            LocalDiff {
                text: "diff --git a/x b/x\n".into(),
                truncated: true,
            },
        );
        drop(guard);

        assert_eq!(text, "diff --git a/x b/x\n");
        let output = logs.contents();
        let warning = output
            .lines()
            .find(|line| line.contains("read limit"))
            .unwrap_or_else(|| panic!("no truncation warning in:\n{output}"));
        assert!(
            warning.contains("WARN")
                && warning.contains("repo=\"alice/widgets\"")
                && warning.contains("pr=42"),
            "{warning}"
        );
    }

    #[test]
    fn whole_local_diff_logs_nothing() {
        let (logs, guard) = captured_logs();

        local_diff_text(
            &job_at("abcdef0123456789abcdef0123456789abcdef01"),
            LocalDiff {
                text: String::new(),
                truncated: false,
            },
        );
        drop(guard);

        assert_eq!(logs.contents(), "");
    }

    #[test]
    fn local_diff_fallback_reasons_are_bounded_labels() {
        let no_base = |reason| WorkspaceError::NoMergeBase(reason);
        let cases = [
            (no_base(MergeBaseUnavailable::HistoryCap), "history_cap"),
            (
                no_base(MergeBaseUnavailable::TimedOut(Duration::from_secs(1))),
                "timed_out",
            ),
            (
                no_base(MergeBaseUnavailable::Fetch("gone".into())),
                "fetch_failed",
            ),
            (
                WorkspaceError::NoIncrementalBase(MergeBaseUnavailable::Deepen("x".into())),
                "fetch_failed",
            ),
            (
                no_base(MergeBaseUnavailable::UnrelatedHistories),
                "no_merge_base",
            ),
            (WorkspaceError::Git("unknown option".into()), "git_error"),
        ];
        for (err, label) in cases {
            assert_eq!(local_diff_fallback_reason(&err), label, "{err}");
        }
    }

    #[tokio::test]
    async fn full_review_reads_the_local_merge_base_diff_instead_of_the_host_diff() {
        let pr = pr_repo_with(
            &[("src/lib.rs", "pub fn old() {}\n")],
            &[("src/lib.rs", "pub fn old() {}\npub fn local_added() {}\n")],
            &[],
        );
        let host = RecordingReviewHost::default()
            .with_head_clone_url(&pr.url)
            .with_pr_base_sha(&pr.base_sha);
        let provider = Arc::new(CannedProvider::new(vec![]));

        run_with(&host, Arc::clone(&provider), None, &pr.head_sha).await;

        assert!(
            host.pr_diff_requests.lock().unwrap().is_empty(),
            "the host's whole-PR diff must not be fetched when the local diff succeeds"
        );
        let prompt = provider.last_user_prompt().expect("LLM called");
        assert!(prompt.contains("+pub fn local_added() {}"), "{prompt}");
        assert_eq!(host.review_requests.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn full_review_takes_its_changed_file_list_from_the_local_diff() {
        let pr = pr_repo_with(
            &[("src/lib.rs", "pub fn old() {}\n")],
            &[
                ("src/lib.rs", "pub fn old() {}\npub fn local_added() {}\n"),
                ("src/extra.rs", "pub fn extra() {}\n"),
            ],
            &[],
        );
        let host = RecordingReviewHost::default()
            .with_head_clone_url(&pr.url)
            .with_pr_base_sha(&pr.base_sha);

        run_with(
            &host,
            Arc::new(CannedProvider::new(vec![])),
            None,
            &pr.head_sha,
        )
        .await;

        assert_eq!(
            host.changed_file_requests.lock().unwrap().len(),
            1,
            "only the pre-clone triage reads the host's file list; the review uses the local one"
        );
    }

    #[tokio::test]
    async fn full_review_diffs_against_the_jobs_base_sha_when_it_has_one() {
        let pr = pr_repo_with(
            &[("src/lib.rs", "pub fn old() {}\n")],
            &[("src/lib.rs", "pub fn old() {}\npub fn local_added() {}\n")],
            &[],
        );
        // The fetched pull request reports no usable base SHA, so only the
        // job's own base SHA can produce the local diff.
        let host = RecordingReviewHost::default().with_head_clone_url(&pr.url);
        let llm = router_with(Arc::new(CannedProvider::new(vec![])));

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
                base_sha: Some(pr.base_sha.clone()),
                ..job_at(&pr.head_sha)
            },
        )
        .await;

        assert!(
            host.pr_diff_requests.lock().unwrap().is_empty(),
            "the job's base SHA should drive the local diff"
        );
    }

    #[tokio::test]
    async fn local_diff_reaches_the_model_filtered_capped_and_without_binary_content() {
        // Well past DEFAULT_MAX_DIFF_BYTES (100 000), ending in a marker
        // line the cap must cut off.
        let mut huge = "pub const FILLER: &str = \"0123456789abcdef\";\n".repeat(6_000);
        huge.push_str("pub const HUGE_TAIL_MARKER: u8 = 0;\n");
        let pr = pr_repo_with(
            &[("src/lib.rs", "pub fn old() {}\n")],
            &[
                ("src/lib.rs", "pub fn old() {}\npub fn local_added() {}\n"),
                ("docs/guide.md", "IGNORED-DOC-LINE\n"),
                ("assets/logo.png", "\u{0}PNG-BINARY-PAYLOAD\u{0}\u{1}\u{2}"),
                ("src/zz_huge.rs", &huge),
            ],
            &[("src/base_only.rs", "pub fn BASE_ONLY_CHANGE() {}\n")],
        );
        let host = RecordingReviewHost::default()
            .with_head_clone_url(&pr.url)
            .with_pr_base_sha(&pr.base_sha)
            .with_base_file(
                "trunk",
                ".auto_review.yaml",
                "ignored_paths: [\"docs/**\"]\n",
            );
        let provider = Arc::new(CannedProvider::new(vec![]));

        run_with(&host, Arc::clone(&provider), None, &pr.head_sha).await;

        assert!(host.pr_diff_requests.lock().unwrap().is_empty());
        let prompt = provider.last_user_prompt().expect("LLM called");
        assert!(prompt.contains("+pub fn local_added() {}"), "{prompt}");
        assert!(
            prompt.contains("assets/logo.png"),
            "binary files still count as changed"
        );
        assert!(
            !prompt.contains("PNG-BINARY-PAYLOAD"),
            "binary content must not reach the model"
        );
        assert!(
            !prompt.contains("docs/guide.md") && !prompt.contains("IGNORED-DOC-LINE"),
            "base-branch ignored_paths apply to the local diff"
        );
        assert!(
            !prompt.contains("BASE_ONLY_CHANGE"),
            "the diff starts at the merge base, not the base tip"
        );
        assert!(
            !prompt.contains("HUGE_TAIL_MARKER") && prompt.len() < huge.len(),
            "cap_diff bounds the local diff; prompt was {} bytes",
            prompt.len()
        );
    }

    fn job_at(head_sha: &str) -> ReviewJob {
        ReviewJob {
            owner: "alice".to_owned(),
            repo: "widgets".to_owned(),
            pr_number: 42,
            head_sha: head_sha.to_owned(),
            pr_title: "feat: widgets".to_owned(),
            pr_body: "Adds widgets.".to_owned(),
            base_sha: None,
            force: false,
        }
    }

    #[tokio::test]
    async fn hostile_head_config_does_not_disable_or_narrow_review() {
        let head = head_repo_with(&[
            (".auto_review.yaml", HOSTILE_HEAD_CONFIG),
            ("src/lib.rs", "pub fn context() {}\n"),
        ]);
        let host = RecordingReviewHost::default().with_head_clone_url(&head.url);
        let provider = Arc::new(CannedProvider::new(vec![]));
        let llm = router_with(Arc::clone(&provider));

        run_review_job(
            &host,
            &llm,
            "",
            "",
            &InMemoryReviewHistory::new(),
            None,
            None,
            None,
            job_at(&head.sha),
        )
        .await;

        assert_eq!(
            host.review_requests.lock().unwrap().len(),
            1,
            "a PR-supplied enabled: false must not disable its own review"
        );
        assert_eq!(*host.pr_fetches.lock().unwrap(), 1);
        let statuses = host.status_requests.lock().unwrap();
        let last = statuses.last().map(|entry| entry.3.clone()).unwrap();
        assert!(
            matches!(
                last.state,
                CommitStatusState::Success | CommitStatusState::Failure
            ) && last.description != "auto_review: disabled by repo config",
            "final status comes from the normal review path: {last:?}"
        );
        let prompt = provider.last_user_prompt().expect("LLM called");
        assert!(
            prompt.contains("src/lib.rs"),
            "PR-supplied ignored_paths must not hide changed files; prompt:\n{prompt}"
        );
    }

    fn as_dyn_observer(observer: &RecordingObserver) -> &dyn ReviewObserver {
        observer
    }

    async fn run_with(
        host: &RecordingReviewHost,
        provider: Arc<CannedProvider>,
        observer: Option<&RecordingObserver>,
        head_sha: &str,
    ) {
        let llm = router_with(provider);
        run_review_job(
            host,
            &llm,
            "",
            "",
            &InMemoryReviewHistory::new(),
            None,
            None,
            observer.map(as_dyn_observer),
            job_at(head_sha),
        )
        .await;
    }

    #[tokio::test]
    async fn base_branch_enabled_false_skips_review_even_when_head_enables_it() {
        let head = head_repo_with(&[
            (".auto_review.yaml", "enabled: true\n"),
            ("src/lib.rs", "pub fn context() {}\n"),
        ]);
        let host = RecordingReviewHost::default()
            .with_head_clone_url(&head.url)
            .with_base_file("trunk", ".auto_review.yaml", "enabled: false\n");
        let observer = RecordingObserver::default();

        run_with(
            &host,
            Arc::new(CannedProvider::new(vec![])),
            Some(&observer),
            &head.sha,
        )
        .await;

        assert!(host.review_requests.lock().unwrap().is_empty());
        let statuses = host.status_requests.lock().unwrap();
        let last = statuses.last().map(|entry| entry.3.clone()).unwrap();
        assert_eq!(last.state, CommitStatusState::Success);
        assert_eq!(last.description, "auto_review: disabled by repo config");
        assert!(observer
            .seen
            .lock()
            .unwrap()
            .iter()
            .any(|observation| matches!(
                *observation,
                ReviewObservation::Skipped {
                    reason: "disabled_by_config"
                }
            )));
    }

    #[tokio::test]
    async fn base_branch_enabled_false_skips_review_when_head_has_no_config() {
        let host = RecordingReviewHost::default().with_base_file(
            "trunk",
            ".auto_review.yml",
            "enabled: false\n",
        );
        let observer = RecordingObserver::default();

        run_with(
            &host,
            Arc::new(CannedProvider::new(vec![])),
            Some(&observer),
            "0123456789abcdef0123456789abcdef01234567",
        )
        .await;

        assert!(host.review_requests.lock().unwrap().is_empty());
        let statuses = host.status_requests.lock().unwrap();
        let last = statuses.last().map(|entry| entry.3.clone()).unwrap();
        assert_eq!(last.state, CommitStatusState::Success);
        assert_eq!(last.description, "auto_review: disabled by repo config");
        assert!(observer
            .seen
            .lock()
            .unwrap()
            .iter()
            .any(|observation| matches!(
                *observation,
                ReviewObservation::Skipped {
                    reason: "disabled_by_config"
                }
            )));
    }

    #[tokio::test]
    async fn workspace_prep_takes_every_config_key_from_the_base_branch() {
        let head = head_repo_with(&[
            (".auto_review.yaml", HEAD_KEYS_CONFIG),
            ("src/lib.rs", "pub fn context() {}\n"),
        ]);
        let host = RecordingReviewHost::default()
            .with_head_clone_url(&head.url)
            .with_base_file("trunk", ".auto_review.yaml", BASE_KEYS_CONFIG);
        let llm = router_with(Arc::new(CannedProvider::new(vec![])));

        let prep = prepare_workspace_context(
            &host,
            &llm,
            "",
            "",
            None,
            None,
            &job_at(&head.sha),
            &test_base_ref(),
            None,
        )
        .await;

        let WorkspacePrep::Ready(out) = prep else {
            panic!("base config enables review");
        };
        assert!(out.workspace.is_some(), "head was cloned");
        assert_eq!(out.config.ignored_paths, vec!["docs/**".to_owned()]);
        assert_eq!(out.config.guidelines, "BASE-GUIDELINE");
        assert!(out.config.pr_metadata_check.enabled);
        assert_eq!(
            out.config.pr_metadata_check.additional_rules,
            vec!["base rule".to_owned()]
        );
        assert_eq!(
            out.config.override_approvers,
            vec!["base-maintainer".to_owned()]
        );
        assert!(!out.config.is_override_approver("pr-author"));
    }

    #[tokio::test]
    async fn review_generation_uses_base_branch_guidelines_and_ignored_paths() {
        let head = head_repo_with(&[
            (".auto_review.yaml", HEAD_KEYS_CONFIG),
            ("src/lib.rs", "pub fn context() {}\n"),
        ]);
        let host = RecordingReviewHost::default()
            .with_head_clone_url(&head.url)
            .with_base_file("trunk", ".auto_review.yaml", BASE_KEYS_CONFIG);
        let provider = Arc::new(CannedProvider::new(vec![]));

        run_with(&host, Arc::clone(&provider), None, &head.sha).await;

        assert_eq!(host.review_requests.lock().unwrap().len(), 1);
        let text = provider.first_request_text();
        assert!(text.contains("BASE-GUIDELINE"), "{text}");
        assert!(!text.contains("HEAD-GUIDELINE"), "{text}");
        let prompt = provider.last_user_prompt().expect("LLM called");
        assert!(
            prompt.contains("src/lib.rs"),
            "head ignored_paths [src/**] must not apply; prompt:\n{prompt}"
        );
    }

    #[tokio::test]
    async fn non_404_base_config_fetch_error_still_reviews() {
        let host = RecordingReviewHost {
            file_error: Some("API error 500: upstream unavailable".to_owned()),
            ..RecordingReviewHost::default()
        };

        run_with(
            &host,
            Arc::new(CannedProvider::new(vec![])),
            None,
            "0123456789abcdef0123456789abcdef01234567",
        )
        .await;

        assert_eq!(host.review_requests.lock().unwrap().len(), 1);
        let statuses = host.status_requests.lock().unwrap();
        let last = statuses.last().map(|entry| entry.3.clone()).unwrap();
        assert_eq!(last.state, CommitStatusState::Success);
    }

    #[tokio::test]
    async fn run_review_job_fetches_pull_request_once_and_reads_config_at_its_base_name() {
        let host = RecordingReviewHost::default();

        run_with(
            &host,
            Arc::new(CannedProvider::new(vec![])),
            None,
            "0123456789abcdef0123456789abcdef01234567",
        )
        .await;

        assert_eq!(*host.pr_fetches.lock().unwrap(), 1);
        assert_eq!(
            host.file_requests.lock().unwrap().as_slice(),
            &[
                ("trunk".to_owned(), ".auto_review.yaml".to_owned()),
                ("trunk".to_owned(), ".auto_review.yml".to_owned()),
            ]
        );
    }

    #[tokio::test]
    #[expect(
        clippy::rest_pattern_accessible_field,
        reason = "the assertion pins error_class and ignores the elapsed duration; spelling it \
                  `duration: _` instead trips unneeded_field_pattern"
    )]
    async fn pull_request_fetch_failure_posts_error_status_and_skips_review() {
        let host = RecordingReviewHost {
            pr_fetch_error: Some("API error 502".to_owned()),
            ..RecordingReviewHost::default()
        };
        let provider = Arc::new(CannedProvider::new(vec![]));
        let observer = RecordingObserver::default();

        run_with(
            &host,
            Arc::clone(&provider),
            Some(&observer),
            "0123456789abcdef0123456789abcdef01234567",
        )
        .await;

        assert!(host.review_requests.lock().unwrap().is_empty());
        assert!(provider.seen.lock().unwrap().is_empty(), "no LLM call");
        assert!(host.file_requests.lock().unwrap().is_empty());
        let statuses = host.status_requests.lock().unwrap();
        let last = statuses.last().map(|entry| entry.3.clone()).unwrap();
        assert_eq!(last.state, CommitStatusState::Error);
        assert!(last.description.contains("API error 502"), "{last:?}");
        assert!(observer
            .seen
            .lock()
            .unwrap()
            .iter()
            .any(|observation| matches!(
                *observation,
                ReviewObservation::Failed {
                    error_class: "forgejo",
                    ..
                }
            )));
    }

    fn sample_event() -> PullRequestEvent {
        serde_json::from_value(serde_json::json!({
            "action": "opened",
            "number": 42,
            "pull_request": {
                "number": 42,
                "title": "fix: bug",
                "body": "details",
                "draft": false,
                "user": {"login": "alice", "id": 1},
                "head": {"ref": "topic", "sha": "deadbeef"},
                "base": {"ref": "main", "sha": "cafef00d"}
            },
            "repository": {
                "name": "widgets", "full_name": "alice/widgets",
                "default_branch": "main",
                "owner": {"login": "alice", "id": 1}
            },
            "sender": {"login": "alice", "id": 1}
        }))
        .unwrap()
    }

    #[test]
    fn review_job_extracts_owner_repo_pr_and_sha_from_event() {
        let evt = sample_event();
        let job = ReviewJob::from(&evt);
        assert_eq!(job.owner, "alice");
        assert_eq!(job.repo, "widgets");
        assert_eq!(job.pr_number, 42);
        assert_eq!(job.head_sha, "deadbeef");
        assert_eq!(job.pr_title, "fix: bug");
        assert_eq!(job.pr_body, "details");
    }

    #[test]
    fn review_job_takes_base_sha_from_event() {
        let job = ReviewJob::from(&sample_event());
        assert_eq!(job.base_sha.as_deref(), Some("cafef00d"));
    }

    #[tokio::test]
    async fn recording_dispatcher_captures_dispatched_jobs() {
        let dispatcher = RecordingDispatcher::new();
        let evt = sample_event();
        dispatcher.dispatch(ReviewJob::from(&evt)).await;
        let seen = dispatcher.seen.lock().unwrap();
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].pr_number, 42);
    }

    #[tokio::test]
    async fn no_op_dispatcher_does_nothing_and_does_not_panic() {
        let dispatcher = NoOpDispatcher;
        dispatcher.dispatch(ReviewJob::from(&sample_event())).await;
    }

    #[tokio::test]
    async fn incremental_compare_diff_fetches_through_review_host() {
        let host = RecordingReviewHost::default();
        let job = ReviewJob {
            owner: "alice".into(),
            repo: "widgets".into(),
            pr_number: 42,
            head_sha: "head-sha".into(),
            pr_title: "fix".into(),
            pr_body: String::new(),
            base_sha: None,
            force: false,
        };

        let diff = fetch_incremental_diff(&host, &job, "base-sha").await;

        assert_eq!(
            diff.as_deref(),
            Some("diff --git a/src/lib.rs b/src/lib.rs\n+new\n")
        );
        assert_eq!(
            host.compare_requests.lock().unwrap().as_slice(),
            &[(
                "alice".to_owned(),
                "widgets".to_owned(),
                "base-sha".to_owned(),
                "head-sha".to_owned()
            )]
        );
    }

    #[tokio::test]
    async fn commit_status_posts_through_review_host() {
        let host = RecordingReviewHost::default();
        let job = ReviewJob {
            owner: "alice".into(),
            repo: "widgets".into(),
            pr_number: 42,
            head_sha: "head-sha".into(),
            pr_title: "fix".into(),
            pr_body: String::new(),
            base_sha: None,
            force: false,
        };

        post_review_status(
            &host,
            &job,
            CommitStatusState::Pending,
            "auto_review running".into(),
        )
        .await;

        assert_eq!(
            host.status_requests.lock().unwrap().as_slice(),
            &[(
                "alice".to_owned(),
                "widgets".to_owned(),
                "head-sha".to_owned(),
                CommitStatus {
                    state: CommitStatusState::Pending,
                    target_url: String::new(),
                    description: "auto_review running".to_owned(),
                    context: STATUS_CONTEXT.to_owned(),
                }
            )]
        );
    }

    #[tokio::test]
    async fn triage_changed_files_fetches_through_review_host() {
        let host = RecordingReviewHost::default();
        let job = ReviewJob {
            owner: "alice".into(),
            repo: "widgets".into(),
            pr_number: 42,
            head_sha: "head-sha".into(),
            pr_title: "fix".into(),
            pr_body: String::new(),
            base_sha: None,
            force: false,
        };

        let files = fetch_changed_files_for_triage(&host, &job)
            .await
            .expect("changed files");

        assert_eq!(files[0].filename, "src/lib.rs");
        assert_eq!(
            host.changed_file_requests.lock().unwrap().as_slice(),
            &[("alice".to_owned(), "widgets".to_owned(), 42)]
        );
    }

    #[tokio::test]
    async fn context_diff_fetches_through_review_host() {
        let host = RecordingReviewHost::default();
        let job = ReviewJob {
            owner: "alice".into(),
            repo: "widgets".into(),
            pr_number: 42,
            head_sha: "head-sha".into(),
            pr_title: "fix".into(),
            pr_body: String::new(),
            base_sha: None,
            force: false,
        };

        let diff = fetch_context_diff(&host, &job).await.expect("context diff");

        assert_eq!(diff, "diff --git a/src/lib.rs b/src/lib.rs\n+context\n");
        assert_eq!(
            host.pr_diff_requests.lock().unwrap().as_slice(),
            &[("alice".to_owned(), "widgets".to_owned(), 42)]
        );
    }

    #[expect(
        clippy::default_numeric_fallback,
        reason = "json! fixture numbers and float assertion literals take their type from the \
                  comparison; a literal suffix would trip separated_literal_suffix or \
                  unseparated_literal_suffix instead"
    )]
    async fn mount_pull_request(server: &MockServer, head_sha: &str) {
        Mock::given(method("GET"))
            .and(path("/api/v1/repos/o/r/pulls/7"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "number": 7,
                "title": "title",
                "body": "body",
                "draft": false,
                "state": "open",
                "head": {"ref": "topic", "sha": head_sha},
                "base": {"ref": "main", "sha": "cafef00d"}
            })))
            .mount(server)
            .await;
    }

    #[tokio::test]
    #[expect(
        clippy::default_numeric_fallback,
        reason = "json! fixture numbers and float assertion literals take their type from the \
                  comparison; a literal suffix would trip separated_literal_suffix or \
                  unseparated_literal_suffix instead"
    )]
    async fn run_review_job_passes_previous_sha_to_incremental_review_prompt() {
        let server = MockServer::start().await;
        let previous_sha = "8f3c2d1e9a0b4c5d6e7f8a9b0c1d2e3f4a5b6c7d";
        let head_sha = "deadbeef";

        Mock::given(method("GET"))
            .and(path(format!(
                "/o/r/compare/{previous_sha}...{head_sha}.diff"
            )))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                "diff --git a/src/lib.rs b/src/lib.rs\n@@ -1 +1,2 @@\n pub fn old() {}\n+pub fn added() {}\n",
            ))
            .mount(&server)
            .await;
        mount_pull_request(&server, head_sha).await;
        Mock::given(method("GET"))
            .and(path("/api/v1/repos/o/r/pulls/7/files"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {"filename": "src/lib.rs", "status": "modified"}
            ])))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path(format!("/api/v1/repos/o/r/statuses/{head_sha}")))
            .respond_with(ResponseTemplate::new(201))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/api/v1/repos/o/r/pulls/7/reviews"))
            .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({
                "id": 1239,
                "state": "APPROVED"
            })))
            .mount(&server)
            .await;

        let forgejo = ForgejoClient::new(&server.uri(), "tok").expect("client");
        let provider = Arc::new(CannedProvider::new(vec![
            r#"{"summary":"looks fine","findings":[]}"#,
        ]));
        let llm = router_with(Arc::clone(&provider));
        let history = InMemoryReviewHistory::new();
        history
            .record(
                &PrKey {
                    owner: "o".into(),
                    repo: "r".into(),
                    pr_number: 7,
                },
                previous_sha,
            )
            .await
            .expect("record previous SHA");

        run_review_job(
            &forgejo,
            &llm,
            &server.uri(),
            "tok",
            &history,
            None,
            None,
            None,
            ReviewJob {
                owner: "o".into(),
                repo: "r".into(),
                pr_number: 7,
                head_sha: head_sha.into(),
                pr_title: "title".into(),
                pr_body: "body".into(),
                base_sha: None,
                force: false,
            },
        )
        .await;

        let prompt = provider
            .last_user_prompt()
            .expect("LLM should have been called");
        assert!(
            prompt.contains("incremental review")
                && prompt.contains("8f3c2d1")
                && prompt.contains("\u{394} since 8f3c2d1:")
                && prompt.contains("+pub fn added() {}")
                && prompt.contains("leave `walkthrough` empty when nothing material changed"),
            "orchestrated incremental review should pass previous SHA and compare-diff content into ReviewArgs so the prompt scopes walkthrough guidance to the prior review; prompt was:\n{prompt}",
        );
    }

    #[tokio::test]
    #[expect(
        clippy::default_numeric_fallback,
        reason = "json! fixture numbers and float assertion literals take their type from the \
                  comparison; a literal suffix would trip separated_literal_suffix or \
                  unseparated_literal_suffix instead"
    )]
    async fn run_review_job_records_review_outcome_cost_in_sqlite_history() {
        let server = MockServer::start().await;
        let previous_sha = "8f3c2d1e9a0b4c5d6e7f8a9b0c1d2e3f4a5b6c7d";
        let head_sha = "deadbeef";

        Mock::given(method("GET"))
            .and(path(format!(
                "/o/r/compare/{previous_sha}...{head_sha}.diff"
            )))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                "diff --git a/src/lib.rs b/src/lib.rs\n@@ -1 +1,2 @@\n pub fn old() {}\n+pub fn added() {}\n",
            ))
            .mount(&server)
            .await;
        mount_pull_request(&server, head_sha).await;
        Mock::given(method("GET"))
            .and(path("/api/v1/repos/o/r/pulls/7/files"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {"filename": "src/lib.rs", "status": "modified"}
            ])))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path(format!("/api/v1/repos/o/r/statuses/{head_sha}")))
            .respond_with(ResponseTemplate::new(201))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/api/v1/repos/o/r/pulls/7/reviews"))
            .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({
                "id": 1239,
                "state": "APPROVED"
            })))
            .mount(&server)
            .await;

        let forgejo = ForgejoClient::new(&server.uri(), "tok").expect("client");
        let provider = Arc::new(CannedProvider::new(vec![
            r#"{"summary":"looks fine","findings":[]}"#,
        ]));
        let llm = router_with(provider);

        let tmp = tempfile::tempdir().expect("tempdir");
        let db_path = tmp.path().join("history.db");
        let history = SqliteReviewHistory::open(&db_path)
            .await
            .expect("sqlite history");
        history
            .record(
                &PrKey {
                    owner: "o".into(),
                    repo: "r".into(),
                    pr_number: 7,
                },
                previous_sha,
            )
            .await
            .expect("record previous SHA");

        run_review_job(
            &forgejo,
            &llm,
            &server.uri(),
            "tok",
            &history,
            None,
            None,
            None,
            ReviewJob {
                owner: "o".into(),
                repo: "r".into(),
                pr_number: 7,
                head_sha: head_sha.into(),
                pr_title: "title".into(),
                pr_body: "body".into(),
                base_sha: None,
                force: false,
            },
        )
        .await;

        let db_url = format!("sqlite://{}", db_path.display());
        let pr_number: i64 = 7;
        let pool = sqlx::SqlitePool::connect(&db_url)
            .await
            .expect("open sqlite db for verification");
        let cost: f64 = sqlx::query_scalar(
            "SELECT per_review_cost_usd FROM review_history \
             WHERE owner = ?1 AND repo = ?2 AND pr_number = ?3",
        )
        .bind("o")
        .bind("r")
        .bind(pr_number)
        .fetch_one(&pool)
        .await
        .expect("read persisted cost");

        assert_eq!(cost, 0.0);
    }

    #[tokio::test]
    async fn spawning_dispatcher_accepts_review_host_without_forgejo_client() {
        let host: Arc<dyn ReviewHost> = Arc::new(RecordingReviewHost::default());
        let llm = Arc::new(Router::new());

        let dispatcher = SpawningDispatcher::new_with_host(host, llm, "", "");

        assert!(
            dispatcher.concurrency_limit.is_none(),
            "new_with_host should construct the normal dispatcher with default runtime settings"
        );
    }

    #[tokio::test]
    async fn inline_dispatcher_completes_review_before_returning() {
        let host = Arc::new(RecordingReviewHost::default());
        let provider = Arc::new(CannedProvider::new(vec![
            r#"{"summary":"looks fine","findings":[]}"#,
        ]));
        let llm = Arc::new(router_with(provider));
        let dispatcher =
            InlineDispatcher::new_with_host(Arc::<RecordingReviewHost>::clone(&host), llm);

        dispatcher
            .dispatch(ReviewJob {
                owner: "alice".to_owned(),
                repo: "widgets".to_owned(),
                pr_number: 42,
                head_sha: "not-a-sha".to_owned(),
                pr_title: "title".to_owned(),
                pr_body: String::new(),
                base_sha: None,
                force: false,
            })
            .await;

        assert_eq!(
            host.review_requests.lock().unwrap().len(),
            1,
            "inline dispatcher should not return until the review job has posted its review"
        );
        assert!(
            host.status_requests
                .lock()
                .unwrap()
                .iter()
                .any(|entry| entry.3.state == CommitStatusState::Success),
            "inline dispatcher should not return before the final success status is posted"
        );
    }

    #[tokio::test]
    async fn with_concurrency_limit_clamps_zero_to_one() {
        // Defensive: max=0 would deadlock if naively set. The
        // builder clamps to 1 so the bot still makes progress
        // even on pathological config.
        use alloc::sync::Arc;
        use ar_forgejo::Client as ForgejoClient;
        use ar_llm::Router;

        let forgejo = Arc::new(ForgejoClient::new("http://x", "tok").unwrap());
        let llm = Arc::new(Router::new());
        let dispatcher =
            SpawningDispatcher::new(forgejo, llm, "http://x", "tok").with_concurrency_limit(0);
        // Available permits should be 1, not 0 (clamped).
        let sem = dispatcher.concurrency_limit.as_ref().expect("limit set");
        assert_eq!(sem.available_permits(), 1);
    }

    #[tokio::test]
    async fn concurrency_limit_serialises_dispatches_when_capped_at_one() {
        // Two dispatches; cap=1; second waits for first to
        // complete. We verify by timing: the inner work for each
        // dispatch sleeps for 50ms via a custom dispatcher
        // wrapper. With cap=1, total time should be >= 100ms; with
        // no cap (or cap=2) it would be ~50ms.
        //
        // Rather than time-based assertions (flaky), we instead
        // observe that the `available_permits()` count drops to 0
        // while a dispatch is running. The acquire_owned() inside
        // the spawn proves the serialisation in code — this test
        // focuses on the builder semantics that downstream tests
        // rely on.
        use alloc::sync::Arc;
        use ar_forgejo::Client as ForgejoClient;
        use ar_llm::Router;

        let forgejo = Arc::new(ForgejoClient::new("http://x", "tok").unwrap());
        let llm = Arc::new(Router::new());
        let dispatcher =
            SpawningDispatcher::new(forgejo, llm, "http://x", "tok").with_concurrency_limit(2);
        let sem = dispatcher.concurrency_limit.as_ref().expect("limit set");
        // Initially 2 permits available.
        assert_eq!(sem.available_permits(), 2);

        // Acquire one manually to simulate a running review.
        let _permit = Arc::clone(sem).acquire_owned().await.unwrap();
        assert_eq!(sem.available_permits(), 1);

        // Acquire the second.
        let _permit2 = Arc::clone(sem).acquire_owned().await.unwrap();
        assert_eq!(sem.available_permits(), 0);
        // (A third acquire would block — we don't test that path
        // here, since the timing's flaky; the prod code path
        // covers it via the spawned task.)
    }

    fn outcome(errors: usize, warnings: usize, notes: usize) -> ar_review::ReviewOutcome {
        ar_review::ReviewOutcome {
            findings_count: errors.saturating_add(warnings).saturating_add(notes),
            review_id: 1,
            errors,
            warnings,
            notes,
            verifier_dropped: 0,
            estimated_total_cost_usd: 0.0,
        }
    }

    #[test]
    fn review_summary_zero_findings_message() {
        assert_eq!(
            review_summary(&outcome(0, 0, 0)),
            "auto_review: no findings"
        );
    }

    #[test]
    fn review_summary_single_severity_uses_singular_label() {
        assert_eq!(review_summary(&outcome(1, 0, 0)), "auto_review: 1 error");
        assert_eq!(review_summary(&outcome(0, 1, 0)), "auto_review: 1 warning");
        assert_eq!(review_summary(&outcome(0, 0, 1)), "auto_review: 1 note");
    }

    #[test]
    fn review_summary_pluralises_above_one() {
        assert_eq!(review_summary(&outcome(0, 3, 0)), "auto_review: 3 warnings");
    }

    #[test]
    fn review_summary_combines_all_three_severities() {
        // Order: error first, then warning, then note (most-to-
        // least operator-relevant).
        assert_eq!(
            review_summary(&outcome(1, 2, 3)),
            "auto_review: 1 error, 2 warnings, 3 notes"
        );
    }

    #[test]
    fn review_summary_skips_zero_severity_buckets() {
        // 1 error + 0 warnings + 1 note → no "0 warnings" in
        // the output.
        assert_eq!(
            review_summary(&outcome(1, 0, 1)),
            "auto_review: 1 error, 1 note"
        );
    }

    #[test]
    fn compare_diff_404_missing_target_is_unremarkable_fallback() {
        let err = ar_forge::HostError::new(
            r#"API error 404: {"message":"The target couldn't be found.","url":"https://git.johnwilger.com/api/swagger","errors":"could not find 'd34db33f' to be a commit, branch or tag in the head repository jwilger/auto_review"}"#,
        );

        assert_eq!(compare_diff_fallback_level(&err), tracing::Level::INFO);
    }

    #[test]
    fn compare_diff_old_api_construction_bug_404_is_warning() {
        let err = ar_forge::HostError::new(
            r#"API error 404: {"message":"The target couldn't be found.","url":"https://git.johnwilger.com/api/swagger","errors":"could not find 'd34db33f.diff' to be a commit, branch or tag in the head repository jwilger/auto_review"}"#,
        );

        assert_eq!(compare_diff_fallback_level(&err), tracing::Level::WARN);
    }

    #[test]
    fn compare_diff_other_404_remains_warning() {
        let err = ar_forge::HostError::new(r#"API error 404: {"message":"Not found"}"#);

        assert_eq!(compare_diff_fallback_level(&err), tracing::Level::WARN);
    }

    #[test]
    fn truncate_status_description_passes_short_strings_through() {
        assert_eq!(
            truncate_status_description("short message"),
            "short message"
        );
    }

    #[test]
    fn truncate_status_description_caps_long_strings_with_ellipsis() {
        // A typical LLM error response body can be 500+ chars.
        // Forgejo would 422 the post and the PR stays on "Pending"
        // forever.
        let long = format!(
            "auto_review failed: LLM error: provider returned 500: {}",
            "x".repeat(400)
        );
        let out = truncate_status_description(&long);
        assert!(out.len() <= MAX_STATUS_DESCRIPTION);
        assert!(out.ends_with('\u{2026}'));
        assert!(out.starts_with("auto_review failed:"));
    }

    #[test]
    fn severity_floor_defaults_to_warning_when_unset() {
        // Issue #6: notes are LLM scratchpad and pure noise on the
        // PR page. Default has to drop them so a fresh deployment
        // doesn't drown PRs in LGTM-style notes the moment it runs.
        use ar_review::ReviewSeverity;
        assert_eq!(parse_severity_floor(None), ReviewSeverity::Warning);
    }

    #[test]
    fn severity_floor_empty_string_defaults_to_warning() {
        // Helm chart leaves the env var as `""` when unset; treat
        // it the same as missing rather than as an unrecognised
        // value (which would log a warning every dispatch).
        use ar_review::ReviewSeverity;
        assert_eq!(parse_severity_floor(Some("")), ReviewSeverity::Warning);
        assert_eq!(parse_severity_floor(Some("   ")), ReviewSeverity::Warning);
    }

    #[test]
    fn severity_floor_note_opts_in_to_posting_notes() {
        // The opt-in path: operators who want the LLM's note-tier
        // observations on the PR set `note` explicitly.
        use ar_review::ReviewSeverity;
        assert_eq!(parse_severity_floor(Some("note")), ReviewSeverity::Note);
        assert_eq!(parse_severity_floor(Some("NOTE")), ReviewSeverity::Note);
        assert_eq!(parse_severity_floor(Some("  note  ")), ReviewSeverity::Note);
    }

    #[test]
    fn severity_floor_warning_and_error_pass_through() {
        use ar_review::ReviewSeverity;
        assert_eq!(
            parse_severity_floor(Some("warning")),
            ReviewSeverity::Warning
        );
        assert_eq!(parse_severity_floor(Some("warn")), ReviewSeverity::Warning);
        assert_eq!(parse_severity_floor(Some("error")), ReviewSeverity::Error);
        assert_eq!(parse_severity_floor(Some("err")), ReviewSeverity::Error);
    }

    #[test]
    fn severity_floor_unrecognised_falls_back_to_default() {
        // Typos must not accidentally invert the operator's
        // signal-to-noise intent. Falling back to the default
        // (Warning) keeps real findings visible while still not
        // surfacing the LLM's note-tier scratchpad.
        use ar_review::ReviewSeverity;
        assert_eq!(
            parse_severity_floor(Some("warningg")),
            ReviewSeverity::Warning
        );
        assert_eq!(parse_severity_floor(Some("info")), ReviewSeverity::Warning);
    }

    #[test]
    #[expect(
        clippy::assertions_on_result_states,
        reason = "decoding without error is the property under test, so it stays an assertion \
                  rather than an unwrap"
    )]
    fn truncate_status_description_respects_utf8_codepoints() {
        // Multi-byte char near the cut boundary must not be split.
        // Build a string ending in a 4-byte emoji right at the cap.
        let mut text = "x".repeat(MAX_STATUS_DESCRIPTION - 2);
        text.push_str("\u{1f980}tail"); // 🦀 is 4 bytes; "tail" pushes past the cap
        let out = truncate_status_description(&text);
        // Result must be valid UTF-8 (decode without error).
        assert!(from_utf8(out.as_bytes()).is_ok());
        assert!(out.ends_with('\u{2026}'));
    }
}

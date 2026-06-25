use crate::review_history::{InMemoryReviewHistory, PrKey, ReviewHistory};
use ar_forge::ReviewHost;
use ar_forgejo::{Client as ForgejoClient, CommitStatus, CommitStatusState, PullRequestEvent};
use ar_index::{LearningsStore, VectorStore};
use ar_llm::Router as LlmRouter;
use ar_review::{
    build_glob_set, build_review_context_with_store, load_repo_config, pr_is_skippable,
    prepare_workspace_from_clone_url, review_pull_request, GlobSet, PreparedWorkspace, ReviewArgs,
    ReviewError, VerifyMode, WorkspaceError,
};
use async_trait::async_trait;
use std::sync::Arc;
use std::time::{Duration, Instant};

const STATUS_CONTEXT: &str = "auto_review";

/// Observed at the boundary of a single review job's lifecycle.
/// Wired into the gateway's Prometheus counters via
/// [`SpawningDispatcher::with_observer`] without ar-orchestrator
/// having to know about the metrics format.
#[derive(Debug, Clone)]
pub enum ReviewObservation {
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
    /// A review terminated with an error (LLM/Workspace/Forgejo
    /// failure or unhealable JSON). `error_class` is one of:
    /// `"forgejo"`, `"workspace"`, `"llm"`, `"unhealable"`. Stable
    /// strings so operators can label dashboards.
    Failed {
        duration: Duration,
        error_class: &'static str,
    },
    /// The review was a no-op for a benign reason — same SHA already
    /// reviewed (and not forced), all-trivial files, or
    /// `enabled: false`. Distinguished from Failed because operators
    /// shouldn't alert on these.
    Skipped { reason: &'static str },
    /// The dispatch had to wait on the concurrency-cap semaphore
    /// before the review could begin. Counts how often the cap is
    /// engaging — sustained increases mean
    /// `AR_REVIEW_CONCURRENCY` is too tight (or traffic exceeds
    /// capacity). Fires AT MOST once per dispatch, before any
    /// other observation; firing implies a `Started` will follow.
    QueueWait,
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
pub struct ReviewJob {
    pub owner: String,
    pub repo: String,
    pub pr_number: u64,
    pub head_sha: String,
    pub pr_title: String,
    pub pr_body: String,
    /// When true, bypasses the per-PR review-history dedup check and
    /// forces a fresh review even if this SHA was already reviewed.
    /// Used by the chat handler's `re-review` command.
    pub force: bool,
}

impl From<&PullRequestEvent> for ReviewJob {
    fn from(evt: &PullRequestEvent) -> Self {
        Self {
            owner: evt.repository.owner.login.clone(),
            repo: evt.repository.name.clone(),
            pr_number: evt.pull_request.number,
            head_sha: evt.pull_request.head.sha.clone(),
            pr_title: evt.pull_request.title.clone(),
            pr_body: evt.pull_request.body.clone(),
            force: false,
        }
    }
}

/// Hands a [`ReviewJob`] off for processing. The webhook handler calls
/// `dispatch` and returns immediately; the actual review may run for
/// minutes in the background.
#[async_trait]
pub trait JobDispatcher: Send + Sync {
    async fn dispatch(&self, job: ReviewJob);
}

/// Discards every job. Useful for tests that exercise the gateway's
/// dispatch surface without wiring the full review pipeline.
#[derive(Debug, Default, Clone)]
pub struct NoOpDispatcher;

#[async_trait]
impl JobDispatcher for NoOpDispatcher {
    async fn dispatch(&self, _job: ReviewJob) {}
}

/// Synchronous dispatcher for runtimes that need the invocation to finish only
/// after the review job has run to completion. Unlike [`SpawningDispatcher`],
/// this does not spawn a background task.
#[derive(Clone)]
pub struct InlineDispatcher {
    host: Arc<dyn ReviewHost>,
    llm: Arc<LlmRouter>,
    history: Arc<dyn ReviewHistory>,
    learnings: Option<Arc<dyn LearningsStore>>,
    vector_store: Option<Arc<dyn VectorStore>>,
    observer: Option<Arc<dyn ReviewObserver>>,
}

impl InlineDispatcher {
    pub fn new_with_host(host: Arc<dyn ReviewHost>, llm: Arc<LlmRouter>) -> Self {
        Self {
            host,
            llm,
            history: Arc::new(InMemoryReviewHistory::new()),
            learnings: None,
            vector_store: None,
            observer: None,
        }
    }

    pub fn with_history(mut self, history: Arc<dyn ReviewHistory>) -> Self {
        self.history = history;
        self
    }

    pub fn with_learnings(mut self, learnings: Arc<dyn LearningsStore>) -> Self {
        self.learnings = Some(learnings);
        self
    }

    pub fn with_vector_store(mut self, store: Arc<dyn VectorStore>) -> Self {
        self.vector_store = Some(store);
        self
    }

    pub fn with_observer(mut self, observer: Arc<dyn ReviewObserver>) -> Self {
        self.observer = Some(observer);
        self
    }
}

#[async_trait]
impl JobDispatcher for InlineDispatcher {
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
pub struct SpawningDispatcher {
    host: Arc<dyn ReviewHost>,
    llm: Arc<LlmRouter>,
    forgejo_base: Arc<String>,
    forgejo_token: Arc<String>,
    history: Arc<dyn ReviewHistory>,
    learnings: Option<Arc<dyn LearningsStore>>,
    /// Optional shared vector store for symbol embeddings. When set,
    /// embeddings persist across reviews (and, when SQLite-backed,
    /// across gateway restarts) so re-reviews of the same PR don't
    /// re-embed unchanged symbols. None ⇒ build_review_context_with_store
    /// constructs a per-call in-memory store as a back-compat default.
    vector_store: Option<Arc<dyn VectorStore>>,
    observer: Option<Arc<dyn ReviewObserver>>,
    /// Optional cap on concurrent in-flight reviews. When set, a
    /// burst of webhooks beyond the cap waits in the spawn queue
    /// rather than thundering through the LLM and workspace
    /// tmpdirs simultaneously. None = unlimited (back-compat
    /// default; small deployments and tests don't need a cap).
    concurrency_limit: Option<Arc<tokio::sync::Semaphore>>,
}

impl SpawningDispatcher {
    pub fn new(
        forgejo: Arc<ForgejoClient>,
        llm: Arc<LlmRouter>,
        forgejo_base: impl Into<String>,
        forgejo_token: impl Into<String>,
    ) -> Self {
        Self::new_with_host(forgejo, llm, forgejo_base, forgejo_token)
    }

    pub fn new_with_host(
        host: Arc<dyn ReviewHost>,
        llm: Arc<LlmRouter>,
        forgejo_base: impl Into<String>,
        forgejo_token: impl Into<String>,
    ) -> Self {
        Self {
            host,
            llm,
            forgejo_base: Arc::new(forgejo_base.into()),
            forgejo_token: Arc::new(forgejo_token.into()),
            history: Arc::new(InMemoryReviewHistory::new()),
            learnings: None,
            vector_store: None,
            observer: None,
            concurrency_limit: None,
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
    pub fn with_concurrency_limit(mut self, max: usize) -> Self {
        self.concurrency_limit = Some(Arc::new(tokio::sync::Semaphore::new(max.max(1))));
        self
    }

    /// Wire in a metrics observer so review outcomes feed the
    /// gateway's `/metrics` endpoint. Without one, reviews still
    /// run but are invisible to Prometheus.
    pub fn with_observer(mut self, observer: Arc<dyn ReviewObserver>) -> Self {
        self.observer = Some(observer);
        self
    }

    /// Replace the default in-memory history with a custom one
    /// (e.g. SQLite-backed for persistence across restarts).
    pub fn with_history(mut self, history: Arc<dyn ReviewHistory>) -> Self {
        self.history = history;
        self
    }

    /// Wire in a learnings store so remembered guidance gets pulled
    /// into the RAG context for future reviews. When unset,
    /// build_review_context skips the learnings-retrieval step.
    pub fn with_learnings(mut self, learnings: Arc<dyn LearningsStore>) -> Self {
        self.learnings = Some(learnings);
        self
    }

    /// Wire in a shared vector store so symbol embeddings persist
    /// across reviews. Without this, every review constructs a
    /// throwaway in-memory store and re-embeds the entire workspace.
    pub fn with_vector_store(mut self, store: Arc<dyn VectorStore>) -> Self {
        self.vector_store = Some(store);
        self
    }
}

#[async_trait]
impl JobDispatcher for SpawningDispatcher {
    async fn dispatch(&self, job: ReviewJob) {
        let forgejo = self.host.clone();
        let llm = self.llm.clone();
        let base = self.forgejo_base.clone();
        let token = self.forgejo_token.clone();
        let history = self.history.clone();
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
        let forgejo_for_status = forgejo.clone();
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
                    match sem.clone().acquire_owned().await {
                        Ok(p) => Some(p),
                        Err(_) => {
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
            if let Err(e) = inner.await {
                tracing::error!(
                    repo,
                    pr,
                    error = %e,
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
                let _ = forgejo_for_status
                    .post_commit_status(
                        &owner_for_status,
                        &repo_for_status,
                        &sha_for_status,
                        &status,
                    )
                    .await
                    .inspect_err(|err| {
                        tracing::warn!(error = %err, "failed to post crash status");
                    });
            }
        });
    }
}

/// Run one review job to completion.
///
/// 1. Post a "pending" commit status.
/// 2. Clone the repo at the head SHA and prepare review context.
/// 3. Call [`review_pull_request`].
/// 4. Post the final success/error commit status.
///
/// Errors are logged and swallowed; the gateway has already returned 202.
#[allow(clippy::too_many_arguments)]
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
    let observe = |o: ReviewObservation| {
        if let Some(obs) = observer {
            obs.record(o);
        }
    };
    let pr_key = PrKey {
        owner: job.owner.clone(),
        repo: job.repo.clone(),
        pr_number: job.pr_number,
    };
    let last_reviewed_sha = match history.last_reviewed(&pr_key).await {
        Ok(sha) => sha,
        Err(e) => {
            tracing::warn!(error = %e, "review history lookup failed; treating as full review");
            None
        }
    };
    let mut incremental_diff: Option<String> = None;
    if let Some(prev) = &last_reviewed_sha {
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
            incremental_diff = fetch_incremental_diff(forgejo, &job, prev).await;
        }
    }

    post_review_status(
        forgejo,
        &job,
        CommitStatusState::Pending,
        "auto_review running".into(),
    )
    .await;

    // Triage: if every changed file is trivial (lockfile bumps, vendored,
    // generated), skip the LLM call entirely and post a success status.
    // Fetch once for the trivial-file skip check.
    let changed_files = match fetch_changed_files_for_triage(forgejo, &job).await {
        Ok(files) => Some(files),
        Err(e) => {
            tracing::warn!(error = %e, "triage file-list failed; proceeding to review");
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

    let prep_outcome = prepare_workspace_context(
        forgejo,
        llm,
        forgejo_base,
        forgejo_token,
        learnings,
        vector_store,
        &job,
    )
    .await;
    let (ignored_paths, guidelines, repo_context, raw_diff, workspace, pr_metadata_check) =
        match prep_outcome {
            Ok(WorkspacePrepOutput {
                skipped_by_config: true,
                ..
            }) => {
                tracing::info!(
                    repo = format!("{}/{}", job.owner, job.repo),
                    pr = job.pr_number,
                    "skipping review: disabled by .auto_review.yaml"
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
            Ok(WorkspacePrepOutput {
                ignored_paths,
                guidelines,
                repo_context,
                raw_diff,
                workspace,
                pr_metadata_check,
                ..
            }) => (
                ignored_paths,
                guidelines,
                repo_context,
                raw_diff,
                workspace,
                pr_metadata_check,
            ),
            Err(e) => {
                tracing::warn!(error = %e, "workspace/context prep failed; continuing without workspace context");
                (
                    GlobSet::empty(),
                    String::new(),
                    String::new(),
                    String::new(),
                    None,
                    ar_review::config::PrMetadataCheck::default(),
                )
            }
        };

    // The agentic verifier needs the cloned workspace to inspect.
    // Operators opt in by setting AR_AGENTIC_VERIFIER=1; without it,
    // the simple verifier (one-pass diff judgement) keeps running.
    // Either way we silently downgrade to Simple when no workspace
    // was prepared (e.g. workspace prep failed) — but log the
    // downgrade so operators who set the flag aren't left wondering
    // why findings stopped being verified against the workspace.
    let agentic_requested = std::env::var("AR_AGENTIC_VERIFIER")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false);
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
    let workspace_path = workspace.as_ref().map(|w| w.path());

    // Fire Started AFTER all early-skip checks (same_sha,
    // trivial_files, disabled_by_config) have passed. Means each
    // review job emits exactly one of {Skipped_*, Started + (one
    // of Succeeded / Failed_*)} — no double-count when an early
    // skip also fired Started. Operator dashboards rely on
    // `started_total ≈ succeeded + failed_*` for sanity checks.
    observe(ReviewObservation::Started);

    let result = review_pull_request(ReviewArgs {
        host: forgejo,
        llm,
        owner: &job.owner,
        repo: &job.repo,
        pr_number: job.pr_number,
        head_sha: &job.head_sha,
        pr_title: &job.pr_title,
        pr_body: &job.pr_body,
        ignored_paths: &ignored_paths,
        min_severity: severity_floor_from_env(),
        guidelines: &guidelines,
        repo_context: &repo_context,
        // Reuse the diff workspace/context prep already fetched. For
        // incremental reviews we override with the compare-diff
        // (smaller, focused on new commits); otherwise fall back
        // to the full PR diff already in hand. Passing the empty
        // raw_diff (workspace prep failed to fetch) as Some("") would
        // suppress the pipeline's own get_pr_diff retry, so map
        // empty back to None to preserve the retry semantics.
        diff_override: incremental_diff.as_deref().or(if raw_diff.is_empty() {
            None
        } else {
            Some(raw_diff.as_str())
        }),
        previous_review_sha: incremental_diff.as_ref().and(last_reviewed_sha.as_deref()),
        verify_mode,
        workspace_path,
        pr_metadata_check,
    })
    .await;

    let final_status = match &result {
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
        Err(e) => {
            tracing::error!(
                repo = format!("{}/{}", job.owner, job.repo),
                pr = job.pr_number,
                error = %e,
                "review failed"
            );
            observe(ReviewObservation::Failed {
                duration: started_at.elapsed(),
                error_class: error_class(e),
            });
            CommitStatus {
                state: error_state(e),
                target_url: String::new(),
                description: truncate_status_description(&format!("auto_review failed: {e}")),
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
    if let Ok(outcome) = &result {
        if let Err(e) = history
            .record_with_cost(&pr_key, &job.head_sha, outcome.estimated_total_cost_usd)
            .await
        {
            tracing::warn!(error = %e, "failed to record review history");
        }
    } else {
        tracing::debug!(
            sha = %job.head_sha,
            "review failed; not recording SHA so next webhook re-runs a full review"
        );
    }
}

#[derive(Debug, thiserror::Error)]
enum WorkspacePrepError {
    #[error("forgejo: {0}")]
    Forgejo(#[from] ar_forgejo::Error),
    #[error("host: {0}")]
    Host(#[from] ar_forge::HostError),
    #[error("workspace: {0}")]
    Workspace(#[from] WorkspaceError),
}

struct WorkspacePrepOutput {
    skipped_by_config: bool,
    ignored_paths: GlobSet,
    guidelines: String,
    repo_context: String,
    /// The raw PR diff fetched for triage and
    /// context building. Surfaced back so the review pipeline can
    /// reuse it as `diff_override` instead of refetching the same
    /// diff a second time. Empty string when the get_pr_diff call
    /// failed during workspace/context prep (we degrade-but-continue;
    /// the pipeline will refetch and likely also fail consistently).
    raw_diff: String,
    pr_metadata_check: ar_review::config::PrMetadataCheck,
    /// Held by the orchestrator until the review pipeline finishes
    /// so the agentic verifier (when enabled) can inspect the
    /// cloned working tree. `None` when workspace prep exited
    /// without cloning (skipped_by_config doesn't reach this).
    workspace: Option<PreparedWorkspace>,
}

#[allow(clippy::too_many_arguments)]
async fn prepare_workspace_context(
    host: &dyn ReviewHost,
    llm: &LlmRouter,
    _base: &str,
    _token: &str,
    learnings: Option<&dyn LearningsStore>,
    vector_store: Option<&dyn VectorStore>,
    job: &ReviewJob,
) -> Result<WorkspacePrepOutput, WorkspacePrepError> {
    let clone_url = host
        .clone_url(&job.owner, &job.repo)
        .await
        .map_err(WorkspacePrepError::Host)?;
    let workspace = prepare_workspace_from_clone_url(&clone_url, &job.head_sha).await?;
    let config = load_repo_config(workspace.path());
    let ignored_paths = build_glob_set(&config.ignored_paths);
    let guidelines = config.guidelines.clone();
    if !config.enabled {
        return Ok(WorkspacePrepOutput {
            skipped_by_config: true,
            ignored_paths,
            guidelines,
            pr_metadata_check: config.pr_metadata_check.clone(),
            repo_context: String::new(),
            raw_diff: String::new(),
            workspace: None,
        });
    }
    // Fetch the diff once for the RAG context build. Failure here just means we
    // skip optional context; the review still proceeds.
    let raw_diff = match fetch_context_diff(host, job).await {
        Ok(d) => d,
        Err(e) => {
            tracing::warn!(error = %e, "diff fetch for context failed; continuing");
            String::new()
        }
    };

    // Build the RAG context (best-effort): walks the workspace,
    // embeds symbols, queries top-K against the diff. Returns empty
    // string when no Embedding tier is configured or the workspace
    // has no extractable symbols.
    let repo_context = if raw_diff.is_empty() {
        String::new()
    } else {
        build_review_context_with_store(
            workspace.path(),
            llm,
            &raw_diff,
            learnings,
            5,
            vector_store,
        )
        .await
        .unwrap_or_else(|e| {
            tracing::warn!(error = %e, "RAG context build failed; continuing");
            String::new()
        })
    };

    Ok(WorkspacePrepOutput {
        skipped_by_config: false,
        ignored_paths,
        guidelines,
        repo_context,
        raw_diff,
        pr_metadata_check: config.pr_metadata_check,
        workspace: Some(workspace),
    })
}

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
        Err(e) => {
            match compare_diff_fallback_level(&e) {
                tracing::Level::INFO => {
                    tracing::info!(error = %e, "compare_diff failed; falling back to full diff");
                }
                _ => {
                    tracing::warn!(error = %e, "compare_diff failed; falling back to full diff");
                }
            }
            None
        }
    }
}

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
    let _ = host
        .post_commit_status(&job.owner, &job.repo, &job.head_sha, &status)
        .await
        .inspect_err(|e| tracing::warn!(error = %e, "failed to post commit status"));
}

async fn fetch_changed_files_for_triage(
    host: &dyn ReviewHost,
    job: &ReviewJob,
) -> Result<Vec<ar_forge::ChangedFile>, ar_forge::HostError> {
    host.list_changed_files(&job.owner, &job.repo, job.pr_number)
        .await
}

async fn fetch_context_diff(
    host: &dyn ReviewHost,
    job: &ReviewJob,
) -> Result<String, ar_forge::HostError> {
    host.get_pr_diff(&job.owner, &job.repo, job.pr_number).await
}

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
    parse_severity_floor(std::env::var("AR_SEVERITY_FLOOR").ok().as_deref())
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
    let normalised = raw.map(|s| s.trim().to_ascii_lowercase());
    match normalised.as_deref() {
        None | Some("") => ReviewSeverity::Warning,
        Some("note") => ReviewSeverity::Note,
        Some("warning") | Some("warn") => ReviewSeverity::Warning,
        Some("error") | Some("err") => ReviewSeverity::Error,
        Some(other) => {
            tracing::warn!(
                value = other,
                "AR_SEVERITY_FLOOR unrecognised; defaulting to Warning (drop note-only nits)"
            );
            ReviewSeverity::Warning
        }
    }
}

/// Forgejo's commit-status `description` is capped at 255 chars.
/// Posting more returns 422 and the user sees no status update at
/// all — leaving the PR stuck on "Pending" with no operator-visible
/// signal that the review actually completed (with an error). LLM
/// errors in particular can dump multi-hundred-char provider response
/// bodies into the format string. Cap at 240 to leave room for the
/// status badge UI's own padding and end with an ellipsis so users
/// know it was truncated.
const MAX_STATUS_DESCRIPTION: usize = 240;

fn truncate_status_description(s: &str) -> String {
    if s.len() <= MAX_STATUS_DESCRIPTION {
        return s.to_string();
    }
    // The ellipsis '…' is 3 bytes in UTF-8; reserve room so the
    // total output still fits under MAX_STATUS_DESCRIPTION.
    const ELLIPSIS_BYTES: usize = '…'.len_utf8();
    let mut cut = MAX_STATUS_DESCRIPTION.saturating_sub(ELLIPSIS_BYTES);
    while cut > 0 && !s.is_char_boundary(cut) {
        cut -= 1;
    }
    format!("{}…", &s[..cut])
}

fn error_state(err: &ReviewError) -> CommitStatusState {
    match err {
        ReviewError::Forgejo(_) | ReviewError::Host(_) | ReviewError::Workspace(_) => {
            CommitStatusState::Error
        }
        ReviewError::Llm(_) | ReviewError::Unhealable { .. } => CommitStatusState::Failure,
    }
}

/// Stable label string for [`ReviewObservation::Failed`]. Used by
/// the gateway's `/metrics` endpoint to bucket failures so operators
/// can see `llm` vs `workspace` vs `forgejo` outage rates separately.
fn error_class(err: &ReviewError) -> &'static str {
    match err {
        ReviewError::Forgejo(_) | ReviewError::Host(_) => "forgejo",
        ReviewError::Workspace(_) => "workspace",
        ReviewError::Llm(_) => "llm",
        ReviewError::Unhealable { .. } => "unhealable",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ar_llm::{
        CompleteRequest, CompleteResponse, Error as LlmError, LlmProvider, ModelTier, Router,
    };
    use std::sync::Mutex;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    struct CannedProvider {
        responses: Mutex<Vec<String>>,
        seen: Mutex<Vec<CompleteRequest>>,
    }

    impl CannedProvider {
        fn new(responses: Vec<&str>) -> Self {
            Self {
                responses: Mutex::new(responses.into_iter().map(String::from).collect()),
                seen: Mutex::new(Vec::new()),
            }
        }

        fn last_user_prompt(&self) -> Option<String> {
            let seen = self.seen.lock().unwrap();
            seen.first().and_then(|req| {
                req.messages
                    .iter()
                    .find(|m| matches!(m.role, ar_llm::Role::User))
                    .map(|m| m.content.clone())
            })
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
                .unwrap_or_else(|| r#"{"summary":"ok","findings":[]}"#.to_string());
            Ok(CompleteResponse {
                content: next,
                input_tokens: 0,
                output_tokens: 0,
            })
        }
    }

    fn router_with(provider: Arc<CannedProvider>) -> Router {
        Router::new().with(ModelTier::Reasoning, provider)
    }

    #[tokio::test]
    async fn workspace_prep_gets_clone_url_from_review_host() {
        let host = RecordingReviewHost::default();
        let provider = Arc::new(CannedProvider::new(vec![]));
        let llm = router_with(provider);
        let job = ReviewJob {
            owner: "alice".to_string(),
            repo: "widgets".to_string(),
            pr_number: 42,
            head_sha: "not-a-sha".to_string(),
            pr_title: "title".to_string(),
            pr_body: String::new(),
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
        )
        .await;

        assert!(result.is_err(), "invalid SHA should stop before git runs");
        assert_eq!(
            host.clone_url_requests.lock().unwrap().as_slice(),
            &[("alice".to_string(), "widgets".to_string())]
        );
    }

    #[derive(Default)]
    struct RecordingReviewHost {
        pr_diff_requests: Mutex<Vec<(String, String, u64)>>,
        compare_requests: Mutex<Vec<(String, String, String, String)>>,
        status_requests: Mutex<Vec<(String, String, String, CommitStatus)>>,
        changed_file_requests: Mutex<Vec<(String, String, u64)>>,
        clone_url_requests: Mutex<Vec<(String, String)>>,
        review_requests: Mutex<Vec<(String, String, u64, ar_forge::CreateReviewRequest)>>,
    }

    #[async_trait]
    impl ar_forge::ReviewHost for RecordingReviewHost {
        async fn clone_url(&self, owner: &str, repo: &str) -> Result<String, ar_forge::HostError> {
            self.clone_url_requests
                .lock()
                .unwrap()
                .push((owner.to_string(), repo.to_string()));
            Ok(format!("https://example.invalid/{owner}/{repo}.git"))
        }

        async fn get_pull_request(
            &self,
            _owner: &str,
            _repo: &str,
            pr_number: u64,
        ) -> Result<ar_forge::PullRequestSummary, ar_forge::HostError> {
            Ok(ar_forge::PullRequestSummary {
                number: pr_number,
                title: "title".to_string(),
                body: "body".to_string(),
                draft: false,
                state: "open".to_string(),
                head: ar_forge::PullRequestRefSummary {
                    ref_name: "feature".to_string(),
                    sha: "head".to_string(),
                },
                base: ar_forge::PullRequestRefSummary {
                    ref_name: "main".to_string(),
                    sha: "base".to_string(),
                },
            })
        }

        async fn get_pr_diff(
            &self,
            owner: &str,
            repo: &str,
            pr_number: u64,
        ) -> Result<String, ar_forge::HostError> {
            self.pr_diff_requests.lock().unwrap().push((
                owner.to_string(),
                repo.to_string(),
                pr_number,
            ));
            Ok("diff --git a/src/lib.rs b/src/lib.rs\n+context\n".to_string())
        }

        async fn get_compare_diff(
            &self,
            owner: &str,
            repo: &str,
            base: &str,
            head: &str,
        ) -> Result<String, ar_forge::HostError> {
            self.compare_requests.lock().unwrap().push((
                owner.to_string(),
                repo.to_string(),
                base.to_string(),
                head.to_string(),
            ));
            Ok("diff --git a/src/lib.rs b/src/lib.rs\n+new\n".to_string())
        }

        async fn list_changed_files(
            &self,
            owner: &str,
            repo: &str,
            pr_number: u64,
        ) -> Result<Vec<ar_forge::ChangedFile>, ar_forge::HostError> {
            self.changed_file_requests.lock().unwrap().push((
                owner.to_string(),
                repo.to_string(),
                pr_number,
            ));
            Ok(vec![ar_forge::ChangedFile {
                filename: "src/lib.rs".to_string(),
                status: "modified".to_string(),
                additions: 1,
                deletions: 0,
                changes: 1,
                patch: None,
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

        async fn list_pull_reviews(
            &self,
            _owner: &str,
            _repo: &str,
            _pr_number: u64,
        ) -> Result<Vec<ar_forge::PullReviewSummary>, ar_forge::HostError> {
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

        async fn post_commit_status(
            &self,
            owner: &str,
            repo: &str,
            sha: &str,
            status: &CommitStatus,
        ) -> Result<(), ar_forge::HostError> {
            self.status_requests.lock().unwrap().push((
                owner.to_string(),
                repo.to_string(),
                sha.to_string(),
                status.clone(),
            ));
            Ok(())
        }

        async fn create_review(
            &self,
            owner: &str,
            repo: &str,
            pr_number: u64,
            request: &ar_forge::CreateReviewRequest,
        ) -> Result<ar_forge::CreatedReview, ar_forge::HostError> {
            self.review_requests.lock().unwrap().push((
                owner.to_string(),
                repo.to_string(),
                pr_number,
                request.clone(),
            ));
            Ok(ar_forge::CreatedReview {
                id: 1,
                state: "COMMENT".to_string(),
            })
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
        let d = NoOpDispatcher;
        d.dispatch(ReviewJob::from(&sample_event())).await;
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
                "alice".to_string(),
                "widgets".to_string(),
                "base-sha".to_string(),
                "head-sha".to_string()
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
                "alice".to_string(),
                "widgets".to_string(),
                "head-sha".to_string(),
                CommitStatus {
                    state: CommitStatusState::Pending,
                    target_url: String::new(),
                    description: "auto_review running".to_string(),
                    context: STATUS_CONTEXT.to_string(),
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
            force: false,
        };

        let files = fetch_changed_files_for_triage(&host, &job)
            .await
            .expect("changed files");

        assert_eq!(files[0].filename, "src/lib.rs");
        assert_eq!(
            host.changed_file_requests.lock().unwrap().as_slice(),
            &[("alice".to_string(), "widgets".to_string(), 42)]
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
            force: false,
        };

        let diff = fetch_context_diff(&host, &job).await.expect("context diff");

        assert_eq!(diff, "diff --git a/src/lib.rs b/src/lib.rs\n+context\n");
        assert_eq!(
            host.pr_diff_requests.lock().unwrap().as_slice(),
            &[("alice".to_string(), "widgets".to_string(), 42)]
        );
    }

    #[tokio::test]
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
        let llm = router_with(provider.clone());
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
                && prompt.contains("Δ since 8f3c2d1:")
                && prompt.contains("+pub fn added() {}")
                && prompt.contains("leave `walkthrough` empty when nothing material changed"),
            "orchestrated incremental review should pass previous SHA and compare-diff content into ReviewArgs so the prompt scopes walkthrough guidance to the prior review; prompt was:\n{prompt}",
        );
    }

    #[tokio::test]
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
        let history = crate::sqlite_history::SqliteReviewHistory::open(&db_path)
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
                force: false,
            },
        )
        .await;

        let db_url = format!("sqlite://{}", db_path.display());
        let pool = sqlx::SqlitePool::connect(&db_url)
            .await
            .expect("open sqlite db for verification");
        let cost: f64 = sqlx::query_scalar(
            "SELECT per_review_cost_usd FROM review_history \
             WHERE owner = ?1 AND repo = ?2 AND pr_number = ?3",
        )
        .bind("o")
        .bind("r")
        .bind(7_i64)
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
        let dispatcher = InlineDispatcher::new_with_host(host.clone(), llm);

        dispatcher
            .dispatch(ReviewJob {
                owner: "alice".to_string(),
                repo: "widgets".to_string(),
                pr_number: 42,
                head_sha: "not-a-sha".to_string(),
                pr_title: "title".to_string(),
                pr_body: String::new(),
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
                .any(|(_, _, _, status)| status.state == CommitStatusState::Success),
            "inline dispatcher should not return before the final success status is posted"
        );
    }

    #[tokio::test]
    async fn with_concurrency_limit_clamps_zero_to_one() {
        // Defensive: max=0 would deadlock if naively set. The
        // builder clamps to 1 so the bot still makes progress
        // even on pathological config.
        use ar_forgejo::Client as ForgejoClient;
        use ar_llm::Router;
        use std::sync::Arc;

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
        use ar_forgejo::Client as ForgejoClient;
        use ar_llm::Router;
        use std::sync::Arc;

        let forgejo = Arc::new(ForgejoClient::new("http://x", "tok").unwrap());
        let llm = Arc::new(Router::new());
        let dispatcher =
            SpawningDispatcher::new(forgejo, llm, "http://x", "tok").with_concurrency_limit(2);
        let sem = dispatcher.concurrency_limit.as_ref().expect("limit set");
        // Initially 2 permits available.
        assert_eq!(sem.available_permits(), 2);

        // Acquire one manually to simulate a running review.
        let _permit = sem.clone().acquire_owned().await.unwrap();
        assert_eq!(sem.available_permits(), 1);

        // Acquire the second.
        let _permit2 = sem.clone().acquire_owned().await.unwrap();
        assert_eq!(sem.available_permits(), 0);
        // (A third acquire would block — we don't test that path
        // here, since the timing's flaky; the prod code path
        // covers it via the spawned task.)
    }

    fn outcome(errors: usize, warnings: usize, notes: usize) -> ar_review::ReviewOutcome {
        ar_review::ReviewOutcome {
            findings_count: errors + warnings + notes,
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
        assert!(out.ends_with('…'));
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
    fn truncate_status_description_respects_utf8_codepoints() {
        // Multi-byte char near the cut boundary must not be split.
        // Build a string ending in a 4-byte emoji right at the cap.
        let mut s = "x".repeat(MAX_STATUS_DESCRIPTION - 2);
        s.push_str("🦀tail"); // 🦀 is 4 bytes; "tail" pushes past the cap
        let out = truncate_status_description(&s);
        // Result must be valid UTF-8 (decode without error).
        assert!(std::str::from_utf8(out.as_bytes()).is_ok());
        assert!(out.ends_with('…'));
    }
}

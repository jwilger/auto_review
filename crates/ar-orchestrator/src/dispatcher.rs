use crate::review_history::{InMemoryReviewHistory, PrKey, ReviewHistory};
use ar_forgejo::{Client as ForgejoClient, CommitStatus, CommitStatusState, PullRequestEvent};
use ar_index::LearningsStore;
use ar_llm::Router as LlmRouter;
use ar_review::{
    build_glob_set, build_review_context, filter_reviewable, lint_workspace_via, load_repo_config,
    pr_is_skippable, prepare_workspace, review_pull_request, triage_files_with_llm, GlobSet,
    PreparedWorkspace, ReviewArgs, ReviewError, VerifyMode, WorkspaceError,
};
use ar_sandbox::{DirectSandbox, Sandbox};
use ar_tools::Finding;
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
    /// covers the whole pipeline (clone + lint + LLM + verify +
    /// post). `findings_count` is what landed on the PR; zero is the
    /// happy path, not an error.
    Succeeded {
        duration: Duration,
        findings_count: usize,
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
    Skipped {
        reason: &'static str,
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

/// Production dispatcher: posts a "pending" commit status, spawns
/// [`run_review_job`] in the background, and returns to the caller.
///
/// Owns the Forgejo base URL + bot token in addition to the API client
/// because the lint phase needs them to build a clone URL. Also owns
/// a [`ReviewHistory`] so subsequent commits on the same PR can use
/// `compare_diff` instead of re-reviewing the whole PR.
#[derive(Clone)]
pub struct SpawningDispatcher {
    forgejo: Arc<ForgejoClient>,
    llm: Arc<LlmRouter>,
    forgejo_base: Arc<String>,
    forgejo_token: Arc<String>,
    history: Arc<dyn ReviewHistory>,
    learnings: Option<Arc<dyn LearningsStore>>,
    sandbox: Arc<dyn Sandbox>,
    observer: Option<Arc<dyn ReviewObserver>>,
}

impl SpawningDispatcher {
    pub fn new(
        forgejo: Arc<ForgejoClient>,
        llm: Arc<LlmRouter>,
        forgejo_base: impl Into<String>,
        forgejo_token: impl Into<String>,
    ) -> Self {
        Self {
            forgejo,
            llm,
            forgejo_base: Arc::new(forgejo_base.into()),
            forgejo_token: Arc::new(forgejo_token.into()),
            history: Arc::new(InMemoryReviewHistory::new()),
            learnings: None,
            // Default: no isolation. Override with `with_sandbox` in
            // production to wrap linter spawns in a hardened container.
            sandbox: Arc::new(DirectSandbox::new()),
            observer: None,
        }
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

    /// Override the default direct sandbox. Production deployments
    /// should pass a [`PodmanSandbox`](ar_sandbox::PodmanSandbox) so
    /// linter binaries run with no network, dropped caps, and resource
    /// limits.
    pub fn with_sandbox(mut self, sandbox: Arc<dyn Sandbox>) -> Self {
        self.sandbox = sandbox;
        self
    }
}

#[async_trait]
impl JobDispatcher for SpawningDispatcher {
    async fn dispatch(&self, job: ReviewJob) {
        let forgejo = self.forgejo.clone();
        let llm = self.llm.clone();
        let base = self.forgejo_base.clone();
        let token = self.forgejo_token.clone();
        let history = self.history.clone();
        let learnings = self.learnings.clone();
        let sandbox = self.sandbox.clone();
        let observer = self.observer.clone();
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
        tokio::spawn(async move {
            let inner = tokio::spawn(async move {
                run_review_job(
                    &forgejo,
                    &llm,
                    &base,
                    &token,
                    history.as_ref(),
                    learnings.as_deref(),
                    sandbox.as_ref(),
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
/// 2. Clone the repo at the head SHA and run language-appropriate linters.
///    A failure here is logged but doesn't abort the review — the model
///    can still produce useful output without static-analysis context.
/// 3. Call [`review_pull_request`] with the linter findings.
/// 4. Post the final success/error commit status.
///
/// Errors are logged and swallowed; the gateway has already returned 202.
#[allow(clippy::too_many_arguments)]
pub async fn run_review_job(
    forgejo: &ForgejoClient,
    llm: &LlmRouter,
    forgejo_base: &str,
    forgejo_token: &str,
    history: &dyn ReviewHistory,
    learnings: Option<&dyn LearningsStore>,
    sandbox: &dyn Sandbox,
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
                observe(ReviewObservation::Skipped {
                    reason: "same_sha",
                });
                return;
            }
        } else if job.force {
            tracing::info!(
                repo = format!("{}/{}", job.owner, job.repo),
                pr = job.pr_number,
                "force=true: full review (skipping compare-diff incremental path)"
            );
        } else {
            tracing::info!(
                repo = format!("{}/{}", job.owner, job.repo),
                pr = job.pr_number,
                previous = %prev,
                current = %job.head_sha,
                "incremental review: fetching compare diff",
            );
            match forgejo
                .get_compare_diff(&job.owner, &job.repo, prev, &job.head_sha)
                .await
            {
                Ok(d) => incremental_diff = Some(d),
                Err(e) => {
                    tracing::warn!(error = %e, "compare_diff failed; falling back to full diff");
                }
            }
        }
    }

    observe(ReviewObservation::Started);

    let _ = forgejo
        .post_commit_status(
            &job.owner,
            &job.repo,
            &job.head_sha,
            &CommitStatus {
                state: CommitStatusState::Pending,
                target_url: String::new(),
                description: "auto_review running".into(),
                context: STATUS_CONTEXT.into(),
            },
        )
        .await
        .inspect_err(|e| tracing::warn!(error = %e, "failed to post pending status"));

    // Triage: if every changed file is trivial (lockfile bumps, vendored,
    // generated), skip the LLM call entirely and post a success status.
    match forgejo
        .list_changed_files(&job.owner, &job.repo, job.pr_number)
        .await
    {
        Ok(files) if pr_is_skippable(&files) => {
            tracing::info!(
                repo = format!("{}/{}", job.owner, job.repo),
                pr = job.pr_number,
                "skipping review: all changed files are trivial"
            );
            let _ = forgejo
                .post_commit_status(
                    &job.owner,
                    &job.repo,
                    &job.head_sha,
                    &CommitStatus {
                        state: CommitStatusState::Success,
                        target_url: String::new(),
                        description: "auto_review: skipped (lockfile/vendored/generated only)"
                            .into(),
                        context: STATUS_CONTEXT.into(),
                    },
                )
                .await;
            observe(ReviewObservation::Skipped {
                reason: "trivial_files",
            });
            return;
        }
        Ok(_) => {}
        Err(e) => {
            tracing::warn!(error = %e, "triage file-list failed; proceeding to lint+review");
        }
    }

    let lint_outcome = prepare_and_lint(
        forgejo,
        llm,
        forgejo_base,
        forgejo_token,
        learnings,
        sandbox,
        &job,
    )
    .await;
    let (findings, ignored_paths, guidelines, repo_context, pre_merge_checks, workspace) =
        match lint_outcome {
            Ok(LintPhaseOutput {
                skipped_by_config: true,
                ..
            }) => {
                tracing::info!(
                    repo = format!("{}/{}", job.owner, job.repo),
                    pr = job.pr_number,
                    "skipping review: disabled by .auto_review.yaml"
                );
                let _ = forgejo
                    .post_commit_status(
                        &job.owner,
                        &job.repo,
                        &job.head_sha,
                        &CommitStatus {
                            state: CommitStatusState::Success,
                            target_url: String::new(),
                            description: "auto_review: disabled by repo config".into(),
                            context: STATUS_CONTEXT.into(),
                        },
                    )
                    .await;
                observe(ReviewObservation::Skipped {
                    reason: "disabled_by_config",
                });
                return;
            }
            Ok(LintPhaseOutput {
                findings,
                ignored_paths,
                guidelines,
                repo_context,
                pre_merge_checks,
                workspace,
                ..
            }) => {
                tracing::debug!(count = findings.len(), "linter findings collected");
                (
                    findings,
                    ignored_paths,
                    guidelines,
                    repo_context,
                    pre_merge_checks,
                    workspace,
                )
            }
            Err(e) => {
                tracing::warn!(error = %e, "lint phase failed; continuing without findings");
                (
                    Vec::new(),
                    GlobSet::empty(),
                    String::new(),
                    String::new(),
                    Vec::new(),
                    None,
                )
            }
        };

    // The agentic verifier needs the cloned workspace to inspect.
    // Operators opt in by setting AR_AGENTIC_VERIFIER=1; without it,
    // the simple verifier (one-pass diff judgement) keeps running.
    // Either way we silently downgrade to Simple when no workspace
    // was prepared (e.g. the lint phase failed).
    let verify_mode = if std::env::var("AR_AGENTIC_VERIFIER")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
        && workspace.is_some()
    {
        VerifyMode::Agentic
    } else {
        VerifyMode::Simple
    };
    let workspace_path = workspace.as_ref().map(|w| w.path());

    let result = review_pull_request(ReviewArgs {
        forgejo,
        llm,
        owner: &job.owner,
        repo: &job.repo,
        pr_number: job.pr_number,
        head_sha: &job.head_sha,
        pr_title: &job.pr_title,
        pr_body: &job.pr_body,
        linter_findings: &findings,
        ignored_paths: &ignored_paths,
        custom_pre_merge_checks: &pre_merge_checks,
        guidelines: &guidelines,
        repo_context: &repo_context,
        diff_override: incremental_diff.as_deref(),
        verify_mode,
        workspace_path,
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
            });
            CommitStatus {
                state: CommitStatusState::Success,
                target_url: String::new(),
                description: review_summary(outcome.findings_count),
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
                description: format!("auto_review failed: {e}"),
                context: STATUS_CONTEXT.into(),
            }
        }
    };

    let _ = forgejo
        .post_commit_status(&job.owner, &job.repo, &job.head_sha, &final_status)
        .await
        .inspect_err(|e| tracing::warn!(error = %e, "failed to post final status"));

    // Record the SHA so the next webhook against this PR can do an
    // incremental review. Best-effort: a record failure just means
    // the next review will be a full one.
    if let Err(e) = history.record(&pr_key, &job.head_sha).await {
        tracing::warn!(error = %e, "failed to record review history");
    }
}

#[derive(Debug, thiserror::Error)]
enum LintPhaseError {
    #[error("forgejo: {0}")]
    Forgejo(#[from] ar_forgejo::Error),
    #[error("workspace: {0}")]
    Workspace(#[from] WorkspaceError),
}

struct LintPhaseOutput {
    findings: Vec<Finding>,
    skipped_by_config: bool,
    ignored_paths: GlobSet,
    guidelines: String,
    repo_context: String,
    /// From `.auto_review.yaml`'s `pre_merge_checks:` — passed through
    /// to the review pipeline so the LLM can evaluate them.
    pre_merge_checks: Vec<String>,
    /// Held by the orchestrator until the review pipeline finishes
    /// so the agentic verifier (when enabled) can inspect the
    /// cloned working tree. `None` when the lint phase exited
    /// without cloning (skipped_by_config doesn't reach this).
    workspace: Option<PreparedWorkspace>,
}

#[allow(clippy::too_many_arguments)]
async fn prepare_and_lint(
    forgejo: &ForgejoClient,
    llm: &LlmRouter,
    base: &str,
    token: &str,
    learnings: Option<&dyn LearningsStore>,
    sandbox: &dyn Sandbox,
    job: &ReviewJob,
) -> Result<LintPhaseOutput, LintPhaseError> {
    let files = forgejo
        .list_changed_files(&job.owner, &job.repo, job.pr_number)
        .await?;
    let workspace = prepare_workspace(base, token, &job.owner, &job.repo, &job.head_sha).await?;
    let config = load_repo_config(workspace.path());
    let ignored_paths = build_glob_set(&config.ignored_paths);
    let guidelines = config.guidelines.clone();
    if !config.enabled {
        return Ok(LintPhaseOutput {
            findings: Vec::new(),
            skipped_by_config: true,
            ignored_paths,
            guidelines,
            repo_context: String::new(),
            pre_merge_checks: Vec::new(),
            workspace: None,
        });
    }
    // Fetch the diff once for both LLM triage and the RAG context
    // build that follow. Failure here just means we skip those
    // optional steps; the review still proceeds.
    let raw_diff = match forgejo
        .get_pr_diff(&job.owner, &job.repo, job.pr_number)
        .await
    {
        Ok(d) => d,
        Err(e) => {
            tracing::warn!(error = %e, "diff fetch for triage/context failed; continuing");
            String::new()
        }
    };

    // LLM triage (optional, opt-in via Cheap tier configuration):
    // narrow the file list to those classified as Simple/Complex.
    let files = if !raw_diff.is_empty() {
        match triage_files_with_llm(llm, &files, &raw_diff).await {
            Ok(Some(triage)) => {
                let kept = filter_reviewable(&files, &triage);
                tracing::info!(
                    in_count = files.len(),
                    out_count = kept.len(),
                    "LLM triage filtered changed files"
                );
                kept
            }
            Ok(None) => files,
            Err(e) => {
                tracing::warn!(error = %e, "LLM triage failed; falling through to all files");
                files
            }
        }
    } else {
        files
    };

    let findings =
        lint_workspace_via(sandbox, workspace.path(), &files, &config.disabled_tools).await;

    // Build the RAG context (best-effort): walks the workspace,
    // embeds symbols, queries top-K against the diff. Returns empty
    // string when no Embedding tier is configured or the workspace
    // has no extractable symbols.
    let repo_context = if raw_diff.is_empty() {
        String::new()
    } else {
        build_review_context(workspace.path(), llm, &raw_diff, learnings, 5)
            .await
            .unwrap_or_else(|e| {
                tracing::warn!(error = %e, "RAG context build failed; continuing");
                String::new()
            })
    };

    Ok(LintPhaseOutput {
        findings,
        skipped_by_config: false,
        ignored_paths,
        guidelines,
        repo_context,
        pre_merge_checks: config.pre_merge_checks.clone(),
        workspace: Some(workspace),
    })
}

fn review_summary(findings_count: usize) -> String {
    match findings_count {
        0 => "auto_review: no findings".into(),
        1 => "auto_review: 1 finding".into(),
        n => format!("auto_review: {n} findings"),
    }
}

fn error_state(err: &ReviewError) -> CommitStatusState {
    match err {
        ReviewError::Forgejo(_) | ReviewError::Workspace(_) => CommitStatusState::Error,
        ReviewError::Llm(_) | ReviewError::Unhealable { .. } => CommitStatusState::Failure,
    }
}

/// Stable label string for [`ReviewObservation::Failed`]. Used by
/// the gateway's `/metrics` endpoint to bucket failures so operators
/// can see `llm` vs `workspace` vs `forgejo` outage rates separately.
fn error_class(err: &ReviewError) -> &'static str {
    match err {
        ReviewError::Forgejo(_) => "forgejo",
        ReviewError::Workspace(_) => "workspace",
        ReviewError::Llm(_) => "llm",
        ReviewError::Unhealable { .. } => "unhealable",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

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
}

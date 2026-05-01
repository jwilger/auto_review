use ar_forgejo::{Client as ForgejoClient, CommitStatus, CommitStatusState, PullRequestEvent};
use ar_llm::Router as LlmRouter;
use ar_review::{
    build_glob_set, lint_workspace_with, load_repo_config, pr_is_skippable, prepare_workspace,
    review_pull_request, GlobSet, ReviewArgs, ReviewError, WorkspaceError,
};
use ar_tools::Finding;
use async_trait::async_trait;
use std::sync::Arc;

const STATUS_CONTEXT: &str = "auto_review";

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
/// because the lint phase needs them to build a clone URL.
#[derive(Clone)]
pub struct SpawningDispatcher {
    forgejo: Arc<ForgejoClient>,
    llm: Arc<LlmRouter>,
    forgejo_base: Arc<String>,
    forgejo_token: Arc<String>,
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
        }
    }
}

#[async_trait]
impl JobDispatcher for SpawningDispatcher {
    async fn dispatch(&self, job: ReviewJob) {
        let forgejo = self.forgejo.clone();
        let llm = self.llm.clone();
        let base = self.forgejo_base.clone();
        let token = self.forgejo_token.clone();
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
                run_review_job(&forgejo, &llm, &base, &token, job).await;
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
pub async fn run_review_job(
    forgejo: &ForgejoClient,
    llm: &LlmRouter,
    forgejo_base: &str,
    forgejo_token: &str,
    job: ReviewJob,
) {
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
            return;
        }
        Ok(_) => {}
        Err(e) => {
            tracing::warn!(error = %e, "triage file-list failed; proceeding to lint+review");
        }
    }

    let lint_outcome = prepare_and_lint(forgejo, forgejo_base, forgejo_token, &job).await;
    let (findings, ignored_paths, guidelines) = match lint_outcome {
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
            return;
        }
        Ok(LintPhaseOutput {
            findings,
            ignored_paths,
            guidelines,
            ..
        }) => {
            tracing::debug!(count = findings.len(), "linter findings collected");
            (findings, ignored_paths, guidelines)
        }
        Err(e) => {
            tracing::warn!(error = %e, "lint phase failed; continuing without findings");
            (Vec::new(), GlobSet::empty(), String::new())
        }
    };

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
        guidelines: &guidelines,
        // RAG-retrieved context lands here once build_review_context
        // is wired in.
        repo_context: "",
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
}

async fn prepare_and_lint(
    forgejo: &ForgejoClient,
    base: &str,
    token: &str,
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
        });
    }
    let findings = lint_workspace_with(workspace.path(), &files, &config.disabled_tools).await;
    Ok(LintPhaseOutput {
        findings,
        skipped_by_config: false,
        ignored_paths,
        guidelines,
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

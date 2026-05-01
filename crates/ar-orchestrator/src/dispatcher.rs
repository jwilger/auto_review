use ar_forgejo::{Client as ForgejoClient, CommitStatus, CommitStatusState, PullRequestEvent};
use ar_llm::Router as LlmRouter;
use ar_review::{review_pull_request, ReviewError};
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
#[derive(Clone)]
pub struct SpawningDispatcher {
    forgejo: Arc<ForgejoClient>,
    llm: Arc<LlmRouter>,
}

impl SpawningDispatcher {
    pub fn new(forgejo: Arc<ForgejoClient>, llm: Arc<LlmRouter>) -> Self {
        Self { forgejo, llm }
    }
}

#[async_trait]
impl JobDispatcher for SpawningDispatcher {
    async fn dispatch(&self, job: ReviewJob) {
        let forgejo = self.forgejo.clone();
        let llm = self.llm.clone();
        tokio::spawn(async move {
            run_review_job(&forgejo, &llm, job).await;
        });
    }
}

/// Run one review job to completion.
///
/// Wraps [`review_pull_request`] with commit-status posting (pending → success
/// or error). Logs and swallows errors — the gateway has already returned 202,
/// so failures here only affect the resulting PR status badge and logs.
pub async fn run_review_job(forgejo: &ForgejoClient, llm: &LlmRouter, job: ReviewJob) {
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

    let result = review_pull_request(
        forgejo,
        llm,
        &job.owner,
        &job.repo,
        job.pr_number,
        &job.head_sha,
        &job.pr_title,
        &job.pr_body,
        // TODO: clone the repo and run ar-tools::run_all here.
        &[],
    )
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

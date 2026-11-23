//! Reconcile an eligible GitHub PR revision against the checks GitHub exposes.

#![expect(
    clippy::pub_with_shorthand,
    reason = "rustfmt uses shorthand for crate scope"
)]

use crate::metrics::Metrics;
use crate::webhook::is_safe_repo_path_segment;
use crate::{AppState, GitHubDeps};
use alloc::sync::Arc;
use ar_github::{InstallationRef, WebhookRepository};
use ar_orchestrator::ReviewJob;
use core::iter::once;
use core::time::Duration;
use serde::Deserialize;
use std::collections::HashSet;
use tokio::sync::{oneshot, Mutex};
use tokio::time::{sleep, timeout, Instant};

/// Bound a single readiness query, including pagination.
const API_TIMEOUT: Duration = Duration::from_secs(25);
/// Limit the number of outstanding PR revisions per gateway.
const MAX_CANDIDATES: usize = 512;
/// Maximum interval between retries.
const MAX_RETRY_DELAY: Duration = Duration::from_secs(120);
/// Give CI time to register after the PR event.
const SETTLE: Duration = if cfg!(test) {
    Duration::from_millis(25)
} else {
    Duration::from_secs(30)
};
/// How long one delivery can wait for checks to pass.
const WATCH_WINDOW: Duration = Duration::from_hours(2);

/// In-flight webhook revisions, shared by requests to this gateway.
#[derive(Default)]
pub(crate) struct Candidates(Mutex<HashSet<Revision>>);

/// PR head in a webhook delivery.
#[derive(Deserialize)]
struct Head {
    /// Current commit reported by GitHub.
    sha: String,
}

/// Eligibility fields in a webhook delivery.
#[derive(Deserialize)]
struct PullRequest {
    /// Whether automatic review should ignore this PR.
    draft: bool,
    /// Current source revision.
    head: Head,
}

/// Signed GitHub App pull-request delivery.
#[derive(Deserialize)]
pub(crate) struct PullRequestEvent {
    /// Pull-request lifecycle action.
    action: String,
    /// Installation on the base repository.
    installation: InstallationRef,
    /// Pull-request number in the base repository.
    number: u64,
    /// Pull-request details.
    pull_request: PullRequest,
    /// Base repository, even for a fork PR.
    repository: WebhookRepository,
}

/// Identity of one candidate review.
#[derive(Clone, Eq, Hash, PartialEq)]
struct Revision {
    /// Pull-request number in the repository.
    number: u64,
    /// Base repository owner.
    owner: String,
    /// Base repository name.
    repo: String,
    /// Source commit to review.
    sha: String,
}

/// Result of checking readiness after a worker slot opens.
enum Admission {
    /// The current job was handed to the worker.
    Dispatched,
    /// The PR moved on or its watch expired.
    Obsolete,
    /// The watch should continue with this known merge blocker, if any.
    Retry(Option<String>),
}

/// What the current PR and its checks allow.
enum Readiness {
    /// The revision is no longer open, non-draft and current.
    Obsolete,
    /// Checks permit dispatch.
    Ready(ReviewJob),
    /// Try again after backing off.
    Waiting,
}

/// Recheck a queued job immediately after it acquires a worker slot.
async fn admit(
    github: &GitHubDeps,
    revision: &Revision,
    mut blocked_merge: Option<String>,
    started: Instant,
) -> Admission {
    let (notify, result) = oneshot::channel();
    let gateway = github.clone();
    let candidate = revision.clone();
    github
        .dispatcher
        .dispatch_when_ready(Box::pin(async move {
            let remaining = WATCH_WINDOW.saturating_sub(started.elapsed());
            if remaining.is_zero() || notify.is_closed() {
                return None;
            }
            let (admission, job) = match timeout(
                API_TIMEOUT.min(remaining),
                reconcile(&gateway, &candidate, &mut blocked_merge),
            )
            .await
            {
                Ok(Ok(Readiness::Ready(job))) => (Admission::Dispatched, Some(job)),
                Ok(Ok(Readiness::Obsolete)) => (Admission::Obsolete, None),
                _ => (Admission::Retry(blocked_merge), None),
            };
            if notify.send(admission).is_ok() {
                job
            } else {
                None
            }
        }))
        .await;
    let remaining = WATCH_WINDOW.saturating_sub(started.elapsed());
    match timeout(remaining, result).await {
        Ok(Ok(outcome)) => outcome,
        _ => Admission::Obsolete,
    }
}

/// Inspect live PR metadata and checks before creating a job.
async fn reconcile(
    github: &GitHubDeps,
    revision: &Revision,
    blocked_merge: &mut Option<String>,
) -> Result<Readiness, ar_github::Error> {
    let client = github.host.client();
    let owner = &revision.owner;
    let repo = &revision.repo;
    let number = revision.number;
    let token = github.host.readiness_token_for(owner, repo).await?;
    let (pr, merge_sha) = client
        .get_pull_request_and_merge_sha(owner, repo, number, &token)
        .await?;
    if pr.draft || pr.state != "open" || pr.head.sha != revision.sha {
        return Ok(Readiness::Obsolete);
    }
    // A base branch update can replace the test merge SHA without rerunning
    // CI. Empty checks on the new SHA do not clear a known blocker.
    let previous_blocker = blocked_merge.clone();
    let refs = once(revision.sha.as_str())
        .chain(previous_blocker.as_deref())
        .chain(
            merge_sha
                .as_deref()
                .filter(|sha| *sha != revision.sha && Some(*sha) != previous_blocker.as_deref()),
        );
    for sha in refs {
        if !valid_sha(sha) {
            return Ok(Readiness::Waiting);
        }
        if !client
            .checks_allow_review(owner, repo, sha, &github.bot_login, &token)
            .await?
        {
            if sha != revision.sha {
                *blocked_merge = Some(sha.to_owned());
            }
            return Ok(Readiness::Waiting);
        }
        if Some(sha) == previous_blocker.as_deref() {
            *blocked_merge = None;
        }
    }
    // Checks took network round trips: make sure the reviewed revision and
    // merge ref are still current before spending model budget.
    let (current_pr, current_merge_sha) = client
        .get_pull_request_and_merge_sha(owner, repo, number, &token)
        .await?;
    if current_pr.draft || current_pr.state != "open" || current_pr.head.sha != revision.sha {
        return Ok(Readiness::Obsolete);
    }
    if merge_sha != current_merge_sha || pr.base.sha != current_pr.base.sha {
        return Ok(Readiness::Waiting);
    }
    Ok(Readiness::Ready(ReviewJob {
        owner: revision.owner.clone(),
        repo: revision.repo.clone(),
        pr_number: number,
        head_sha: revision.sha.clone(),
        pr_title: current_pr.title,
        pr_body: current_pr.body,
        base_sha: Some(current_pr.base.sha),
        force: false,
    }))
}

/// Start one background watch for an eligible PR revision.
pub(crate) async fn schedule(state: &AppState, github: GitHubDeps, evt: PullRequestEvent) {
    if !matches!(
        evt.action.as_str(),
        "opened" | "reopened" | "synchronize" | "ready_for_review"
    ) || evt.pull_request.draft
        || !is_safe_repo_path_segment(&evt.repository.owner.login)
        || !is_safe_repo_path_segment(&evt.repository.name)
        || !valid_sha(&evt.pull_request.head.sha)
    {
        return;
    }

    let revision = Revision {
        owner: evt.repository.owner.login,
        repo: evt.repository.name,
        number: evt.number,
        sha: evt.pull_request.head.sha,
    };
    let inserted = {
        let mut active = github.review_candidates.0.lock().await;
        if active.contains(&revision) {
            return;
        }
        if active.len() >= MAX_CANDIDATES {
            tracing::warn!(owner = %revision.owner, repo = %revision.repo, pr = revision.number,
                "GitHub review readiness queue is full");
            return;
        }
        active.insert(revision.clone())
    };
    debug_assert!(
        inserted,
        "the locked candidate set did not contain this revision"
    );

    let metrics = Arc::clone(&state.metrics);
    tokio::spawn(async move {
        watch(&github, &metrics, &revision, evt.installation.id).await;
        github.review_candidates.0.lock().await.remove(&revision);
    });
}

/// Reject non-hex values before interpolating a SHA into an API path.
fn valid_sha(sha: &str) -> bool {
    !sha.is_empty() && sha.bytes().all(|byte| byte.is_ascii_hexdigit())
}

/// Retry only within the watch window, stopping if the PR moves on.
async fn watch(github: &GitHubDeps, metrics: &Metrics, revision: &Revision, installation: u64) {
    github
        .host
        .remember_installation(&revision.owner, &revision.repo, installation)
        .await;
    let started = Instant::now();
    let mut delay = SETTLE;
    let mut blocked_merge = None;
    while started.elapsed().saturating_add(delay) < WATCH_WINDOW {
        sleep(delay).await;
        match timeout(API_TIMEOUT, reconcile(github, revision, &mut blocked_merge)).await {
            Ok(Ok(Readiness::Obsolete)) => return,
            Ok(Ok(Readiness::Ready(_))) => {
                match admit(github, revision, blocked_merge.take(), started).await {
                    Admission::Dispatched => {
                        metrics.record_job_dispatched();
                        return;
                    }
                    Admission::Obsolete => return,
                    Admission::Retry(blocker) => blocked_merge = blocker,
                }
            }
            Ok(Ok(Readiness::Waiting)) => (),
            Ok(Err(_)) | Err(_) => tracing::debug!(
                owner = %revision.owner, repo = %revision.repo, pr = revision.number,
                "GitHub review readiness could not be read; will retry"
            ),
        }
        delay = delay.saturating_mul(2).min(MAX_RETRY_DELAY);
    }
    tracing::info!(owner = %revision.owner, repo = %revision.repo, pr = revision.number,
        sha = %revision.sha, "GitHub review readiness window expired");
}

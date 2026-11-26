//! Reconcile an eligible GitHub PR revision against the checks GitHub exposes.

#![expect(
    clippy::pub_with_shorthand,
    reason = "rustfmt uses shorthand for crate scope"
)]

use crate::metrics::Metrics;
use crate::webhook::is_safe_repo_path_segment;
use crate::{AppState, GitHubDeps};
use alloc::sync::Arc;
use ar_forge::PullRequestSummary;
use ar_github::{CheckProgress, InstallationRef, ReviewWaitEnd, WebhookRepository};
use ar_orchestrator::ReviewJob;
use ar_review::config::{CheckRequirement, GitHubAutomaticReview};
use ar_review::load_repo_config_from_base;
use core::time::Duration;
use serde::Deserialize;
use std::collections::{HashMap, HashSet};
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
pub(crate) struct Candidates(Mutex<HashMap<Revision, Option<u64>>>);

#[cfg(test)]
impl Candidates {
    pub(crate) async fn is_empty(&self) -> bool {
        self.0.lock().await.is_empty()
    }
}

/// PR head in a webhook delivery.
#[derive(Deserialize)]
struct Head {
    /// Current commit reported by GitHub.
    sha: String,
}

/// Eligibility fields in a webhook delivery.
#[derive(Deserialize)]
struct PullRequest {
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
    /// The worker slot did not open in time.
    Expired,
    /// The PR moved on.
    Obsolete,
    /// The watch should continue.
    Retry,
}

/// Base branch and its repository-controlled admission policy for this watch.
#[derive(Clone, Eq, PartialEq)]
struct Policy {
    /// Discard the watch if the PR is retargeted.
    base: String,
    /// Policy read from the trusted branch, not the proposed changes.
    config: GitHubAutomaticReview,
}

impl Policy {
    /// Verify the PR is still eligible under the loaded policy.
    fn permits(&self, pr: &PullRequestSummary, revision: &Revision) -> bool {
        pr.state == "open"
            && pr.head.sha == revision.sha
            && pr.base.ref_name == self.base
            && (!pr.draft || self.config.allow_drafts)
    }
}

/// What the current PR and its checks allow.
enum Readiness {
    /// The revision no longer satisfies the live PR eligibility rules.
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
    policy: &Policy,
    started: Instant,
) -> Admission {
    let (notify, result) = oneshot::channel();
    let gateway = github.clone();
    let candidate = revision.clone();
    let admission_policy = policy.clone();
    github
        .dispatcher
        .dispatch_when_ready(Box::pin(async move {
            let remaining = WATCH_WINDOW.saturating_sub(started.elapsed());
            if remaining.is_zero() || notify.is_closed() {
                return None;
            }
            let checked = timeout(API_TIMEOUT.min(remaining), async {
                if load_policy(&gateway, &candidate).await?.as_ref() != Some(&admission_policy) {
                    return Ok(Readiness::Obsolete);
                }
                reconcile(&gateway, &candidate, &admission_policy)
                    .await
                    .map_err(|error| {
                        tracing::debug!(%error, "Could not confirm GitHub review admission");
                    })
            })
            .await;
            let (admission, job) = match checked {
                Ok(Ok(Readiness::Ready(job))) => (Admission::Dispatched, Some(job)),
                Ok(Ok(Readiness::Obsolete)) => (Admission::Obsolete, None),
                _ => (Admission::Retry, None),
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
        Ok(Err(_)) if !remaining.is_zero() => Admission::Retry,
        _ => Admission::Expired,
    }
}

/// Inspect live PR metadata and checks before creating a job.
async fn reconcile(
    github: &GitHubDeps,
    revision: &Revision,
    policy: &Policy,
) -> Result<Readiness, ar_github::Error> {
    if policy.config.required_checks.is_empty() {
        let pr = fetch_pr(github, revision).await?;
        return Ok(readiness_from_pr(revision, policy, pr));
    }
    let client = github.host.client();
    let owner = &revision.owner;
    let repo = &revision.repo;
    let number = revision.number;
    let token = github.host.readiness_token_for(owner, repo).await?;
    let (pr, merge_sha) = client
        .get_pull_request_and_merge_sha(owner, repo, number, &token)
        .await?;
    if !policy.permits(&pr, revision) {
        return Ok(Readiness::Obsolete);
    }
    let head = revision.sha.as_str();
    let refs = [Some(head), merge_sha.as_deref().filter(|sha| *sha != head)];
    let mut observed = HashSet::new();
    for sha in refs.into_iter().flatten() {
        if !valid_sha(sha) {
            return Ok(Readiness::Waiting);
        }
        let signals = client.check_progress(owner, repo, sha, &token).await?;
        if !requirements_met(&policy.config, &signals, &mut observed) {
            return Ok(Readiness::Waiting);
        }
    }
    if observed.len() != policy.config.required_checks.len() {
        return Ok(Readiness::Waiting);
    }
    // Checks took network round trips: make sure the reviewed revision and
    // merge ref are still current before spending model budget.
    let (current_pr, current_merge_sha) = client
        .get_pull_request_and_merge_sha(owner, repo, number, &token)
        .await?;
    if !policy.permits(&current_pr, revision) {
        return Ok(Readiness::Obsolete);
    }
    if merge_sha != current_merge_sha || pr.base.sha != current_pr.base.sha {
        return Ok(Readiness::Waiting);
    }
    Ok(readiness_from_pr(revision, policy, current_pr))
}

/// Match only configured names, requiring all matching signals to satisfy the policy.
fn requirements_met(
    policy: &GitHubAutomaticReview,
    signals: &[(String, CheckProgress)],
    observed: &mut HashSet<String>,
) -> bool {
    signals.iter().all(|signal| {
        let name = &signal.0;
        let Some(requirement) = policy.required_checks.get(name) else {
            return true;
        };
        observed.insert(name.clone());
        matches!(
            (*requirement, signal.1),
            (_, CheckProgress::Passed) | (CheckRequirement::Complete, CheckProgress::Complete)
        )
    })
}

/// Construct the job only for the eligible current revision.
fn readiness_from_pr(revision: &Revision, policy: &Policy, pr: PullRequestSummary) -> Readiness {
    if !policy.permits(&pr, revision) {
        return Readiness::Obsolete;
    }
    Readiness::Ready(ReviewJob {
        owner: revision.owner.clone(),
        repo: revision.repo.clone(),
        pr_number: revision.number,
        head_sha: revision.sha.clone(),
        pr_title: pr.title,
        pr_body: pr.body,
        base_sha: Some(pr.base.sha),
        force: false,
    })
}

/// Start one background watch for an eligible PR revision.
pub(crate) async fn schedule(state: &AppState, github: GitHubDeps, evt: PullRequestEvent) {
    if !matches!(
        evt.action.as_str(),
        "opened" | "reopened" | "synchronize" | "ready_for_review"
    ) || !is_safe_repo_path_segment(&evt.repository.owner.login)
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
        if let Some(retry) = active.get_mut(&revision) {
            // The same SHA can become eligible during a live metadata read.
            if matches!(evt.action.as_str(), "ready_for_review" | "reopened") {
                *retry = Some(evt.installation.id);
            }
            return;
        }
        if active.len() >= MAX_CANDIDATES {
            tracing::warn!(owner = %revision.owner, repo = %revision.repo, pr = revision.number,
                "GitHub review readiness queue is full");
            return;
        }
        active.insert(revision.clone(), None).is_none()
    };
    debug_assert!(
        inserted,
        "the locked candidate set did not contain this revision"
    );

    let metrics = Arc::clone(&state.metrics);
    tokio::spawn(async move {
        let mut installation = evt.installation.id;
        loop {
            let dispatched = watch(&github, &metrics, &revision, installation).await;
            let mut active = github.review_candidates.0.lock().await;
            let retry = if dispatched {
                None
            } else {
                active.get_mut(&revision).and_then(Option::take)
            };
            if let Some(next_installation) = retry {
                installation = next_installation;
            } else {
                active.remove(&revision);
                break;
            }
        }
    });
}

/// Reject non-hex values before interpolating a SHA into an API path.
fn valid_sha(sha: &str) -> bool {
    !sha.is_empty() && sha.bytes().all(|byte| byte.is_ascii_hexdigit())
}

/// Fetch PR metadata without requesting check permissions.
async fn fetch_pr(
    github: &GitHubDeps,
    revision: &Revision,
) -> Result<PullRequestSummary, ar_github::Error> {
    let owner = &revision.owner;
    let repo = &revision.repo;
    let token = github.host.installation_token_for(owner, repo).await?;
    github
        .host
        .client()
        .get_pull_request(owner, repo, revision.number, &token)
        .await
}

/// Read the policy once per watch from the live PR's trusted base branch.
async fn load_policy(github: &GitHubDeps, revision: &Revision) -> Result<Option<Policy>, ()> {
    let pr = fetch_pr(github, revision).await.map_err(|error| {
        tracing::debug!(%error, "Could not read GitHub PR metadata for admission");
    })?;
    if pr.state != "open" || pr.head.sha != revision.sha {
        return Ok(None);
    }
    let base = pr.base.ref_name.clone();
    let loaded =
        load_repo_config_from_base(github.host.as_ref(), &revision.owner, &revision.repo, &base)
            .await;
    if loaded.warning.is_some() {
        return Err(());
    }
    let config = match loaded.config.github_automatic_review() {
        Ok(config) => config,
        Err(error) => {
            tracing::warn!(owner = %revision.owner, repo = %revision.repo, pr = revision.number, %error,
                "Skipping automatic review because the base-branch admission policy is invalid");
            return Ok(None);
        }
    };
    let policy = Policy { base, config };
    Ok(policy.permits(&pr, revision).then_some(policy))
}

/// Retry unavailable metadata and config without ever falling back to ungated dispatch.
async fn wait_for_policy(
    github: &GitHubDeps,
    revision: &Revision,
    started: Instant,
) -> Option<Policy> {
    let mut delay = SETTLE;
    loop {
        let remaining = WATCH_WINDOW.saturating_sub(started.elapsed());
        if remaining.is_zero() {
            return None;
        }
        if let Ok(Ok(policy)) =
            timeout(API_TIMEOUT.min(remaining), load_policy(github, revision)).await
        {
            return policy;
        }
        tracing::debug!(owner = %revision.owner, repo = %revision.repo, pr = revision.number,
            "GitHub admission policy could not be read; will retry");
        sleep(delay.min(WATCH_WINDOW.saturating_sub(started.elapsed()))).await;
        delay = delay.saturating_mul(2).min(MAX_RETRY_DELAY);
    }
}

/// Retry only within the watch window, stopping if the PR moves on.
async fn watch(
    github: &GitHubDeps,
    metrics: &Metrics,
    revision: &Revision,
    installation: u64,
) -> bool {
    github
        .host
        .remember_installation(&revision.owner, &revision.repo, installation)
        .await;
    let started = Instant::now();
    let Some(policy) = wait_for_policy(github, revision, started).await else {
        return false;
    };
    let pending_id = if policy.config.required_checks.is_empty() {
        None
    } else {
        let write = github.host.update_review_wait_status(
            &revision.owner,
            &revision.repo,
            &revision.sha,
            &github.bot_login,
            None,
        );
        if let Ok(id) = write.await {
            id
        } else {
            tracing::warn!(owner = %revision.owner, repo = %revision.repo, pr = revision.number,
                "Could not publish the GitHub wait status");
            None
        }
    };
    if let Some(reason) = wait_and_dispatch(github, metrics, revision, &policy, started).await {
        report_wait_end(github, revision, pending_id, reason).await;
        return false;
    }
    true
}

/// Reconcile and dispatch, returning why the watch stopped if it did not dispatch.
async fn wait_and_dispatch(
    github: &GitHubDeps,
    metrics: &Metrics,
    revision: &Revision,
    policy: &Policy,
    started: Instant,
) -> Option<ReviewWaitEnd> {
    let mut delay = if policy.config.required_checks.is_empty() {
        Duration::ZERO
    } else {
        SETTLE.saturating_sub(started.elapsed())
    };
    loop {
        sleep(delay.min(WATCH_WINDOW.saturating_sub(started.elapsed()))).await;
        let remaining = WATCH_WINDOW.saturating_sub(started.elapsed());
        if remaining.is_zero() {
            return Some(ReviewWaitEnd::TimedOut);
        }
        match timeout(
            API_TIMEOUT.min(remaining),
            reconcile(github, revision, policy),
        )
        .await
        {
            Ok(Ok(Readiness::Obsolete)) => return Some(ReviewWaitEnd::Canceled),
            Ok(Ok(Readiness::Ready(_))) => match admit(github, revision, policy, started).await {
                Admission::Dispatched => {
                    metrics.record_job_dispatched();
                    return None;
                }
                Admission::Obsolete => return Some(ReviewWaitEnd::Canceled),
                Admission::Expired => return Some(ReviewWaitEnd::TimedOut),
                Admission::Retry => (),
            },
            Ok(Ok(Readiness::Waiting)) => (),
            Ok(Err(_)) | Err(_) => tracing::debug!(
                owner = %revision.owner, repo = %revision.repo, pr = revision.number,
                "GitHub review readiness could not be read; will retry"
            ),
        }
        delay = delay.max(SETTLE).saturating_mul(2).min(MAX_RETRY_DELAY);
    }
}

/// Retire the watch's own pending status without overwriting a newer review.
async fn report_wait_end(
    github: &GitHubDeps,
    revision: &Revision,
    pending_id: Option<u64>,
    reason: ReviewWaitEnd,
) {
    if let Some(id) = pending_id {
        let write = github.host.update_review_wait_status(
            &revision.owner,
            &revision.repo,
            &revision.sha,
            &github.bot_login,
            Some((id, reason)),
        );
        if write.await.is_err() {
            tracing::warn!(owner = %revision.owner, repo = %revision.repo, pr = revision.number,
                "Could not publish the terminal GitHub watch status");
        }
    }
    tracing::info!(owner = %revision.owner, repo = %revision.repo, pr = revision.number,
        sha = %revision.sha, ?reason, "GitHub review watch ended without dispatch");
}

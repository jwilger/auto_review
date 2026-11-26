//! Admission from signed Forgejo PR lifecycle deliveries.

#![expect(
    clippy::pub_with_shorthand,
    reason = "rustfmt uses shorthand for crate scope"
)]

use crate::webhook::is_safe_repo_path_segment;
use crate::{AppState, ForgejoDeps};
use alloc::sync::Arc;
use ar_forge::PullRequestSummary;
use ar_forgejo::{ActionJobProgress, Client, PullRequestEvent, ReviewWaitEnd};
use ar_orchestrator::ReviewJob;
use ar_review::config::{AutomaticReview, CheckRequirement, ForgejoTrigger};
use ar_review::load_repo_config_from_base;
use ar_review::trusted_files::{ConfigProvenance, REPO_CONFIG_FILE_NAMES};
use core::{mem, time::Duration};
use std::collections::{HashMap, HashSet};
use tokio::sync::{oneshot, Mutex};
use tokio::time::{sleep, timeout, Instant};

/// Bounds a sequence of admission API reads.
const API_TIMEOUT: Duration = Duration::from_secs(25);
/// Maximum number of concurrent distinct revision watches.
const MAX_CANDIDATES: usize = 512;
/// Largest delay between polls.
const MAX_RETRY_DELAY: Duration = Duration::from_secs(120);
/// Maximum number of branch opt-ins remembered by a gateway.
const MAX_TRIGGER_OPT_INS: usize = 4096;
/// Grace period for Actions to register jobs.
const SETTLE: Duration = if cfg!(test) {
    Duration::from_millis(25)
} else {
    Duration::from_secs(30)
};
/// Lifetime of an individual admission watch.
const WATCH_WINDOW: Duration = Duration::from_hours(2);

/// Coalesce concurrent candidate deliveries, retaining a possible readiness transition.
#[derive(Default)]
pub(crate) struct Candidates(Mutex<HashMap<Revision, bool>>);

#[cfg(test)]
impl Candidates {
    /// Whether all scheduled work has exited.
    pub(crate) async fn is_empty(&self) -> bool {
        self.0.lock().await.is_empty()
    }
}

/// Shared by callback intake, callback worker admission, and webhook watches.
#[derive(Default)]
pub(crate) struct TriggerHistory(Mutex<TriggerOptIns>);

/// Base repository and exact target branch, independent of PR or revision.
#[derive(Eq, Hash, PartialEq)]
struct Target {
    /// Live base branch name.
    base: String,
    /// Case-folded repository owner.
    owner: String,
    /// Case-folded repository name.
    repo: String,
}

/// A positive webhook observation, including which file must be superseded.
struct OptIn {
    /// When the most recent positive read was recorded.
    observed: Instant,
    /// Whether the higher-priority `.yaml` file reported webhook mode.
    primary: bool,
}

/// Bounded store of positive webhook observations.
#[derive(Default)]
struct TriggerOptIns {
    /// Remembered branch opt-ins.
    branches: HashMap<Target, OptIn>,
    /// Whether the capacity warning has already been logged.
    warned_at_capacity: bool,
}

/// A fresh observation, distinct from a loader fallback.
enum PolicyRead {
    /// Config was parsed but the automatic policy did not validate.
    Invalid,
    /// A usable policy, including defaults when no file was found.
    Readable(AutomaticReview),
    /// The loader could not fetch or parse trusted config.
    Unreadable,
}

/// Fresh policy for the watcher and the resulting callback veto.
pub(crate) struct TriggerRead {
    /// Only a fresh policy can enable a webhook review.
    policy: PolicyRead,
    /// A callback must defer to either a fresh or remembered webhook opt-in.
    veto_callback: bool,
}

impl TriggerRead {
    /// Whether a callback must be suppressed.
    pub(crate) const fn veto_callback(&self) -> bool {
        self.veto_callback
    }
}

impl TriggerHistory {
    /// Resolve trusted policy and update the callback veto as one observation.
    pub(crate) async fn load(
        &self,
        host: &Client,
        owner: &str,
        repo: &str,
        base: &str,
    ) -> TriggerRead {
        let started = Instant::now();
        let loaded = load_repo_config_from_base(host, owner, repo, base).await;
        let webhook_requested =
            loaded.warning.is_none() && loaded.config.forgejo_webhook_requested();
        let policy = if loaded.warning.is_some() {
            PolicyRead::Unreadable
        } else {
            match loaded.config.automatic_review() {
                Ok(config) => PolicyRead::Readable(config),
                Err(_) => PolicyRead::Invalid,
            }
        };
        #[expect(
            clippy::rest_pattern_accessible_field,
            reason = "The loader already knows the base ref; only file precedence matters here"
        )]
        let file = match loaded.provenance {
            ConfigProvenance::BaseBranch { path, .. } => Some(path == REPO_CONFIG_FILE_NAMES[0]),
            ConfigProvenance::Default => None,
        };
        let target = Target {
            base: base.to_owned(),
            owner: owner.to_ascii_lowercase(),
            repo: repo.to_ascii_lowercase(),
        };
        let veto_callback =
            self.0
                .lock()
                .await
                .observe(target, started, &policy, file, webhook_requested);
        TriggerRead {
            policy,
            veto_callback,
        }
    }
}

impl TriggerOptIns {
    /// Retain positive evidence across failures, including ambiguous raw-file 404s.
    fn observe(
        &mut self,
        target: Target,
        started: Instant,
        policy: &PolicyRead,
        file: Option<bool>,
        webhook_requested: bool,
    ) -> bool {
        let known = self.branches.get_mut(&target);
        // A bad check requirement cannot undo an explicit trusted cutover.
        if webhook_requested {
            let primary = file == Some(true);
            let observed = Instant::now();
            if let Some(opt_in) = known {
                opt_in.observed = observed;
                opt_in.primary |= primary;
            } else if self.branches.len() < MAX_TRIGGER_OPT_INS {
                let _prior = self.branches.insert(target, OptIn { observed, primary });
            } else if !mem::replace(&mut self.warned_at_capacity, true) {
                tracing::warn!("Forgejo trigger history full; additional opt-ins will not be remembered during config outages");
            } else {
                // The current readable opt-in still suppresses this callback.
            }
            return true;
        }
        if let (&PolicyRead::Readable(_), Some(primary)) = (policy, file) {
            // A missing higher-priority file could be an outage, not a deletion.
            // Don't let an overlapping CI read erase a newly recorded opt-in either.
            if let Some(opt_in) = known {
                if started <= opt_in.observed || (opt_in.primary && !primary) {
                    return true;
                }
                self.branches.remove(&target);
            }
            return false;
        }
        known.is_some()
    }
}

/// Base repository, PR, and revision named by a signed event.
#[derive(Clone, Eq, Hash, PartialEq)]
struct Revision {
    /// Repository-local PR number.
    number: u64,
    /// Base repository owner.
    owner: String,
    /// Base repository name.
    repo: String,
    /// Candidate head commit.
    sha: String,
}

/// Admission policy pinned to the live base branch at watch start.
#[derive(Clone, PartialEq, Eq)]
struct Policy {
    /// Base ref used to read trusted config.
    base: String,
    /// Resolved policy.
    config: AutomaticReview,
}

impl Policy {
    /// Verify that the revision and its base still qualify.
    fn permits(&self, pr: &PullRequestSummary, revision: &Revision) -> bool {
        is_current(pr, revision)
            && pr.base.ref_name == self.base
            && (!pr.draft || self.config.allow_drafts)
    }
}

/// Outcome of reconciling a watch with the forge.
enum Readiness {
    /// The candidate or policy no longer applies.
    Obsolete,
    /// A job constructed from live metadata.
    Ready(ReviewJob),
    /// Allow another attempt within the watch window.
    Waiting,
}

/// Schedule only after webhook authentication. Host calls happen off the intake path.
pub(crate) async fn schedule(
    state: &AppState,
    deps: ForgejoDeps,
    evt: PullRequestEvent,
    retry: bool,
) {
    let revision = Revision {
        number: evt.number,
        owner: evt.repository.owner.login,
        repo: evt.repository.name,
        sha: evt.pull_request.head.sha,
    };
    if !is_safe_repo_path_segment(&revision.owner)
        || !is_safe_repo_path_segment(&revision.repo)
        || revision.sha.is_empty()
        || !revision.sha.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return;
    }
    {
        let mut active = deps.review_candidates.0.lock().await;
        if let Some(queued) = active.get_mut(&revision) {
            *queued |= retry;
            return;
        }
        if active.len() >= MAX_CANDIDATES {
            tracing::warn!("Forgejo automatic review queue is full");
            return;
        }
        let _prior = active.insert(revision.clone(), false);
    }
    let watch_state = state.clone();
    tokio::spawn(async move {
        loop {
            let dispatched = watch(&watch_state, &deps.host, &revision).await;
            let mut active = deps.review_candidates.0.lock().await;
            if dispatched || !active.get_mut(&revision).is_some_and(mem::take) {
                active.remove(&revision);
                break;
            }
        }
    });
}

/// Check the live revision without relying on delivery metadata.
fn is_current(pr: &PullRequestSummary, revision: &Revision) -> bool {
    pr.state.eq_ignore_ascii_case("open") && pr.head.sha == revision.sha
}

/// `Err` is retryable, never an invitation to use default policy.
async fn load_policy(
    host: &Client,
    triggers: &TriggerHistory,
    revision: &Revision,
) -> Result<Option<Policy>, ()> {
    let pr = live_pr(host, revision).await?;
    if !is_current(&pr, revision) {
        return Ok(None);
    }
    let base = pr.base.ref_name.clone();
    let config = match triggers
        .load(host, &revision.owner, &revision.repo, &base)
        .await
        .policy
    {
        PolicyRead::Readable(config) => config,
        PolicyRead::Unreadable => return Err(()),
        PolicyRead::Invalid => {
            tracing::warn!(repo = %revision.repo, pr = revision.number, "Invalid Forgejo automatic review policy");
            return Ok(None);
        }
    };
    let policy = Policy { base, config };
    Ok(
        (policy.config.forgejo_trigger == ForgejoTrigger::Webhook && policy.permits(&pr, revision))
            .then_some(policy),
    )
}

/// Read PR metadata from the base repository.
async fn live_pr(host: &Client, revision: &Revision) -> Result<PullRequestSummary, ()> {
    host.get_pull_request(&revision.owner, &revision.repo, revision.number)
        .await
        .map_err(|error| {
            tracing::debug!(%error, "Could not read Forgejo PR metadata for admission");
        })
}

/// Every configured exact name must be present, and every matching instance must qualify.
fn requirements_met(policy: &AutomaticReview, signals: Vec<(String, ActionJobProgress)>) -> bool {
    let mut observed = HashSet::new();
    for (name, progress) in signals {
        let Some(requirement) = policy.required_checks.get(&name) else {
            continue;
        };
        if !matches!(
            (*requirement, progress),
            (_, ActionJobProgress::Passed)
                | (CheckRequirement::Complete, ActionJobProgress::Complete)
        ) {
            return false;
        }
        observed.insert(name);
    }
    observed.len() == policy.required_checks.len()
}

/// Poll live metadata and check evidence before proposing a review job.
async fn reconcile(host: &Client, revision: &Revision, policy: &Policy) -> Result<Readiness, ()> {
    let mut pr = live_pr(host, revision).await?;
    if !policy.permits(&pr, revision) {
        return Ok(Readiness::Obsolete);
    }
    if !policy.config.required_checks.is_empty() {
        let Ok(signals) = host
            .action_job_progress(
                &revision.owner,
                &revision.repo,
                revision.number,
                &revision.sha,
            )
            .await
        else {
            return Ok(Readiness::Waiting);
        };
        if !requirements_met(&policy.config, signals) {
            return Ok(Readiness::Waiting);
        }
        // CI reads can take time; don't use the PR snapshot taken before them.
        pr = live_pr(host, revision).await?;
        if !policy.permits(&pr, revision) {
            return Ok(Readiness::Obsolete);
        }
    }
    Ok(Readiness::Ready(ReviewJob {
        owner: revision.owner.clone(),
        repo: revision.repo.clone(),
        pr_number: revision.number,
        head_sha: revision.sha.clone(),
        pr_title: pr.title,
        pr_body: pr.body,
        base_sha: Some(pr.base.sha),
        force: false,
    }))
}

/// Reconcile once more under worker admission and report the outcome to the watch.
async fn admit(
    state: &AppState,
    host: &Arc<Client>,
    revision: &Revision,
    policy: &Policy,
    deadline: Instant,
) -> Readiness {
    let worker_host = Arc::clone(host);
    let worker_triggers = Arc::clone(&state.forgejo_triggers);
    let worker_revision = revision.clone();
    let worker_policy = policy.clone();
    let (notify, result) = oneshot::channel();
    state
        .dispatcher
        .dispatch_when_ready(Box::pin(async move {
            if notify.is_closed() || Instant::now() >= deadline {
                return None;
            }
            let checked = timeout(remaining(deadline).min(API_TIMEOUT), async {
                // A switch back to CI (or any changed policy) invalidates an old watch.
                if load_policy(&worker_host, &worker_triggers, &worker_revision)
                    .await?
                    .as_ref()
                    != Some(&worker_policy)
                {
                    return Ok(Readiness::Obsolete);
                }
                reconcile(&worker_host, &worker_revision, &worker_policy).await
            })
            .await;
            let outcome = match checked {
                Ok(Ok(readiness)) => readiness,
                _ => Readiness::Waiting,
            };
            match outcome {
                Readiness::Ready(job) => notify
                    .send(Readiness::Ready(job.clone()))
                    .ok()
                    .map(|()| job),
                Readiness::Obsolete | Readiness::Waiting => {
                    let _closed = notify.send(outcome);
                    None
                }
            }
        }))
        .await;
    match timeout(remaining(deadline), result).await {
        Ok(Ok(outcome)) => outcome,
        _ => Readiness::Waiting,
    }
}

/// Remaining time, or zero after expiry.
fn remaining(deadline: Instant) -> Duration {
    deadline.saturating_duration_since(Instant::now())
}

/// Increase delay without exceeding the polling cap.
fn backoff(delay: Duration) -> Duration {
    delay.max(SETTLE).saturating_mul(2).min(MAX_RETRY_DELAY)
}

/// Retry transient reads before establishing the trusted admission policy.
async fn wait_for_policy(
    host: &Client,
    triggers: &TriggerHistory,
    revision: &Revision,
    deadline: Instant,
) -> Option<Policy> {
    let mut delay = SETTLE;
    while !remaining(deadline).is_zero() {
        if let Ok(Ok(policy)) = timeout(
            remaining(deadline).min(API_TIMEOUT),
            load_policy(host, triggers, revision),
        )
        .await
        {
            return policy;
        }
        sleep(delay.min(remaining(deadline))).await;
        delay = backoff(delay);
    }
    None
}

/// Track one revision to admission, cancellation, or expiry.
async fn watch(state: &AppState, host: &Arc<Client>, revision: &Revision) -> bool {
    let Some(deadline) = Instant::now().checked_add(WATCH_WINDOW) else {
        return false;
    };
    let Some(policy) = wait_for_policy(host, &state.forgejo_triggers, revision, deadline).await
    else {
        return false;
    };
    let mut delay = Duration::ZERO;
    let pending_id = if policy.config.required_checks.is_empty() {
        None
    } else {
        delay = SETTLE;
        update_wait_status(host, revision, None).await
    };
    let reason = loop {
        sleep(delay.min(remaining(deadline))).await;
        if remaining(deadline).is_zero() {
            break ReviewWaitEnd::TimedOut;
        }
        let checked = timeout(
            remaining(deadline).min(API_TIMEOUT),
            reconcile(host, revision, &policy),
        )
        .await;
        let outcome = match checked {
            Ok(Ok(Readiness::Ready(_))) => admit(state, host, revision, &policy, deadline).await,
            Ok(Ok(readiness)) => readiness,
            _ => Readiness::Waiting,
        };
        match outcome {
            Readiness::Ready(_) => {
                state.metrics.record_job_dispatched();
                return true;
            }
            Readiness::Obsolete => break ReviewWaitEnd::Canceled,
            Readiness::Waiting => delay = backoff(delay),
        }
    };
    if let Some(id) = pending_id {
        update_wait_status(host, revision, Some((id, reason))).await;
    }
    false
}

/// Publish an advisory status without blocking the watch on a failed write.
async fn update_wait_status(
    host: &Client,
    revision: &Revision,
    end: Option<(u64, ReviewWaitEnd)>,
) -> Option<u64> {
    match host
        .update_review_wait_status(&revision.owner, &revision.repo, &revision.sha, end)
        .await
    {
        Ok(id) => id,
        Err(error) => {
            tracing::warn!(repo = %revision.repo, pr = revision.number, %error, "Could not publish Forgejo wait status");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_trigger_history_does_not_veto_unrelated_legacy_callbacks() {
        let mut history = TriggerOptIns::default();
        let target = |index: usize| Target {
            base: "main".to_owned(),
            owner: "owner".to_owned(),
            repo: format!("repo-{index}"),
        };
        for index in 0..=MAX_TRIGGER_OPT_INS {
            assert!(history.observe(
                target(index),
                Instant::now(),
                &PolicyRead::Invalid,
                Some(true),
                true
            ));
        }
        let legacy = MAX_TRIGGER_OPT_INS + 1;
        let absent = PolicyRead::Readable(AutomaticReview::default());
        assert!(
            !history.observe(target(legacy), Instant::now(), &absent, None, false),
            "full history must not suppress unrelated legacy callbacks"
        );
        assert!(!history.observe(
            target(legacy),
            Instant::now(),
            &PolicyRead::Unreadable,
            None,
            false
        ));
        assert!(history.observe(
            target(0),
            Instant::now(),
            &PolicyRead::Unreadable,
            None,
            false
        ));
    }
}

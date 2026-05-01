//! Lightweight in-process Prometheus-style counters.
//!
//! Operators wire `/metrics` into their Prometheus scrape config and
//! get visibility into webhook traffic, signature failures, and
//! dispatch volume without pulling in a heavy metrics library.
//!
//! All counters are `AtomicU64` so the cost of an increment is
//! one relaxed RMW; reads on the scrape path go through the same
//! ordering. We don't need stronger guarantees because the values
//! are advisory, not load-bearing.

use ar_orchestrator::{ReviewObservation, ReviewObserver};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

#[derive(Default)]
pub struct Metrics {
    pub webhooks_pull_request: AtomicU64,
    pub webhooks_issue_comment: AtomicU64,
    pub webhooks_ping: AtomicU64,
    pub webhooks_other: AtomicU64,
    pub webhook_signature_failures: AtomicU64,
    pub webhook_payload_failures: AtomicU64,
    pub jobs_dispatched: AtomicU64,
    pub chat_commands_received: AtomicU64,
    pub chat_handler_unconfigured: AtomicU64,

    // Review-outcome counters (fed by the orchestrator's
    // ReviewObserver trait via `MetricsObserver`).
    pub reviews_started: AtomicU64,
    pub reviews_succeeded: AtomicU64,
    pub reviews_failed_forgejo: AtomicU64,
    pub reviews_failed_workspace: AtomicU64,
    pub reviews_failed_llm: AtomicU64,
    pub reviews_failed_unhealable: AtomicU64,
    pub reviews_skipped_same_sha: AtomicU64,
    pub reviews_skipped_trivial: AtomicU64,
    pub reviews_skipped_disabled: AtomicU64,
    /// Sum of all completed review durations (succeeded + failed) in
    /// milliseconds. Pair with `reviews_completed_count` to compute
    /// the running average. Not a proper histogram — operators who
    /// want p99 should scrape and downsample externally.
    pub review_duration_ms_sum: AtomicU64,
    pub reviews_completed_count: AtomicU64,
    /// Sum of `findings_count` across successful reviews. Lets
    /// operators chart total bot-flagged issues over time.
    pub review_findings_sum: AtomicU64,
}

impl Metrics {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn record_event(&self, event: &str) {
        let counter = match event {
            "pull_request" => &self.webhooks_pull_request,
            "issue_comment" => &self.webhooks_issue_comment,
            "ping" => &self.webhooks_ping,
            _ => &self.webhooks_other,
        };
        counter.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_signature_failure(&self) {
        self.webhook_signature_failures
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_payload_failure(&self) {
        self.webhook_payload_failures
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_job_dispatched(&self) {
        self.jobs_dispatched.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_chat_command(&self) {
        self.chat_commands_received.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_chat_unconfigured(&self) {
        self.chat_handler_unconfigured
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_review_started(&self) {
        self.reviews_started.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_review_succeeded(&self, duration_ms: u64, findings_count: u64) {
        self.reviews_succeeded.fetch_add(1, Ordering::Relaxed);
        self.reviews_completed_count.fetch_add(1, Ordering::Relaxed);
        self.review_duration_ms_sum
            .fetch_add(duration_ms, Ordering::Relaxed);
        self.review_findings_sum
            .fetch_add(findings_count, Ordering::Relaxed);
    }

    pub fn record_review_failed(&self, duration_ms: u64, error_class: &str) {
        let counter = match error_class {
            "forgejo" => &self.reviews_failed_forgejo,
            "workspace" => &self.reviews_failed_workspace,
            "llm" => &self.reviews_failed_llm,
            _ => &self.reviews_failed_unhealable,
        };
        counter.fetch_add(1, Ordering::Relaxed);
        self.reviews_completed_count.fetch_add(1, Ordering::Relaxed);
        self.review_duration_ms_sum
            .fetch_add(duration_ms, Ordering::Relaxed);
    }

    pub fn record_review_skipped(&self, reason: &str) {
        let counter = match reason {
            "same_sha" => &self.reviews_skipped_same_sha,
            "trivial_files" => &self.reviews_skipped_trivial,
            _ => &self.reviews_skipped_disabled,
        };
        counter.fetch_add(1, Ordering::Relaxed);
    }

    /// Render counters in the Prometheus text exposition format.
    /// One `# HELP` and `# TYPE` line per metric, then a single
    /// sample line. We don't emit `_created` timestamps; scrapers
    /// don't need them for counters.
    pub fn render(&self) -> String {
        let mut out = String::with_capacity(1024);
        let metrics: &[(&str, &str, &AtomicU64)] = &[
            (
                "auto_review_webhooks_pull_request_total",
                "Pull-request webhook events received with a valid signature.",
                &self.webhooks_pull_request,
            ),
            (
                "auto_review_webhooks_issue_comment_total",
                "Issue-comment webhook events received with a valid signature.",
                &self.webhooks_issue_comment,
            ),
            (
                "auto_review_webhooks_ping_total",
                "Ping webhook events received with a valid signature.",
                &self.webhooks_ping,
            ),
            (
                "auto_review_webhooks_other_total",
                "Other webhook events received with a valid signature.",
                &self.webhooks_other,
            ),
            (
                "auto_review_webhook_signature_failures_total",
                "Webhook requests rejected for invalid HMAC signature. Sustained increases imply secret rotation drift or an active probing attack.",
                &self.webhook_signature_failures,
            ),
            (
                "auto_review_webhook_payload_failures_total",
                "Webhook requests rejected for malformed JSON payload. Increases typically imply a Forgejo version mismatch.",
                &self.webhook_payload_failures,
            ),
            (
                "auto_review_jobs_dispatched_total",
                "Review jobs handed to the orchestrator dispatcher.",
                &self.jobs_dispatched,
            ),
            (
                "auto_review_chat_commands_received_total",
                "Chat commands parsed from issue-comment events.",
                &self.chat_commands_received,
            ),
            (
                "auto_review_chat_handler_unconfigured_total",
                "Chat commands dropped because ChatDeps was not wired into the running gateway.",
                &self.chat_handler_unconfigured,
            ),
            (
                "auto_review_reviews_started_total",
                "Review jobs that began executing (post-dedup, post-trivial-skip).",
                &self.reviews_started,
            ),
            (
                "auto_review_reviews_succeeded_total",
                "Review jobs that posted a review without erroring.",
                &self.reviews_succeeded,
            ),
            (
                "auto_review_reviews_failed_forgejo_total",
                "Review jobs that failed talking to Forgejo (token, transport, 5xx).",
                &self.reviews_failed_forgejo,
            ),
            (
                "auto_review_reviews_failed_workspace_total",
                "Review jobs that failed cloning the PR's head SHA into the workspace.",
                &self.reviews_failed_workspace,
            ),
            (
                "auto_review_reviews_failed_llm_total",
                "Review jobs that failed talking to the LLM provider.",
                &self.reviews_failed_llm,
            ),
            (
                "auto_review_reviews_failed_unhealable_total",
                "Review jobs whose LLM output never satisfied the schema validator after the self-heal retry budget.",
                &self.reviews_failed_unhealable,
            ),
            (
                "auto_review_reviews_skipped_same_sha_total",
                "Reviews skipped because this head SHA was already reviewed (incremental dedup).",
                &self.reviews_skipped_same_sha,
            ),
            (
                "auto_review_reviews_skipped_trivial_total",
                "Reviews skipped because the changed-file set is entirely lockfiles, vendored, or generated.",
                &self.reviews_skipped_trivial,
            ),
            (
                "auto_review_reviews_skipped_disabled_total",
                "Reviews skipped because .auto_review.yaml has enabled: false.",
                &self.reviews_skipped_disabled,
            ),
            (
                "auto_review_review_duration_ms_sum",
                "Sum of completed review durations (succeeded + failed) in milliseconds. Pair with reviews_completed_count to compute mean.",
                &self.review_duration_ms_sum,
            ),
            (
                "auto_review_reviews_completed_count",
                "Total reviews that ran to completion (succeeded or failed). Excludes skipped.",
                &self.reviews_completed_count,
            ),
            (
                "auto_review_review_findings_sum",
                "Sum of findings_count across successful reviews. Useful for charting bot-flagged issue volume.",
                &self.review_findings_sum,
            ),
        ];
        for (name, help, counter) in metrics {
            out.push_str("# HELP ");
            out.push_str(name);
            out.push(' ');
            out.push_str(help);
            out.push('\n');
            out.push_str("# TYPE ");
            out.push_str(name);
            out.push_str(" counter\n");
            out.push_str(name);
            out.push(' ');
            out.push_str(&counter.load(Ordering::Relaxed).to_string());
            out.push('\n');
        }
        out
    }
}

/// Bridges the orchestrator's [`ReviewObserver`] trait to a
/// gateway-owned [`Metrics`]. Wire one of these into
/// `SpawningDispatcher::with_observer` and review outcomes flow
/// straight to `/metrics`.
pub struct MetricsObserver {
    metrics: Arc<Metrics>,
}

impl MetricsObserver {
    pub fn new(metrics: Arc<Metrics>) -> Self {
        Self { metrics }
    }
}

impl ReviewObserver for MetricsObserver {
    fn record(&self, observation: ReviewObservation) {
        match observation {
            ReviewObservation::Started => self.metrics.record_review_started(),
            ReviewObservation::Succeeded {
                duration,
                findings_count,
            } => self.metrics.record_review_succeeded(
                duration.as_millis() as u64,
                findings_count as u64,
            ),
            ReviewObservation::Failed {
                duration,
                error_class,
            } => self
                .metrics
                .record_review_failed(duration.as_millis() as u64, error_class),
            ReviewObservation::Skipped { reason } => self.metrics.record_review_skipped(reason),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_emits_zero_counters_with_help_and_type_lines() {
        let m = Metrics::new();
        let out = m.render();
        assert!(out.contains("# HELP auto_review_webhooks_pull_request_total"));
        assert!(out.contains("# TYPE auto_review_webhooks_pull_request_total counter"));
        assert!(out.contains("auto_review_webhooks_pull_request_total 0\n"));
        assert!(out.contains("auto_review_jobs_dispatched_total 0\n"));
    }

    #[test]
    fn record_event_buckets_known_events() {
        let m = Metrics::new();
        m.record_event("pull_request");
        m.record_event("pull_request");
        m.record_event("issue_comment");
        m.record_event("ping");
        m.record_event("create");
        m.record_event("delete");
        let out = m.render();
        assert!(out.contains("auto_review_webhooks_pull_request_total 2\n"));
        assert!(out.contains("auto_review_webhooks_issue_comment_total 1\n"));
        assert!(out.contains("auto_review_webhooks_ping_total 1\n"));
        assert!(out.contains("auto_review_webhooks_other_total 2\n"));
    }

    #[test]
    fn review_outcome_counters_route_by_class() {
        let m = Arc::new(Metrics::new());
        let obs = MetricsObserver::new(m.clone());

        obs.record(ReviewObservation::Started);
        obs.record(ReviewObservation::Started);

        obs.record(ReviewObservation::Succeeded {
            duration: std::time::Duration::from_millis(1500),
            findings_count: 3,
        });
        obs.record(ReviewObservation::Succeeded {
            duration: std::time::Duration::from_millis(500),
            findings_count: 1,
        });

        obs.record(ReviewObservation::Failed {
            duration: std::time::Duration::from_millis(800),
            error_class: "llm",
        });
        obs.record(ReviewObservation::Failed {
            duration: std::time::Duration::from_millis(200),
            error_class: "workspace",
        });

        obs.record(ReviewObservation::Skipped { reason: "same_sha" });
        obs.record(ReviewObservation::Skipped {
            reason: "trivial_files",
        });
        obs.record(ReviewObservation::Skipped {
            reason: "disabled_by_config",
        });

        let out = m.render();
        assert!(out.contains("auto_review_reviews_started_total 2\n"));
        assert!(out.contains("auto_review_reviews_succeeded_total 2\n"));
        assert!(out.contains("auto_review_reviews_failed_llm_total 1\n"));
        assert!(out.contains("auto_review_reviews_failed_workspace_total 1\n"));
        assert!(out.contains("auto_review_reviews_failed_forgejo_total 0\n"));
        assert!(out.contains("auto_review_reviews_skipped_same_sha_total 1\n"));
        assert!(out.contains("auto_review_reviews_skipped_trivial_total 1\n"));
        assert!(out.contains("auto_review_reviews_skipped_disabled_total 1\n"));
        // 1500 + 500 + 800 + 200 = 3000 ms across four completed
        // reviews.
        assert!(out.contains("auto_review_review_duration_ms_sum 3000\n"));
        assert!(out.contains("auto_review_reviews_completed_count 4\n"));
        // 3 + 1 = 4 findings across the two successes.
        assert!(out.contains("auto_review_review_findings_sum 4\n"));
    }

    #[test]
    fn failure_and_dispatch_counters_increment() {
        let m = Metrics::new();
        m.record_signature_failure();
        m.record_signature_failure();
        m.record_payload_failure();
        m.record_job_dispatched();
        m.record_job_dispatched();
        m.record_job_dispatched();
        m.record_chat_command();
        m.record_chat_unconfigured();
        let out = m.render();
        assert!(out.contains("auto_review_webhook_signature_failures_total 2\n"));
        assert!(out.contains("auto_review_webhook_payload_failures_total 1\n"));
        assert!(out.contains("auto_review_jobs_dispatched_total 3\n"));
        assert!(out.contains("auto_review_chat_commands_received_total 1\n"));
        assert!(out.contains("auto_review_chat_handler_unconfigured_total 1\n"));
    }
}

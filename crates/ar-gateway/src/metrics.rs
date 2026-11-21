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

use alloc::sync::Arc;
use ar_orchestrator::{ReviewObservation, ReviewObserver};
use core::fmt::Write as _;
use core::sync::atomic::{AtomicU64, Ordering};
use core::time::Duration;

/// Bucket bounds (in seconds) for the review-duration histogram.
/// Tuned for review work: most fast on cached PRs, long-tail
/// reaching minutes on big diffs with cloud LLMs. Operators wanting
/// a different distribution can derive their own from the raw
/// `review_duration_ms_sum + reviews_completed_count` pair.
const DURATION_BUCKETS_SECS: [f64; 8] = [1.0, 5.0, 15.0, 30.0, 60.0, 120.0, 300.0, 600.0];

/// `reason` label values of `auto_review_local_diff_fallbacks_total`, in
/// render order. The orchestrator's `ReviewObservation::LocalDiffFallback`
/// reasons, plus `unknown` for a reason this sink was not updated for.
const LOCAL_DIFF_FALLBACK_REASONS: [&str; 7] = [
    "clone_failed",
    "no_merge_base",
    "history_cap",
    "timed_out",
    "fetch_failed",
    "git_error",
    "unknown",
];

#[expect(
    clippy::arbitrary_source_item_ordering,
    reason = "Group counters by webhook intake, review outcomes, polling, and histogram"
)]
#[expect(
    clippy::partial_pub_fields,
    reason = "Preserve public scalar counters while keeping bucket arrays internal"
)]
#[derive(Default)]
pub struct Metrics {
    pub webhooks_pull_request: AtomicU64,
    pub webhooks_issue_comment: AtomicU64,
    pub webhooks_ping: AtomicU64,
    pub webhooks_other: AtomicU64,
    pub webhook_signature_failures: AtomicU64,
    pub webhook_payload_failures: AtomicU64,
    pub webhook_rate_limited: AtomicU64,
    pub webhook_duplicates: AtomicU64,
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
    /// Reviews that ended via a tokio task panic / cancellation in
    /// the dispatcher's outer wrapper. Distinct from the four
    /// `ReviewError` variants because a panic isn't a structured
    /// failure — it's a bug. Sustained increases here mean
    /// "investigate logs, fix the panic site".
    pub reviews_failed_panic: AtomicU64,
    /// Failure classes we don't recognise. Routes here instead of
    /// silently misbucketing into `reviews_failed_unhealable`. Drift
    /// alarm: a non-zero value means the orchestrator's
    /// `error_class()` returned a string this metrics sink wasn't
    /// updated to handle.
    pub reviews_failed_unknown: AtomicU64,
    pub reviews_skipped_same_sha: AtomicU64,
    pub reviews_skipped_trivial: AtomicU64,
    pub reviews_skipped_disabled: AtomicU64,
    /// Skip reasons we don't recognise. Routes here instead of
    /// silently misbucketing into `reviews_skipped_disabled`. Drift
    /// alarm: a non-zero value here means the dispatcher emitted a
    /// new `Skipped { reason }` string this metric sink wasn't
    /// updated to handle.
    pub reviews_skipped_unknown: AtomicU64,
    /// Sum of all completed review durations (succeeded + failed) in
    /// milliseconds. Pair with `reviews_completed_count` to compute
    /// the running average. Not a proper histogram — operators who
    /// want p99 should scrape and downsample externally.
    pub review_duration_ms_sum: AtomicU64,
    pub reviews_completed_count: AtomicU64,
    /// Sum of `findings_count` across successful reviews. Lets
    /// operators chart total bot-flagged issues over time.
    pub review_findings_sum: AtomicU64,
    pub verifier_findings_dropped: AtomicU64,
    pub review_queue_waits: AtomicU64,
    /// Full reviews that used the host's diff because the local merge-base
    /// diff was unavailable, one counter per `LOCAL_DIFF_FALLBACK_REASONS`
    /// entry. Rendered as one labelled family.
    local_diff_fallbacks: [AtomicU64; LOCAL_DIFF_FALLBACK_REASONS.len()],

    // Poller counters: track the background ChatPoller's progress
    // so operators can see whether inline-thread mentions are being
    // picked up. Disjoint from `chat_commands_received_total` (which
    // tracks webhook-path mentions).
    pub poll_cycles: AtomicU64,
    pub poll_history_failures: AtomicU64,
    pub poll_pr_failures: AtomicU64,
    pub poll_mentions_dispatched: AtomicU64,
    pub poll_chat_failures: AtomicU64,

    /// Cumulative-bucket counters for the review-duration histogram.
    /// Each entry counts reviews whose duration was <= the
    /// corresponding `DURATION_BUCKETS_SECS` bound. Cumulative
    /// (Prometheus convention), so the last bucket equals
    /// `reviews_completed_count` and a `+Inf` line is emitted on
    /// scrape. Indexed by position in `DURATION_BUCKETS_SECS`.
    duration_buckets: [AtomicU64; DURATION_BUCKETS_SECS.len()],
}

#[expect(
    clippy::arbitrary_source_item_ordering,
    reason = "Keep recording methods grouped by event source and exposition helpers by scrape order"
)]
impl Metrics {
    #[inline]
    #[must_use = "Keep the returned metrics instance to record or render counters"]
    pub fn new() -> Self {
        Self::default()
    }

    #[inline]
    pub fn record_event(&self, event: &str) {
        let counter = match event {
            "pull_request" => &self.webhooks_pull_request,
            "issue_comment" => &self.webhooks_issue_comment,
            "ping" => &self.webhooks_ping,
            _ => &self.webhooks_other,
        };
        counter.fetch_add(1, Ordering::Relaxed);
    }

    #[inline]
    pub fn record_signature_failure(&self) {
        self.webhook_signature_failures
            .fetch_add(1, Ordering::Relaxed);
    }

    #[inline]
    pub fn record_payload_failure(&self) {
        self.webhook_payload_failures
            .fetch_add(1, Ordering::Relaxed);
    }

    #[inline]
    pub fn record_rate_limited(&self) {
        self.webhook_rate_limited.fetch_add(1, Ordering::Relaxed);
    }

    #[inline]
    pub fn record_duplicate(&self) {
        self.webhook_duplicates.fetch_add(1, Ordering::Relaxed);
    }

    #[inline]
    pub fn record_job_dispatched(&self) {
        self.jobs_dispatched.fetch_add(1, Ordering::Relaxed);
    }

    #[inline]
    pub fn record_chat_command(&self) {
        self.chat_commands_received.fetch_add(1, Ordering::Relaxed);
    }

    #[inline]
    pub fn record_chat_unconfigured(&self) {
        self.chat_handler_unconfigured
            .fetch_add(1, Ordering::Relaxed);
    }

    #[inline]
    pub fn record_review_started(&self) {
        self.reviews_started.fetch_add(1, Ordering::Relaxed);
    }

    #[inline]
    pub fn record_review_succeeded(
        &self,
        duration_ms: u64,
        findings_count: u64,
        verifier_dropped: u64,
    ) {
        self.reviews_succeeded.fetch_add(1, Ordering::Relaxed);
        self.reviews_completed_count.fetch_add(1, Ordering::Relaxed);
        self.review_duration_ms_sum
            .fetch_add(duration_ms, Ordering::Relaxed);
        self.review_findings_sum
            .fetch_add(findings_count, Ordering::Relaxed);
        self.verifier_findings_dropped
            .fetch_add(verifier_dropped, Ordering::Relaxed);
        self.bucket_duration(duration_ms);
    }

    #[inline]
    pub fn record_review_failed(&self, duration_ms: u64, error_class: &str) {
        let counter = match error_class {
            "forgejo" => &self.reviews_failed_forgejo,
            "workspace" => &self.reviews_failed_workspace,
            "llm" => &self.reviews_failed_llm,
            "unhealable" => &self.reviews_failed_unhealable,
            "panic" => &self.reviews_failed_panic,
            other => {
                tracing::warn!(
                    error_class = other,
                    "unknown review failure class; routing to reviews_failed_unknown \u{2014} \
                     update the metrics map to track this class explicitly"
                );
                &self.reviews_failed_unknown
            }
        };
        counter.fetch_add(1, Ordering::Relaxed);
        self.reviews_completed_count.fetch_add(1, Ordering::Relaxed);
        self.review_duration_ms_sum
            .fetch_add(duration_ms, Ordering::Relaxed);
        self.bucket_duration(duration_ms);
    }

    /// Increment every histogram bucket whose upper bound is >= the
    /// observed duration. Cumulative-bucket convention per the
    /// Prometheus exposition format.
    fn bucket_duration(&self, duration_ms: u64) {
        let secs = millis_to_seconds(duration_ms);
        for (&bound, counter) in DURATION_BUCKETS_SECS.iter().zip(&self.duration_buckets) {
            if secs <= bound {
                counter.fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    #[inline]
    pub fn record_poll_cycle(&self) {
        self.poll_cycles.fetch_add(1, Ordering::Relaxed);
    }

    #[inline]
    pub fn record_poll_history_failure(&self) {
        self.poll_history_failures.fetch_add(1, Ordering::Relaxed);
    }

    #[inline]
    pub fn record_poll_pr_failure(&self) {
        self.poll_pr_failures.fetch_add(1, Ordering::Relaxed);
    }

    #[inline]
    pub fn record_poll_mention_dispatched(&self) {
        self.poll_mentions_dispatched
            .fetch_add(1, Ordering::Relaxed);
    }

    #[inline]
    pub fn record_poll_chat_failure(&self) {
        self.poll_chat_failures.fetch_add(1, Ordering::Relaxed);
    }

    #[inline]
    pub fn record_review_queue_wait(&self) {
        self.review_queue_waits.fetch_add(1, Ordering::Relaxed);
    }

    #[inline]
    pub fn record_local_diff_fallback(&self, reason: &str) {
        let [.., unknown_counter] = self.local_diff_fallbacks.each_ref();
        let counter = LOCAL_DIFF_FALLBACK_REASONS
            .iter()
            .zip(&self.local_diff_fallbacks)
            .find_map(|(&known, counter)| (known == reason && known != "unknown").then_some(counter))
            .unwrap_or_else(|| {
                tracing::warn!(
                    reason,
                    "unknown local diff fallback reason; counting it as reason=\"unknown\" \u{2014} \
                     update the metrics map to track this reason explicitly"
                );
                unknown_counter
            });
        counter.fetch_add(1, Ordering::Relaxed);
    }

    #[inline]
    pub fn record_review_skipped(&self, reason: &str) {
        let counter = match reason {
            "same_sha" => &self.reviews_skipped_same_sha,
            "trivial_files" => &self.reviews_skipped_trivial,
            "disabled_by_config" => &self.reviews_skipped_disabled,
            other => {
                tracing::warn!(
                    reason = other,
                    "unknown skip reason; routing to reviews_skipped_unknown \u{2014} \
                     update the metrics map to track this reason explicitly"
                );
                &self.reviews_skipped_unknown
            }
        };
        counter.fetch_add(1, Ordering::Relaxed);
    }

    /// Render counters in the Prometheus text exposition format.
    /// One `# HELP` and `# TYPE` line per metric, then a single
    /// sample line. We don't emit `_created` timestamps; scrapers
    /// don't need them for counters.
    #[expect(
        clippy::too_many_lines,
        reason = "Keep the ordered scalar metric registry together"
    )]
    #[inline]
    #[must_use = "The scrape body is returned, not written to an external sink"]
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
                "auto_review_webhook_rate_limited_total",
                "Webhook requests rejected because the global token-bucket rate limit was empty. Sustained increases mean a probing source or a misconfigured webhook firing in a tight loop.",
                &self.webhook_rate_limited,
            ),
            (
                "auto_review_webhook_duplicates_total",
                "Webhook deliveries replied 200 OK without dispatching because the X-Forgejo-Delivery UUID matched a recently-seen one. Counts Forgejo's at-least-once-delivery retries.",
                &self.webhook_duplicates,
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
                "auto_review_reviews_failed_panic_total",
                "Review jobs whose tokio task panicked or was cancelled. Bugs to investigate, distinct from the structured ReviewError variants.",
                &self.reviews_failed_panic,
            ),
            (
                "auto_review_reviews_failed_unknown_total",
                "Review jobs that failed for an error class this metrics sink doesn't recognise. Non-zero indicates orchestrator/metrics drift; update the failure-class map.",
                &self.reviews_failed_unknown,
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
                "auto_review_reviews_skipped_unknown_total",
                "Reviews skipped for a reason this metrics sink doesn't recognise. Non-zero indicates dispatcher/metrics drift; update the skip-reason map.",
                &self.reviews_skipped_unknown,
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
            (
                "auto_review_verifier_findings_dropped_total",
                "Findings the cheap-tier verifier corrected away. Sustained increases relative to review_findings_sum mean the reasoning model is hallucinating; consider switching to a higher-quality model.",
                &self.verifier_findings_dropped,
            ),
            (
                "auto_review_review_queue_waits_total",
                "Dispatches that had to wait on the AR_REVIEW_CONCURRENCY semaphore before starting. Sustained ratio relative to reviews_started_total > 10% means the cap is too tight or traffic exceeds capacity.",
                &self.review_queue_waits,
            ),
            (
                "auto_review_poll_cycles_total",
                "Background-poller passes that completed successfully. Compare against the configured AR_POLL_INTERVAL_SECS to spot a stalled poller.",
                &self.poll_cycles,
            ),
            (
                "auto_review_poll_history_failures_total",
                "Poll passes that failed at the review-history list step. Sustained increases mean the SQLite/in-memory store is broken.",
                &self.poll_history_failures,
            ),
            (
                "auto_review_poll_pr_failures_total",
                "Per-PR poll failures (e.g. 5xx from Forgejo on list-review-comments). One PR's failure doesn't abort the whole pass.",
                &self.poll_pr_failures,
            ),
            (
                "auto_review_poll_mentions_dispatched_total",
                "Inline-thread mentions the poller picked up and dispatched to the chat handler. Disjoint from chat_commands_received_total (webhook path).",
                &self.poll_mentions_dispatched,
            ),
            (
                "auto_review_poll_chat_failures_total",
                "Chat-handler errors when the poller dispatched a polled mention.",
                &self.poll_chat_failures,
            ),
        ];
        for &(name, help, counter) in metrics {
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
        self.render_local_diff_fallbacks(&mut out);
        self.render_duration_histogram(&mut out);
        out
    }

    /// Emit `auto_review_local_diff_fallbacks_total`, one sample per
    /// bounded `reason` label, every reason present from zero.
    fn render_local_diff_fallbacks(&self, out: &mut String) {
        out.push_str(
            "# HELP auto_review_local_diff_fallbacks_total Full reviews that used the host's diff \
             because the local merge-base diff was unavailable, by reason. A fleet-wide rise \
             points at a git older than 2.42, a failing clone or a stalled forge; GitHub refuses the host diff for \
             pull requests over 300 files.\n",
        );
        out.push_str("# TYPE auto_review_local_diff_fallbacks_total counter\n");
        for (reason, counter) in LOCAL_DIFF_FALLBACK_REASONS
            .iter()
            .zip(&self.local_diff_fallbacks)
        {
            // Writing these primitive values to a String cannot fail.
            let _write_result = writeln!(
                out,
                "auto_review_local_diff_fallbacks_total{{reason=\"{reason}\"}} {}",
                counter.load(Ordering::Relaxed)
            );
        }
    }

    /// Emit the Prometheus histogram lines for review duration:
    /// one `_bucket{le="X"}` line per bound plus a `+Inf` line that
    /// equals `reviews_completed_count`.
    fn render_duration_histogram(&self, out: &mut String) {
        out.push_str(
            "# HELP auto_review_review_duration_seconds Review-pipeline wall-clock latency from \
             pipeline start through commit-status post.\n",
        );
        out.push_str("# TYPE auto_review_review_duration_seconds histogram\n");
        // Writing these primitive values to a String cannot fail.
        for (&bound, counter) in DURATION_BUCKETS_SECS.iter().zip(&self.duration_buckets) {
            let count = counter.load(Ordering::Relaxed);
            let _write_result = writeln!(
                out,
                "auto_review_review_duration_seconds_bucket{{le=\"{}\"}} {count}",
                fmt_bucket_bound(bound)
            );
        }
        let total = self.reviews_completed_count.load(Ordering::Relaxed);
        let _write_result = writeln!(
            out,
            "auto_review_review_duration_seconds_bucket{{le=\"+Inf\"}} {total}"
        );
        let sum_ms = self.review_duration_ms_sum.load(Ordering::Relaxed);
        let sum_secs = millis_to_seconds(sum_ms);
        let _sum_result = writeln!(out, "auto_review_review_duration_seconds_sum {sum_secs}");
        let _count_result = writeln!(out, "auto_review_review_duration_seconds_count {total}");
    }
}

/// Bridges the orchestrator's [`ReviewObserver`] trait to a
/// gateway-owned [`Metrics`]. Wire one of these into
/// `SpawningDispatcher::with_observer` and review outcomes flow
/// straight to `/metrics`.
#[expect(
    clippy::module_name_repetitions,
    reason = "Preserve the public MetricsObserver name"
)]
pub struct MetricsObserver {
    /// Shared counters updated by observed review events.
    metrics: Arc<Metrics>,
}

impl MetricsObserver {
    #[inline]
    pub fn new(metrics: Arc<Metrics>) -> Self {
        Self { metrics }
    }
}

impl ReviewObserver for MetricsObserver {
    #[expect(
        clippy::as_conversions,
        reason = "Finding counts are usize, which fits in u64 on supported targets"
    )]
    #[inline]
    fn record(&self, observation: ReviewObservation) {
        match observation {
            ReviewObservation::Started => self.metrics.record_review_started(),
            ReviewObservation::Succeeded {
                duration,
                findings_count,
                verifier_dropped,
            } => self.metrics.record_review_succeeded(
                duration_millis(duration),
                findings_count as u64,
                verifier_dropped as u64,
            ),
            ReviewObservation::Failed {
                duration,
                error_class,
            } => self
                .metrics
                .record_review_failed(duration_millis(duration), error_class),
            ReviewObservation::Skipped { reason } => self.metrics.record_review_skipped(reason),
            ReviewObservation::QueueWait => self.metrics.record_review_queue_wait(),
            ReviewObservation::LocalDiffFallback { reason } => {
                self.metrics.record_local_diff_fallback(reason);
            }
        }
    }
}

/// Convert observer durations to the millisecond counter representation.
#[expect(
    clippy::as_conversions,
    clippy::cast_possible_truncation,
    reason = "Preserve the low 64 bits for durations beyond the counter range"
)]
fn duration_millis(duration: Duration) -> u64 {
    duration.as_millis() as u64
}

/// Render a bucket bound as `60` for integers or `0.5` for fractions.
#[expect(
    clippy::as_conversions,
    clippy::cast_possible_truncation,
    clippy::float_cmp,
    reason = "Configured bounds fit in i64; exact integrality selects the integer rendering"
)]
fn fmt_bucket_bound(bound: f64) -> String {
    if bound == bound.trunc() {
        format!("{}", bound as i64)
    } else {
        format!("{bound}")
    }
}

/// Convert the millisecond counter to Prometheus histogram seconds.
#[expect(
    clippy::as_conversions,
    clippy::cast_precision_loss,
    clippy::float_arithmetic,
    reason = "Preserve the histogram's existing cast-then-divide rounding"
)]
fn millis_to_seconds(millis: u64) -> f64 {
    let millis_per_second: f64 = 1_000.0;
    millis as f64 / millis_per_second
}

#[cfg(test)]
#[expect(
    clippy::min_ident_chars,
    reason = "m is the shared convention for the metrics fixture"
)]
mod tests {
    use super::*;
    use alloc::collections::BTreeSet;

    #[test]
    fn render_exposes_every_known_counter() {
        // Pin the full set of counter names. If you add a counter
        // field to the struct, this test reminds you to also wire
        // it into render() — otherwise it'd be invisible to ops.
        // The list is alphabetised for easy diff review when
        // counters are added.
        const EXPECTED: &[&str] = &[
            "auto_review_chat_commands_received_total",
            "auto_review_chat_handler_unconfigured_total",
            "auto_review_jobs_dispatched_total",
            "auto_review_poll_chat_failures_total",
            "auto_review_poll_cycles_total",
            "auto_review_poll_history_failures_total",
            "auto_review_poll_mentions_dispatched_total",
            "auto_review_poll_pr_failures_total",
            "auto_review_review_duration_ms_sum",
            "auto_review_review_findings_sum",
            "auto_review_review_queue_waits_total",
            "auto_review_reviews_completed_count",
            "auto_review_reviews_failed_forgejo_total",
            "auto_review_reviews_failed_llm_total",
            "auto_review_reviews_failed_panic_total",
            "auto_review_reviews_failed_unhealable_total",
            "auto_review_reviews_failed_unknown_total",
            "auto_review_reviews_failed_workspace_total",
            "auto_review_reviews_skipped_disabled_total",
            "auto_review_reviews_skipped_same_sha_total",
            "auto_review_reviews_skipped_trivial_total",
            "auto_review_reviews_skipped_unknown_total",
            "auto_review_reviews_started_total",
            "auto_review_reviews_succeeded_total",
            "auto_review_verifier_findings_dropped_total",
            "auto_review_webhook_duplicates_total",
            "auto_review_webhook_payload_failures_total",
            "auto_review_webhook_rate_limited_total",
            "auto_review_webhook_signature_failures_total",
            "auto_review_webhooks_issue_comment_total",
            "auto_review_webhooks_other_total",
            "auto_review_webhooks_ping_total",
            "auto_review_webhooks_pull_request_total",
        ];
        // Counter families with labels: a # TYPE line, then one sample per
        // label value (pinned by their own tests), so no bare `name 0` line.
        const EXPECTED_LABELLED: &[&str] = &["auto_review_local_diff_fallbacks_total"];

        let m = Metrics::new();
        let out = m.render();
        for name in EXPECTED_LABELLED {
            let type_line = format!("# TYPE {name} counter");
            assert!(
                out.contains(&type_line),
                "render() missing # TYPE for {name}"
            );
        }
        for name in EXPECTED {
            // Each counter family must show up as both a # TYPE
            // line and a sample line.
            let type_line = format!("# TYPE {name} counter");
            assert!(
                out.contains(&type_line),
                "render() missing # TYPE for {name}"
            );
            let sample_line = format!("{name} 0\n");
            assert!(
                out.contains(&sample_line),
                "render() missing zero-value sample for {name}"
            );
        }

        // Also assert no unexpected counter families appear — if
        // someone adds one without updating EXPECTED, this catches
        // drift in the other direction.
        let actual: BTreeSet<&str> = out
            .lines()
            .filter_map(|line| line.strip_prefix("# TYPE "))
            .filter_map(|rest| rest.strip_suffix(" counter"))
            .collect();
        let expected: BTreeSet<&str> = EXPECTED.iter().chain(EXPECTED_LABELLED).copied().collect();
        let extra: Vec<&&str> = actual.difference(&expected).collect();
        assert!(
            extra.is_empty(),
            "render() exposes counters not in EXPECTED: {extra:?}"
        );
    }

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
    fn duration_histogram_buckets_cumulatively() {
        let m = Arc::new(Metrics::new());
        let obs = MetricsObserver::new(Arc::clone(&m));
        // 500ms (in 1s bucket and up), 3s (in 5s+), 45s (in 60s+),
        // 200s (in 300s+), 700s (only +Inf).
        let durations_ms: [u64; 5] = [500, 3000, 45_000, 200_000, 700_000];
        for duration_ms in durations_ms {
            obs.record(ReviewObservation::Succeeded {
                duration: Duration::from_millis(duration_ms),
                findings_count: 0,
                verifier_dropped: 0,
            });
        }
        let out = m.render();
        // 1s bucket: only the 500ms review.
        assert!(
            out.contains("auto_review_review_duration_seconds_bucket{le=\"1\"} 1\n"),
            "{out}"
        );
        // 5s bucket: 500ms + 3s = 2 reviews.
        assert!(out.contains("auto_review_review_duration_seconds_bucket{le=\"5\"} 2\n"));
        // 30s bucket: still only 500ms + 3s — 45s falls outside.
        assert!(out.contains("auto_review_review_duration_seconds_bucket{le=\"30\"} 2\n"));
        // 60s bucket: 500ms + 3s + 45s = 3.
        assert!(out.contains("auto_review_review_duration_seconds_bucket{le=\"60\"} 3\n"));
        // 300s bucket: 500ms + 3s + 45s + 200s = 4.
        assert!(out.contains("auto_review_review_duration_seconds_bucket{le=\"300\"} 4\n"));
        // 600s: still 4 (the 700s review doesn't fit).
        assert!(out.contains("auto_review_review_duration_seconds_bucket{le=\"600\"} 4\n"));
        // +Inf includes everything.
        assert!(out.contains("auto_review_review_duration_seconds_bucket{le=\"+Inf\"} 5\n"));
        // Sum and count.
        assert!(out.contains("auto_review_review_duration_seconds_count 5\n"));
        // sum = 0.5 + 3 + 45 + 200 + 700 = 948.5 seconds
        assert!(
            out.contains("auto_review_review_duration_seconds_sum 948.5\n"),
            "{out}"
        );
    }

    #[test]
    fn duration_histogram_includes_failed_reviews() {
        let m = Arc::new(Metrics::new());
        let obs = MetricsObserver::new(Arc::clone(&m));
        obs.record(ReviewObservation::Failed {
            duration: Duration::from_millis(2500),
            error_class: "llm",
        });
        let out = m.render();
        // 2.5s falls into the 5s+ buckets.
        assert!(out.contains("auto_review_review_duration_seconds_bucket{le=\"1\"} 0\n"));
        assert!(out.contains("auto_review_review_duration_seconds_bucket{le=\"5\"} 1\n"));
        assert!(out.contains("auto_review_review_duration_seconds_count 1\n"));
    }

    #[test]
    fn duration_histogram_skipped_reviews_do_not_register() {
        let m = Arc::new(Metrics::new());
        let obs = MetricsObserver::new(Arc::clone(&m));
        obs.record(ReviewObservation::Skipped { reason: "same_sha" });
        let out = m.render();
        assert!(out.contains("auto_review_review_duration_seconds_bucket{le=\"+Inf\"} 0\n"));
        assert!(out.contains("auto_review_review_duration_seconds_count 0\n"));
    }

    #[test]
    fn duration_histogram_emits_help_and_type_lines() {
        let m = Metrics::new();
        let out = m.render();
        assert!(out.contains("# HELP auto_review_review_duration_seconds"));
        assert!(out.contains("# TYPE auto_review_review_duration_seconds histogram"));
    }

    #[test]
    fn fmt_bucket_bound_renders_integers_without_decimals() {
        assert_eq!(fmt_bucket_bound(1.0), "1");
        assert_eq!(fmt_bucket_bound(60.0), "60");
        assert_eq!(fmt_bucket_bound(0.5), "0.5");
    }

    #[test]
    fn queue_wait_observation_increments_counter() {
        let m = Arc::new(Metrics::new());
        let obs = MetricsObserver::new(Arc::clone(&m));
        obs.record(ReviewObservation::QueueWait);
        obs.record(ReviewObservation::QueueWait);
        let out = m.render();
        assert!(
            out.contains("auto_review_review_queue_waits_total 2\n"),
            "{out}"
        );
        // Other counters unchanged.
        assert!(out.contains("auto_review_reviews_started_total 0\n"));
    }

    #[test]
    fn verifier_dropped_counter_sums_across_reviews() {
        let m = Arc::new(Metrics::new());
        let obs = MetricsObserver::new(Arc::clone(&m));
        obs.record(ReviewObservation::Succeeded {
            duration: Duration::from_millis(100),
            findings_count: 5,
            verifier_dropped: 2,
        });
        obs.record(ReviewObservation::Succeeded {
            duration: Duration::from_millis(100),
            findings_count: 3,
            verifier_dropped: 1,
        });
        // Failed observations don't carry verifier_dropped (no
        // post-verifier output to compare against).
        obs.record(ReviewObservation::Failed {
            duration: Duration::from_millis(50),
            error_class: "llm",
        });
        let out = m.render();
        assert!(
            out.contains("auto_review_verifier_findings_dropped_total 3\n"),
            "expected sum 3 (2+1), got:\n{out}"
        );
        // Findings sum still tracks what got POSTED, not what
        // was dropped.
        assert!(out.contains("auto_review_review_findings_sum 8\n"));
    }

    #[test]
    fn review_outcome_counters_route_by_class() {
        let m = Arc::new(Metrics::new());
        let obs = MetricsObserver::new(Arc::clone(&m));

        obs.record(ReviewObservation::Started);
        obs.record(ReviewObservation::Started);

        obs.record(ReviewObservation::Succeeded {
            duration: Duration::from_millis(1500),
            findings_count: 3,
            verifier_dropped: 0,
        });
        obs.record(ReviewObservation::Succeeded {
            duration: Duration::from_millis(500),
            findings_count: 1,
            verifier_dropped: 0,
        });

        obs.record(ReviewObservation::Failed {
            duration: Duration::from_millis(800),
            error_class: "llm",
        });
        obs.record(ReviewObservation::Failed {
            duration: Duration::from_millis(200),
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
    fn unknown_failure_class_routes_to_unknown_bucket_not_unhealable() {
        // Defense-in-depth: a new error_class string from
        // ar-orchestrator must NOT silently land in
        // reviews_failed_unhealable. Unknown classes go to a separate
        // bucket so operators can alert on the drift.
        let m = Metrics::new();
        m.record_review_failed(100, "definitely_not_a_real_class");
        let out = m.render();
        assert!(
            out.contains("auto_review_reviews_failed_unknown_total 1\n"),
            "unknown class should bucket to failed_unknown, got:\n{out}"
        );
        assert!(
            out.contains("auto_review_reviews_failed_unhealable_total 0\n"),
            "unknown class must not land in failed_unhealable (silent misclassification), got:\n{out}"
        );
        assert!(out.contains("auto_review_reviews_failed_forgejo_total 0\n"));
        assert!(out.contains("auto_review_reviews_failed_workspace_total 0\n"));
        assert!(out.contains("auto_review_reviews_failed_llm_total 0\n"));
        // Duration and completed-count still track unknown failures.
        assert!(out.contains("auto_review_reviews_completed_count 1\n"));
        assert!(out.contains("auto_review_review_duration_ms_sum 100\n"));
    }

    #[test]
    fn known_failure_classes_route_to_their_dedicated_buckets() {
        let m = Metrics::new();
        m.record_review_failed(10, "forgejo");
        m.record_review_failed(20, "workspace");
        m.record_review_failed(30, "llm");
        m.record_review_failed(40, "unhealable");
        m.record_review_failed(50, "panic");
        let out = m.render();
        assert!(out.contains("auto_review_reviews_failed_forgejo_total 1\n"));
        assert!(out.contains("auto_review_reviews_failed_workspace_total 1\n"));
        assert!(out.contains("auto_review_reviews_failed_llm_total 1\n"));
        assert!(out.contains("auto_review_reviews_failed_unhealable_total 1\n"));
        assert!(out.contains("auto_review_reviews_failed_panic_total 1\n"));
        assert!(out.contains("auto_review_reviews_failed_unknown_total 0\n"));
    }

    #[test]
    fn unknown_skip_reason_routes_to_unknown_bucket_not_disabled() {
        // Defense-in-depth: a new Skipped reason added in the
        // dispatcher must NOT silently land in
        // reviews_skipped_disabled. Unknown reasons go to a separate
        // bucket and tick reviews_skipped_unknown so operators can
        // alert on the drift.
        let m = Metrics::new();
        m.record_review_skipped("definitely_not_a_real_reason");
        let out = m.render();
        assert!(
            out.contains("auto_review_reviews_skipped_unknown_total 1\n"),
            "unknown reason should bucket to skipped_unknown, got:\n{out}"
        );
        assert!(
            out.contains("auto_review_reviews_skipped_disabled_total 0\n"),
            "unknown reason must not land in skipped_disabled (silent misclassification), got:\n{out}"
        );
        assert!(out.contains("auto_review_reviews_skipped_same_sha_total 0\n"));
        assert!(out.contains("auto_review_reviews_skipped_trivial_total 0\n"));
    }

    #[test]
    fn known_skip_reasons_route_to_their_dedicated_buckets() {
        let m = Metrics::new();
        m.record_review_skipped("same_sha");
        m.record_review_skipped("trivial_files");
        m.record_review_skipped("disabled_by_config");
        let out = m.render();
        assert!(out.contains("auto_review_reviews_skipped_same_sha_total 1\n"));
        assert!(out.contains("auto_review_reviews_skipped_trivial_total 1\n"));
        assert!(out.contains("auto_review_reviews_skipped_disabled_total 1\n"));
        assert!(out.contains("auto_review_reviews_skipped_unknown_total 0\n"));
    }

    #[test]
    fn local_diff_fallbacks_are_counted_by_reason() {
        let m = Arc::new(Metrics::new());
        let obs = MetricsObserver::new(Arc::clone(&m));
        obs.record(ReviewObservation::LocalDiffFallback {
            reason: "history_cap",
        });
        obs.record(ReviewObservation::LocalDiffFallback {
            reason: "history_cap",
        });
        obs.record(ReviewObservation::LocalDiffFallback {
            reason: "git_error",
        });
        let out = m.render();
        assert!(
            out.contains("# TYPE auto_review_local_diff_fallbacks_total counter\n"),
            "{out}"
        );
        assert!(
            out.contains("auto_review_local_diff_fallbacks_total{reason=\"history_cap\"} 2\n"),
            "{out}"
        );
        assert!(out.contains("auto_review_local_diff_fallbacks_total{reason=\"git_error\"} 1\n"));
        // Every known reason is exposed from zero, so rate() works at once.
        for reason in [
            "clone_failed",
            "no_merge_base",
            "timed_out",
            "fetch_failed",
            "unknown",
        ] {
            assert!(
                out.contains(&format!(
                    "auto_review_local_diff_fallbacks_total{{reason=\"{reason}\"}} 0\n"
                )),
                "{reason}: {out}"
            );
        }
    }

    #[test]
    fn clone_failures_are_counted_as_their_own_fallback_reason() {
        let m = Metrics::new();
        m.record_local_diff_fallback("clone_failed");
        let out = m.render();
        assert!(
            out.contains("auto_review_local_diff_fallbacks_total{reason=\"clone_failed\"} 1\n"),
            "{out}"
        );
    }

    #[test]
    fn unknown_local_diff_fallback_reason_routes_to_unknown() {
        let m = Metrics::new();
        m.record_local_diff_fallback("definitely_not_a_reason");
        let out = m.render();
        assert!(
            out.contains("auto_review_local_diff_fallbacks_total{reason=\"unknown\"} 1\n"),
            "{out}"
        );
        assert!(!out.contains("definitely_not_a_reason"), "{out}");
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

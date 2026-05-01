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

/// Bucket bounds (in seconds) for the review-duration histogram.
/// Tuned for review work: most fast on cached PRs, long-tail
/// reaching minutes on big diffs with cloud LLMs. Operators wanting
/// a different distribution can derive their own from the raw
/// `review_duration_ms_sum + reviews_completed_count` pair.
const DURATION_BUCKETS_SECS: &[f64] = &[1.0, 5.0, 15.0, 30.0, 60.0, 120.0, 300.0, 600.0];

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
    pub verifier_findings_dropped: AtomicU64,

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
    duration_buckets: [AtomicU64; 8],
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

    pub fn record_rate_limited(&self) {
        self.webhook_rate_limited.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_duplicate(&self) {
        self.webhook_duplicates.fetch_add(1, Ordering::Relaxed);
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
        self.bucket_duration(duration_ms);
    }

    /// Increment every histogram bucket whose upper bound is >= the
    /// observed duration. Cumulative-bucket convention per the
    /// Prometheus exposition format.
    fn bucket_duration(&self, duration_ms: u64) {
        let secs = duration_ms as f64 / 1000.0;
        for (i, &bound) in DURATION_BUCKETS_SECS.iter().enumerate() {
            if secs <= bound {
                self.duration_buckets[i].fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    pub fn record_poll_cycle(&self) {
        self.poll_cycles.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_poll_history_failure(&self) {
        self.poll_history_failures.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_poll_pr_failure(&self) {
        self.poll_pr_failures.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_poll_mention_dispatched(&self) {
        self.poll_mentions_dispatched
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_poll_chat_failure(&self) {
        self.poll_chat_failures.fetch_add(1, Ordering::Relaxed);
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
            (
                "auto_review_verifier_findings_dropped_total",
                "Findings the cheap-tier verifier corrected away. Sustained increases relative to review_findings_sum mean the reasoning model is hallucinating; consider switching to a higher-quality model.",
                &self.verifier_findings_dropped,
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
        self.render_duration_histogram(&mut out);
        out
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
        for (i, &bound) in DURATION_BUCKETS_SECS.iter().enumerate() {
            let count = self.duration_buckets[i].load(Ordering::Relaxed);
            // Use `{:.3}` so 0.5 doesn't render as `0` and integer
            // bounds don't grow trailing zeros beyond reason.
            out.push_str(&format!(
                "auto_review_review_duration_seconds_bucket{{le=\"{}\"}} {}\n",
                fmt_bucket_bound(bound),
                count
            ));
        }
        let total = self.reviews_completed_count.load(Ordering::Relaxed);
        out.push_str(&format!(
            "auto_review_review_duration_seconds_bucket{{le=\"+Inf\"}} {total}\n"
        ));
        let sum_ms = self.review_duration_ms_sum.load(Ordering::Relaxed);
        let sum_secs = sum_ms as f64 / 1000.0;
        out.push_str(&format!(
            "auto_review_review_duration_seconds_sum {sum_secs}\n"
        ));
        out.push_str(&format!(
            "auto_review_review_duration_seconds_count {total}\n"
        ));
    }
}

/// Render a bucket bound the way Prometheus expects: integers as
/// `60`, fractions as `0.5`. Avoids `60.0` which is technically
/// permitted but visually noisy.
fn fmt_bucket_bound(b: f64) -> String {
    if b == b.trunc() {
        format!("{}", b as i64)
    } else {
        format!("{b}")
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
                verifier_dropped,
            } => self.metrics.record_review_succeeded(
                duration.as_millis() as u64,
                findings_count as u64,
                verifier_dropped as u64,
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

    /// Cross-file contract test: every `auto_review_*` metric the
    /// shipped Grafana dashboard references must exist in the
    /// rendered `/metrics` output OR be a recording rule defined in
    /// the Prometheus rules file. Catches drift when someone
    /// renames a counter without updating the dashboard.
    #[test]
    fn shipped_grafana_dashboard_only_references_real_metrics() {
        let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap();
        let dashboard_path = manifest.join("deploy/grafana/auto_review.dashboard.json");
        let rules_path = manifest.join("deploy/prometheus/auto_review.rules.yaml");
        let dashboard = std::fs::read_to_string(&dashboard_path)
            .unwrap_or_else(|e| panic!("reading {}: {e}", dashboard_path.display()));
        let rules = std::fs::read_to_string(&rules_path)
            .unwrap_or_else(|e| panic!("reading {}: {e}", rules_path.display()));

        let referenced = collect_metric_tokens(&dashboard);

        let exposed = exposed_metric_canonical_names();

        // Recording-rule names look like `auto_review:foo`. Pull
        // them out of the rules file too — the dashboard may
        // reference them legitimately.
        let mut recording_rule_names = std::collections::BTreeSet::<String>::new();
        for line in rules.lines() {
            if let Some(rest) = line.trim_start().strip_prefix("- record:") {
                let name = rest.trim().trim_matches('"').to_string();
                recording_rule_names.insert(name);
            }
        }
        // Filter out recording-rule references — they're valid
        // even though they don't appear in /metrics.
        let referenced_metrics: std::collections::BTreeSet<String> = referenced
            .into_iter()
            .filter(|t| !recording_rule_names.contains(t) && !t.contains(':'))
            .collect();

        let unknown: Vec<&String> = referenced_metrics.difference(&exposed).collect();
        assert!(
            unknown.is_empty(),
            "dashboard references metrics not exposed: {unknown:?}"
        );
    }

    /// Helper: extract every `auto_review_<name>` or
    /// `auto_review:<name>` token that appears in `text`,
    /// normalising histogram suffixes. A bare `auto_review`
    /// (in titles, tags, etc.) is skipped.
    fn collect_metric_tokens(text: &str) -> std::collections::BTreeSet<String> {
        let mut out = std::collections::BTreeSet::new();
        for line in text.lines() {
            let mut idx = 0;
            while let Some(start) = line[idx..].find("auto_review") {
                let abs = idx + start;
                let end_byte = line[abs..]
                    .find(|c: char| !c.is_ascii_alphanumeric() && c != '_' && c != ':')
                    .map(|n| abs + n)
                    .unwrap_or(line.len());
                let token = &line[abs..end_byte];
                // Reject tokens that are just the bare prefix
                // (`auto_review` with no trailing identifier).
                let after_prefix = &token["auto_review".len()..];
                let starts_with_separator = after_prefix.starts_with('_')
                    || after_prefix.starts_with(':');
                if starts_with_separator && after_prefix.len() > 1 {
                    let canonical = token
                        .trim_end_matches("_bucket")
                        .trim_end_matches("_sum")
                        .trim_end_matches("_count");
                    out.insert(canonical.to_string());
                }
                idx = end_byte;
            }
        }
        out
    }

    /// Helper: parse the `Metrics::render()` output into a
    /// canonical-name set (no histogram suffixes, no labels).
    fn exposed_metric_canonical_names() -> std::collections::BTreeSet<String> {
        let mut out = std::collections::BTreeSet::new();
        let exposed_text = Metrics::new().render();
        for line in exposed_text.lines() {
            let line = line.trim_start();
            if line.starts_with("# ") || line.is_empty() {
                continue;
            }
            let name = line
                .split([' ', '{'])
                .next()
                .unwrap_or("")
                .trim_end_matches("_bucket")
                .trim_end_matches("_sum")
                .trim_end_matches("_count");
            if !name.is_empty() {
                out.insert(name.to_string());
            }
        }
        out
    }

    /// Cross-file contract test: every `auto_review_*` metric the
    /// shipped Prometheus rules file references must exist in the
    /// rendered `/metrics` output. Catches drift when someone
    /// renames a counter without updating
    /// `deploy/prometheus/auto_review.rules.yaml`.
    #[test]
    fn shipped_prometheus_rules_reference_only_real_metrics() {
        let rules_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("deploy/prometheus/auto_review.rules.yaml");
        let rules = std::fs::read_to_string(&rules_path)
            .unwrap_or_else(|e| panic!("reading {}: {e}", rules_path.display()));

        let referenced = collect_metric_tokens(&rules);
        let exposed = exposed_metric_canonical_names();

        // Recording-rule outputs are derived names that look like
        // `auto_review:foo`; ignore those (they're prefixed with a
        // colon, not an underscore).
        let referenced: std::collections::BTreeSet<String> = referenced
            .into_iter()
            .filter(|r| !r.contains(':'))
            .collect();

        let unknown: Vec<&String> = referenced.difference(&exposed).collect();
        assert!(
            unknown.is_empty(),
            "rules reference metrics that don't exist in /metrics: {unknown:?}"
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
        let obs = MetricsObserver::new(m.clone());
        // 500ms (in 1s bucket and up), 3s (in 5s+), 45s (in 60s+),
        // 200s (in 300s+), 700s (only +Inf).
        for d in [500u64, 3000, 45_000, 200_000, 700_000] {
            obs.record(ReviewObservation::Succeeded {
                duration: std::time::Duration::from_millis(d),
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
        let obs = MetricsObserver::new(m.clone());
        obs.record(ReviewObservation::Failed {
            duration: std::time::Duration::from_millis(2500),
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
        let obs = MetricsObserver::new(m.clone());
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
    fn verifier_dropped_counter_sums_across_reviews() {
        let m = Arc::new(Metrics::new());
        let obs = MetricsObserver::new(m.clone());
        obs.record(ReviewObservation::Succeeded {
            duration: std::time::Duration::from_millis(100),
            findings_count: 5,
            verifier_dropped: 2,
        });
        obs.record(ReviewObservation::Succeeded {
            duration: std::time::Duration::from_millis(100),
            findings_count: 3,
            verifier_dropped: 1,
        });
        // Failed observations don't carry verifier_dropped (no
        // post-verifier output to compare against).
        obs.record(ReviewObservation::Failed {
            duration: std::time::Duration::from_millis(50),
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
        let obs = MetricsObserver::new(m.clone());

        obs.record(ReviewObservation::Started);
        obs.record(ReviewObservation::Started);

        obs.record(ReviewObservation::Succeeded {
            duration: std::time::Duration::from_millis(1500),
            findings_count: 3,
            verifier_dropped: 0,
        });
        obs.record(ReviewObservation::Succeeded {
            duration: std::time::Duration::from_millis(500),
            findings_count: 1,
            verifier_dropped: 0,
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

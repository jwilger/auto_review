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

use std::sync::atomic::{AtomicU64, Ordering};

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

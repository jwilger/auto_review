//! Agentic chat handler.
//!
//! Triggered by `@auto_review` mentions in PR comments. Maintains
//! per-thread conversation state and uses sandboxed tools (grep, cat,
//! ast-grep, optional test/build) to investigate and respond.
//!
//! Forgejo gotcha: `pull_request_review_comment` webhooks do not fire
//! reliably (gitea#26023); the handler also accepts `issue_comment`
//! events and falls back to polling for missed mentions.

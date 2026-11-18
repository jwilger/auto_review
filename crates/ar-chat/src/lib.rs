//! Agentic chat handler.
//!
//! Triggered by `@auto_review` mentions in PR comments. Maintains
//! per-thread conversation state, dispatches review actions, and uses
//! LLM-generated text for human-applied suggestions. It does not expose
//! shell, test, or build execution from chat.
//!
//! Currently shipping the command parser and the data types. The
//! webhook routing and per-command handlers (remember/forget against
//! the `LearningsStore`, `ReReview` against the orchestrator, freeform
//! against the chat-tier LLM) land in follow-up commits.
//!
//! Forgejo gotcha: `pull_request_review_comment` webhooks do not fire
//! reliably (gitea#26023); the handler also accepts `issue_comment`
//! events and falls back to polling for missed mentions.

extern crate alloc;

pub mod command;
pub mod handler;

#[expect(
    clippy::useless_attribute,
    reason = "clippy flags any lint attribute on a use item outside its fixed list, which omits \
              pub_use, yet the pub_use expectation below is what suppresses that lint here"
)]
#[expect(
    clippy::pub_use,
    reason = "ar-gateway (webhook, github_webhook, poller), ar-cli (commands) and this crate's \
              integration tests import these items from the crate root; dropping the re-exports \
              breaks them"
)]
pub use crate::{
    command::{parse_chat_command, ChatCommand},
    handler::{ChatContext, ChatError, ChatHandler},
};

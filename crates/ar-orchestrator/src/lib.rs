//! Per-PR durable workflow state machine.
//!
//! Each PR run is a row in `pr_run` (see `state.rs`); state transitions are
//! activity-driven and idempotent so the orchestrator can resume after crash.
//!
//! `dispatcher.rs` contains the production [`SpawningDispatcher`] that ties
//! webhook intake (in `ar-gateway`) to the review pipeline (in `ar-review`).

extern crate alloc;

pub mod dispatcher;
pub mod dynamodb_history;
pub mod review_history;
pub mod sqlite_history;
pub mod state;

#[expect(
    clippy::useless_attribute,
    reason = "clippy flags any lint attribute on a use item outside its fixed list, which omits \
              pub_use, yet the pub_use expectation below is what suppresses that lint here"
)]
#[expect(
    clippy::pub_use,
    reason = "ar-agentcore, ar-chat, ar-gateway (startup, webhook, poller, metrics) and ar-cli \
              (commands) import these items from the crate root; dropping the re-exports \
              breaks them"
)]
pub use crate::{
    dispatcher::{
        run_review_job, InlineDispatcher, JobDispatcher, NoOpDispatcher, ReviewJob,
        ReviewObservation, ReviewObserver, SpawningDispatcher,
    },
    dynamodb_history::{
        AwsDynamoDbReviewHistoryClient, DynamoDbReviewHistory, DynamoDbReviewHistoryClient,
        HistoryEpochSecondsClock, SystemHistoryEpochSecondsClock,
    },
    review_history::{HistoryError, InMemoryReviewHistory, PrKey, ReviewHistory},
    sqlite_history::SqliteReviewHistory,
};

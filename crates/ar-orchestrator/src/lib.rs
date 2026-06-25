//! Per-PR durable workflow state machine.
//!
//! Each PR run is a row in `pr_run` (see `state.rs`); state transitions are
//! activity-driven and idempotent so the orchestrator can resume after crash.
//!
//! `dispatcher.rs` contains the production [`SpawningDispatcher`] that ties
//! webhook intake (in `ar-gateway`) to the review pipeline (in `ar-review`).

pub mod dispatcher;
pub mod dynamodb_history;
pub mod review_history;
pub mod sqlite_history;
pub mod state;

pub use dispatcher::{
    run_review_job, InlineDispatcher, JobDispatcher, NoOpDispatcher, ReviewJob, ReviewObservation,
    ReviewObserver, SpawningDispatcher,
};
pub use dynamodb_history::{
    AwsDynamoDbReviewHistoryClient, DynamoDbReviewHistory, DynamoDbReviewHistoryClient,
    HistoryEpochSecondsClock, SystemHistoryEpochSecondsClock,
};
pub use review_history::{HistoryError, InMemoryReviewHistory, PrKey, ReviewHistory};
pub use sqlite_history::SqliteReviewHistory;

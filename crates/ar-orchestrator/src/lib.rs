//! Per-PR durable workflow state machine.
//!
//! Each PR run is a row in `pr_run` (see `state.rs`); state transitions are
//! activity-driven and idempotent so the orchestrator can resume after crash.

pub mod state;

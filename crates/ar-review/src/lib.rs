//! Review pipeline activities.
//!
//! Each activity is a step in the orchestrator's state machine. They share a
//! `ReviewContext` (PR diff, repo index handle, learnings handle, LLM router)
//! and produce structured intermediate results that flow into the next stage.

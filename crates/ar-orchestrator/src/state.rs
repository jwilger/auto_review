use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[expect(
    clippy::module_name_repetitions,
    reason = "PrRunState is the public name of the per-PR run state; renaming a pub item is out \
              of scope for a lint-only change"
)]
#[expect(
    clippy::arbitrary_source_item_ordering,
    reason = "variants are listed in pipeline order, the order `next` walks them; alphabetical \
              order would hide that sequence"
)]
pub enum PrRunState {
    Intake,
    CloneRepo,
    Triage,
    Summarize,
    StaticAnalysis,
    IndexDelta,
    ContextCurate,
    ReviewFanout,
    Verify,
    SelfHeal,
    Walkthrough,
    PreMergeChecks,
    PostReview,
    PostStatus,
    Done,
    Failed,
}

impl PrRunState {
    /// Whether the run has finished, successfully or not.
    #[inline]
    #[must_use = "the check has no side effects"]
    pub fn is_terminal(self) -> bool {
        matches!(self, PrRunState::Done | PrRunState::Failed)
    }

    /// The state that follows this one, or `None` for a terminal state.
    #[inline]
    #[must_use = "computing the next state has no side effects"]
    pub fn next(self) -> Option<Self> {
        use PrRunState::{
            CloneRepo, ContextCurate, Done, Failed, IndexDelta, Intake, PostReview, PostStatus,
            PreMergeChecks, ReviewFanout, SelfHeal, StaticAnalysis, Summarize, Triage, Verify,
            Walkthrough,
        };
        match self {
            Intake => Some(CloneRepo),
            CloneRepo => Some(Triage),
            Triage => Some(Summarize),
            Summarize => Some(StaticAnalysis),
            StaticAnalysis => Some(IndexDelta),
            IndexDelta => Some(ContextCurate),
            ContextCurate => Some(ReviewFanout),
            ReviewFanout => Some(Verify),
            Verify => Some(SelfHeal),
            SelfHeal => Some(Walkthrough),
            Walkthrough => Some(PreMergeChecks),
            PreMergeChecks => Some(PostReview),
            PostReview => Some(PostStatus),
            PostStatus => Some(Done),
            Done | Failed => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pipeline_progresses_to_done() {
        let mut state = PrRunState::Intake;
        let mut steps: u32 = 0;
        while let Some(next) = state.next() {
            state = next;
            steps += 1;
            assert!(steps < 100, "infinite loop in state machine");
        }
        assert_eq!(state, PrRunState::Done);
    }
}

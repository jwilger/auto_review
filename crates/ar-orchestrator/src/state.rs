use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
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
    pub fn next(self) -> Option<Self> {
        use PrRunState::*;
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

    pub fn is_terminal(self) -> bool {
        matches!(self, PrRunState::Done | PrRunState::Failed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pipeline_progresses_to_done() {
        let mut s = PrRunState::Intake;
        let mut steps = 0;
        while let Some(next) = s.next() {
            s = next;
            steps += 1;
            assert!(steps < 100, "infinite loop in state machine");
        }
        assert_eq!(s, PrRunState::Done);
    }
}

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReviewComment {
    pub path: String,
    pub body: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub old_position: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub new_position: Option<u32>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ReviewEvent {
    Approved,
    RequestChanges,
    Comment,
    Pending,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CreateReviewRequest {
    pub body: String,
    pub commit_id: String,
    pub event: ReviewEvent,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub comments: Vec<ReviewComment>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ChangedFile {
    pub filename: String,
    pub status: String,
    #[serde(default)]
    pub additions: u32,
    #[serde(default)]
    pub deletions: u32,
    #[serde(default)]
    pub changes: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub patch: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum CommitStatusState {
    Pending,
    Success,
    Error,
    Failure,
    Warning,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CommitStatus {
    pub state: CommitStatusState,
    pub target_url: String,
    pub description: String,
    pub context: String,
}

//! Forgejo REST client.
//!
//! Targets the Gitea-compatible API. Key endpoints used:
//! - `GET /repos/{owner}/{repo}/pulls/{n}.diff`
//! - `GET /repos/{owner}/{repo}/pulls/{n}/files`
//! - `POST /repos/{owner}/{repo}/pulls/{n}/reviews`
//! - `POST /repos/{owner}/{repo}/statuses/{sha}`

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReviewComment {
    pub path: String,
    pub body: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub old_position: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub new_position: Option<u32>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ReviewEvent {
    Approved,
    RequestChanges,
    Comment,
    Pending,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateReviewRequest {
    pub body: String,
    pub commit_id: String,
    pub event: ReviewEvent,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub comments: Vec<ReviewComment>,
}

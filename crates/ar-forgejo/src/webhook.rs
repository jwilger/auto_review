//! Forgejo webhook payload types.
//!
//! Only the fields the reviewer actually needs are decoded; unknown fields
//! are ignored so payload-format drift across Forgejo versions doesn't break
//! ingestion.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PullRequestAction {
    Opened,
    Synchronized,
    Reopened,
    Closed,
    Edited,
    Labeled,
    Unlabeled,
    Assigned,
    Unassigned,
    ReviewRequested,
    ReviewRequestRemoved,
    ReadyForReview,
    Milestoned,
    Demilestoned,
    /// Action variants we don't act on still parse cleanly.
    #[serde(other)]
    Other,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct User {
    pub login: String,
    #[serde(default)]
    pub id: u64,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Repository {
    pub name: String,
    pub full_name: String,
    pub owner: User,
    pub default_branch: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct PullRequestRef {
    #[serde(rename = "ref")]
    pub ref_name: String,
    pub sha: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct PullRequest {
    pub number: u64,
    pub title: String,
    #[serde(default)]
    pub body: String,
    pub head: PullRequestRef,
    pub base: PullRequestRef,
    pub user: User,
    #[serde(default)]
    pub draft: bool,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct PullRequestEvent {
    pub action: PullRequestAction,
    pub number: u64,
    pub pull_request: PullRequest,
    pub repository: Repository,
    pub sender: User,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_typical_pr_opened_payload() {
        let raw = serde_json::json!({
            "action": "opened",
            "number": 7,
            "pull_request": {
                "number": 7,
                "title": "fix: thing",
                "body": "",
                "draft": false,
                "user": {"login": "alice", "id": 1},
                "head": {"ref": "topic", "sha": "deadbeef"},
                "base": {"ref": "main", "sha": "cafef00d"}
            },
            "repository": {
                "name": "r",
                "full_name": "o/r",
                "default_branch": "main",
                "owner": {"login": "o", "id": 99}
            },
            "sender": {"login": "alice", "id": 1}
        });
        let evt: PullRequestEvent = serde_json::from_value(raw).expect("decode");
        assert_eq!(evt.action, PullRequestAction::Opened);
        assert_eq!(evt.pull_request.head.sha, "deadbeef");
    }

    #[test]
    fn unknown_action_falls_into_other() {
        let raw = serde_json::json!({
            "action": "transmogrified",
            "number": 1,
            "pull_request": {
                "number": 1, "title": "x", "body": "",
                "user": {"login": "u", "id": 1},
                "head": {"ref": "t", "sha": "a"},
                "base": {"ref": "main", "sha": "b"}
            },
            "repository": {
                "name": "r", "full_name": "o/r", "default_branch": "main",
                "owner": {"login": "o", "id": 1}
            },
            "sender": {"login": "u", "id": 1}
        });
        let evt: PullRequestEvent = serde_json::from_value(raw).expect("decode");
        assert_eq!(evt.action, PullRequestAction::Other);
    }
}

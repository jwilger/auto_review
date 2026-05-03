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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct PullRequestEvent {
    pub action: PullRequestAction,
    pub number: u64,
    pub pull_request: PullRequest,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requested_reviewer: Option<User>,
    pub repository: Repository,
    pub sender: User,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IssueCommentAction {
    Created,
    Edited,
    Deleted,
    /// Action variants we don't act on still parse cleanly.
    #[serde(other)]
    Other,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Comment {
    pub id: u64,
    #[serde(default)]
    pub body: String,
    pub user: User,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Issue {
    pub number: u64,
    #[serde(default)]
    pub title: String,
    /// Forgejo's `issue_comment` event fires for both issues and PR
    /// comments. When the underlying object is a PR, this carries
    /// metadata identifying it.
    #[serde(default)]
    pub pull_request: Option<IssuePullRequestRef>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct IssuePullRequestRef {
    /// HTML URL of the linked PR. Useful for distinguishing PR
    /// comments from plain issue comments.
    #[serde(default)]
    pub html_url: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct IssueCommentEvent {
    pub action: IssueCommentAction,
    pub comment: Comment,
    pub issue: Issue,
    pub repository: Repository,
    pub sender: User,
}

impl IssueCommentEvent {
    /// True iff the comment is on a pull request (not a plain issue).
    /// Forgejo's `issue_comment` event covers both; the chat handler
    /// only acts on PR comments.
    pub fn is_pull_request_comment(&self) -> bool {
        self.issue.pull_request.is_some()
    }
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
    fn decodes_typical_issue_comment_payload_on_pr() {
        let raw = serde_json::json!({
            "action": "created",
            "comment": {
                "id": 99,
                "body": "@auto_review remember always check error returns",
                "user": {"login": "alice", "id": 1}
            },
            "issue": {
                "number": 42,
                "title": "fix: thing",
                "pull_request": {"html_url": "https://forge/o/r/pulls/42"}
            },
            "repository": {
                "name": "r",
                "full_name": "o/r",
                "default_branch": "main",
                "owner": {"login": "o", "id": 99}
            },
            "sender": {"login": "alice", "id": 1}
        });
        let evt: IssueCommentEvent = serde_json::from_value(raw).expect("decode");
        assert_eq!(evt.action, IssueCommentAction::Created);
        assert!(evt.comment.body.contains("remember"));
        assert!(evt.is_pull_request_comment());
    }

    #[test]
    fn issue_comment_without_pull_request_ref_is_not_pr_comment() {
        let raw = serde_json::json!({
            "action": "created",
            "comment": {"id": 1, "body": "x", "user": {"login": "u", "id": 1}},
            "issue": {"number": 7, "title": "bug"},
            "repository": {
                "name": "r", "full_name": "o/r", "default_branch": "main",
                "owner": {"login": "o", "id": 1}
            },
            "sender": {"login": "u", "id": 1}
        });
        let evt: IssueCommentEvent = serde_json::from_value(raw).expect("decode");
        assert!(!evt.is_pull_request_comment());
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

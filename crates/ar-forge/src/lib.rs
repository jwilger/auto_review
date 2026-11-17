//! Provider-neutral repository host DTOs.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, thiserror::Error, PartialEq, Eq)]
#[error("repository host error: {message}")]
pub struct HostError {
    /// Human-readable failure description, rendered by `Display`.
    message: String,
}

impl HostError {
    #[must_use = "reading the message has no side effect"]
    #[inline]
    pub fn message(&self) -> &str {
        &self.message
    }

    #[inline]
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

/// Review features the connected repository host supports.
///
/// Hosts differ: GitHub supports all of these, while Forgejo gained
/// multi-line review comments in v16 and suggestion blocks, thread replies
/// and thread resolution in v18. Callers must check these flags and degrade
/// instead of assuming a feature exists.
///
/// [`HostCapabilities::default`] supports nothing: the safe fallback.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[expect(
    clippy::exhaustive_structs,
    reason = "ar-forgejo (capabilities_for_version) and ar-github build this by struct literal; \
              every consumer is in this publish = false workspace"
)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "each flag is an independent host feature that callers test on its own; \
              no state-machine or enum collapses these four orthogonal capabilities"
)]
pub struct HostCapabilities {
    /// Inline review comments can span a line range.
    pub multi_line_comments: bool,
    /// The bot can mark a review-comment thread as resolved.
    pub resolve_threads: bool,
    /// Review comments can carry a ```` ```suggestion ```` block the author
    /// can apply.
    pub suggestion_blocks: bool,
    /// The bot can reply inside an existing review-comment thread.
    pub thread_replies: bool,
}

impl HostCapabilities {
    /// Every capability supported.
    #[must_use = "building the capability set has no side effect"]
    #[inline]
    pub const fn all() -> Self {
        Self {
            multi_line_comments: true,
            resolve_threads: true,
            suggestion_blocks: true,
            thread_replies: true,
        }
    }
}

#[async_trait]
pub trait ReviewHost: Send + Sync {
    /// Login of the account this host acts as, used to find the bot's own
    /// reviews and comments. Implementations cache it per host.
    #[inline]
    async fn bot_login(&self) -> Result<String, HostError> {
        Err(HostError::new("bot login not available for this host"))
    }

    /// Review features this host supports. The default reports none, so a
    /// host that cannot tell gets the plain-comment fallback.
    #[inline]
    async fn capabilities(&self) -> HostCapabilities {
        HostCapabilities::default()
    }

    #[inline]
    async fn clone_url(&self, owner: &str, repo: &str) -> Result<String, HostError> {
        Err(HostError::new(format!(
            "clone URL not available for {owner}/{repo}"
        )))
    }

    async fn create_review(
        &self,
        owner: &str,
        repo: &str,
        pr_number: u64,
        request: &CreateReviewRequest,
    ) -> Result<CreatedReview, HostError>;

    /// Replace the body of an existing top-level issue/PR comment.
    ///
    /// Callers must pass only ids of comments the bot itself authored
    /// (ids returned by `post_issue_comment`, or comments filtered by
    /// the bot's login); see THREAT-MODEL.md T9.
    #[inline]
    async fn edit_issue_comment(
        &self,
        owner: &str,
        repo: &str,
        comment_id: u64,
        body: &str,
    ) -> Result<(), HostError> {
        let _: &str = body;
        Err(HostError::new(format!(
            "issue comment editing not available for {owner}/{repo} comment {comment_id}"
        )))
    }

    /// Replace the body of an existing inline review comment.
    ///
    /// Callers must pass only ids of comments the bot itself authored
    /// (ids from its own `create_review`, or comments filtered by the
    /// bot's login); see THREAT-MODEL.md T9.
    #[inline]
    async fn edit_review_comment(
        &self,
        owner: &str,
        repo: &str,
        comment_id: u64,
        body: &str,
    ) -> Result<(), HostError> {
        let _: &str = body;
        Err(HostError::new(format!(
            "review comment editing not available for {owner}/{repo} comment {comment_id}"
        )))
    }

    async fn get_compare_diff(
        &self,
        owner: &str,
        repo: &str,
        base: &str,
        head: &str,
    ) -> Result<String, HostError>;

    #[inline]
    async fn get_file_content(
        &self,
        owner: &str,
        repo: &str,
        file_path: &str,
        ref_: &str,
    ) -> Result<Option<String>, HostError> {
        Err(HostError::new(format!(
            "file content not available for {owner}/{repo}:{ref_}:{file_path}"
        )))
    }

    /// Fetch an issue (or a pull request, via the shared issues endpoint) by
    /// number. Returns `Ok(None)` when the host answers 404; any other
    /// failure is an error. `is_pull_request` tells issues and PRs apart.
    #[inline]
    async fn get_issue(
        &self,
        owner: &str,
        repo: &str,
        issue_number: u64,
    ) -> Result<Option<IssueSummary>, HostError> {
        Err(HostError::new(format!(
            "issue lookup not available for {owner}/{repo}#{issue_number}"
        )))
    }

    async fn get_pr_diff(
        &self,
        owner: &str,
        repo: &str,
        pr_number: u64,
    ) -> Result<String, HostError>;

    async fn get_pull_request(
        &self,
        owner: &str,
        repo: &str,
        pr_number: u64,
    ) -> Result<PullRequestSummary, HostError>;

    async fn list_changed_files(
        &self,
        owner: &str,
        repo: &str,
        pr_number: u64,
    ) -> Result<Vec<ChangedFile>, HostError>;

    async fn list_pr_review_comments(
        &self,
        owner: &str,
        repo: &str,
        pr_number: u64,
    ) -> Result<Vec<PrReviewComment>, HostError>;

    async fn list_pull_review_comments(
        &self,
        owner: &str,
        repo: &str,
        pr_number: u64,
        review_id: u64,
    ) -> Result<Vec<PrReviewComment>, HostError>;

    async fn list_pull_reviews(
        &self,
        owner: &str,
        repo: &str,
        pr_number: u64,
    ) -> Result<Vec<PullReviewSummary>, HostError>;

    async fn post_commit_status(
        &self,
        owner: &str,
        repo: &str,
        sha: &str,
        status: &CommitStatus,
    ) -> Result<(), HostError>;

    #[inline]
    async fn post_issue_comment(
        &self,
        owner: &str,
        repo: &str,
        issue_number: u64,
        body: &str,
    ) -> Result<u64, HostError> {
        let _: &str = body;
        Err(HostError::new(format!(
            "issue comments not available for {owner}/{repo}#{issue_number}"
        )))
    }

    async fn update_pull_request(
        &self,
        owner: &str,
        repo: &str,
        pr_number: u64,
        title: Option<&str>,
        body: Option<&str>,
    ) -> Result<(), HostError>;
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[expect(
    clippy::exhaustive_structs,
    reason = "ar-review (mapping), ar-forgejo and ar-chat build inline comments by struct literal; \
              every consumer is in this publish = false workspace"
)]
pub struct ReviewComment {
    pub body: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub new_position: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub old_position: Option<u32>,
    pub path: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ReviewEvent {
    Approved,
    Comment,
    Pending,
    RequestChanges,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[expect(
    clippy::exhaustive_structs,
    reason = "ar-review (mapping), ar-forgejo and ar-chat build review requests by struct literal; \
              every consumer is in this publish = false workspace"
)]
pub struct CreateReviewRequest {
    pub body: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub comments: Vec<ReviewComment>,
    pub commit_id: String,
    pub event: ReviewEvent,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[expect(
    clippy::exhaustive_structs,
    reason = "ReviewHost fakes in ar-review, ar-orchestrator and ar-chat return this by struct literal; \
              every consumer is in this publish = false workspace"
)]
pub struct CreatedReview {
    pub id: u64,
    #[serde(default)]
    pub state: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[expect(
    clippy::exhaustive_structs,
    reason = "ar-review (triage, ignored-path filtering) and ar-orchestrator build changed files by \
              struct literal; every consumer is in this publish = false workspace"
)]
pub struct ChangedFile {
    #[serde(default)]
    pub additions: u32,
    #[serde(default)]
    pub changes: u32,
    #[serde(default)]
    pub deletions: u32,
    pub filename: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub patch: Option<String>,
    pub status: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum CommitStatusState {
    Error,
    Failure,
    Pending,
    Success,
    Warning,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[expect(
    clippy::exhaustive_structs,
    reason = "ar-orchestrator (dispatcher) and ar-forgejo build commit statuses by struct literal; \
              every consumer is in this publish = false workspace"
)]
pub struct CommitStatus {
    pub context: String,
    pub description: String,
    pub state: CommitStatusState,
    pub target_url: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[expect(
    clippy::exhaustive_structs,
    reason = "ReviewHost fakes in ar-review, ar-orchestrator and ar-chat build PR summaries by \
              struct literal; every consumer is in this publish = false workspace"
)]
pub struct PullRequestSummary {
    pub base: PullRequestRefSummary,
    #[serde(default, deserialize_with = "empty_string_when_null")]
    pub body: String,
    #[serde(default)]
    pub draft: bool,
    pub head: PullRequestRefSummary,
    pub number: u64,
    #[serde(default = "default_pr_state")]
    pub state: String,
    pub title: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[expect(
    clippy::exhaustive_structs,
    reason = "ar-review (pipeline, trusted_files), ar-orchestrator and ar-chat build head/base refs \
              by struct literal; every consumer is in this publish = false workspace"
)]
pub struct PullRequestRefSummary {
    #[serde(rename = "ref")]
    pub ref_name: String,
    pub sha: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[expect(
    clippy::exhaustive_structs,
    reason = "wire DTO that ar-forgejo and ar-github deserialize and callers read field by field; \
              every consumer is in this publish = false workspace, so new fields are coordinated edits"
)]
pub struct PullReviewSummary {
    /// Number of inline (code) comments the review holds, when the host
    /// reports it. `None` means unknown, not zero.
    #[serde(default)]
    pub comments_count: Option<u64>,
    pub id: u64,
    #[serde(default)]
    pub state: String,
    #[serde(default)]
    pub user: PrReviewCommentUser,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[expect(
    clippy::exhaustive_structs,
    reason = "ar-gateway (poller) builds review comments by struct literal; \
              every consumer is in this publish = false workspace"
)]
pub struct PrReviewComment {
    #[serde(default)]
    pub body: String,
    pub id: u64,
    /// Old-side (base) line the inline comment is anchored to; zero for
    /// comments on the new side of the diff. See `position`.
    #[serde(default, deserialize_with = "zero_when_null")]
    pub original_position: u64,
    /// File path an inline review comment is anchored to. Empty for
    /// top-level conversation comments.
    #[serde(default, deserialize_with = "empty_string_when_null")]
    pub path: String,
    /// New-side (head) line the inline comment is anchored to. Forgejo
    /// sets exactly one of `position` and `original_position`, depending
    /// on which side of the diff the comment is on, and leaves the other
    /// at zero. Replies in the same thread share `path`, `position` and
    /// `original_position`. Zero for top-level conversation comments.
    #[serde(default, deserialize_with = "zero_when_null")]
    pub position: u64,
    #[serde(default)]
    pub user: PrReviewCommentUser,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[expect(
    clippy::exhaustive_structs,
    reason = "wire DTO for a comment or review author that ar-forgejo re-exports and callers read \
              by `.login`; every consumer is in this publish = false workspace"
)]
pub struct PrReviewCommentUser {
    #[serde(default)]
    pub login: String,
}

/// Host-neutral issue summary returned by [`ReviewHost::get_issue`].
/// `is_pull_request` is true when the number names a pull request rather
/// than a plain issue (both hosts serve PRs on the issues endpoint).
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[expect(
    clippy::exhaustive_structs,
    reason = "ar-forgejo and ar-github map their issue payloads into this by struct literal; \
              every consumer is in this publish = false workspace"
)]
pub struct IssueSummary {
    #[serde(default, deserialize_with = "empty_string_when_null")]
    pub body: String,
    pub html_url: String,
    pub is_pull_request: bool,
    pub number: u64,
    pub state: String,
    pub title: String,
}

/// Serde default for [`PullRequestSummary::state`]: hosts that omit the
/// state only list open pull requests.
fn default_pr_state() -> String {
    "open".to_owned()
}

/// Deserialize a string that the host may send as JSON `null`, mapping
/// `null` to the empty string.
fn empty_string_when_null<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(Option::<String>::deserialize(deserializer)?.unwrap_or_default())
}

/// Deserialize a line position that the host may send as JSON `null`,
/// mapping `null` to zero.
fn zero_when_null<'de, D>(deserializer: D) -> Result<u64, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(Option::<u64>::deserialize(deserializer)?.unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use core::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    /// Minimal host that implements only the required methods. Every
    /// required method bumps `calls`, so a default edit method that
    /// tried to reach the host would show up as a non-zero count.
    #[derive(Default)]
    struct MinimalHost {
        calls: AtomicUsize,
    }

    impl MinimalHost {
        fn record<T>(&self) -> Result<T, HostError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Err(HostError::new("minimal host"))
        }
    }

    #[async_trait]
    impl ReviewHost for MinimalHost {
        async fn create_review(
            &self,
            _: &str,
            _: &str,
            _: u64,
            _: &CreateReviewRequest,
        ) -> Result<CreatedReview, HostError> {
            self.record()
        }
        async fn get_compare_diff(
            &self,
            _: &str,
            _: &str,
            _: &str,
            _: &str,
        ) -> Result<String, HostError> {
            self.record()
        }
        async fn get_pr_diff(&self, _: &str, _: &str, _: u64) -> Result<String, HostError> {
            self.record()
        }
        async fn get_pull_request(
            &self,
            _: &str,
            _: &str,
            _: u64,
        ) -> Result<PullRequestSummary, HostError> {
            self.record()
        }
        async fn list_changed_files(
            &self,
            _: &str,
            _: &str,
            _: u64,
        ) -> Result<Vec<ChangedFile>, HostError> {
            self.record()
        }
        async fn list_pr_review_comments(
            &self,
            _: &str,
            _: &str,
            _: u64,
        ) -> Result<Vec<PrReviewComment>, HostError> {
            self.record()
        }
        async fn list_pull_review_comments(
            &self,
            _: &str,
            _: &str,
            _: u64,
            _: u64,
        ) -> Result<Vec<PrReviewComment>, HostError> {
            self.record()
        }
        async fn list_pull_reviews(
            &self,
            _: &str,
            _: &str,
            _: u64,
        ) -> Result<Vec<PullReviewSummary>, HostError> {
            self.record()
        }
        async fn post_commit_status(
            &self,
            _: &str,
            _: &str,
            _: &str,
            _: &CommitStatus,
        ) -> Result<(), HostError> {
            self.record()
        }
        async fn update_pull_request(
            &self,
            _: &str,
            _: &str,
            _: u64,
            _: Option<&str>,
            _: Option<&str>,
        ) -> Result<(), HostError> {
            self.record()
        }
    }

    struct RequiredOnlyHost;

    #[async_trait]
    impl ReviewHost for RequiredOnlyHost {
        async fn create_review(
            &self,
            _owner: &str,
            _repo: &str,
            _pr_number: u64,
            _request: &CreateReviewRequest,
        ) -> Result<CreatedReview, HostError> {
            Err(HostError::new("unused"))
        }

        async fn get_compare_diff(
            &self,
            _owner: &str,
            _repo: &str,
            _base: &str,
            _head: &str,
        ) -> Result<String, HostError> {
            Err(HostError::new("unused"))
        }

        async fn get_pr_diff(
            &self,
            _owner: &str,
            _repo: &str,
            _pr_number: u64,
        ) -> Result<String, HostError> {
            Err(HostError::new("unused"))
        }

        async fn get_pull_request(
            &self,
            _owner: &str,
            _repo: &str,
            _pr_number: u64,
        ) -> Result<PullRequestSummary, HostError> {
            Err(HostError::new("unused"))
        }

        async fn list_changed_files(
            &self,
            _owner: &str,
            _repo: &str,
            _pr_number: u64,
        ) -> Result<Vec<ChangedFile>, HostError> {
            Err(HostError::new("unused"))
        }

        async fn list_pr_review_comments(
            &self,
            _owner: &str,
            _repo: &str,
            _pr_number: u64,
        ) -> Result<Vec<PrReviewComment>, HostError> {
            Err(HostError::new("unused"))
        }

        async fn list_pull_review_comments(
            &self,
            _owner: &str,
            _repo: &str,
            _pr_number: u64,
            _review_id: u64,
        ) -> Result<Vec<PrReviewComment>, HostError> {
            Err(HostError::new("unused"))
        }

        async fn list_pull_reviews(
            &self,
            _owner: &str,
            _repo: &str,
            _pr_number: u64,
        ) -> Result<Vec<PullReviewSummary>, HostError> {
            Err(HostError::new("unused"))
        }

        async fn post_commit_status(
            &self,
            _owner: &str,
            _repo: &str,
            _sha: &str,
            _status: &CommitStatus,
        ) -> Result<(), HostError> {
            Err(HostError::new("unused"))
        }

        async fn update_pull_request(
            &self,
            _owner: &str,
            _repo: &str,
            _pr_number: u64,
            _title: Option<&str>,
            _body: Option<&str>,
        ) -> Result<(), HostError> {
            Err(HostError::new("unused"))
        }
    }

    #[tokio::test]
    #[expect(
        clippy::expect_used,
        reason = "the default edit methods must fail; expect_err aborts the test with that \
                  expectation as the failure message"
    )]
    async fn default_edit_methods_return_err_without_touching_the_host() {
        let host = MinimalHost::default();

        let issue = host
            .edit_issue_comment("o", "r", 11, "new body")
            .await
            .expect_err("default edit_issue_comment must fail");
        let review = host
            .edit_review_comment("o", "r", 12, "new body")
            .await
            .expect_err("default edit_review_comment must fail");

        assert!(issue.message().contains("o/r"), "{issue}");
        assert!(issue.message().contains("11"), "{issue}");
        assert!(review.message().contains("o/r"), "{review}");
        assert!(review.message().contains("12"), "{review}");
        assert_eq!(host.calls.load(Ordering::SeqCst), 0);
    }

    #[test]
    #[expect(
        clippy::expect_used,
        reason = "a payload that fails to deserialize is the regression under test; expect aborts \
                  the test naming it"
    )]
    #[expect(
        clippy::default_numeric_fallback,
        reason = "json! integer literals stand in for host JSON numbers; their Rust type never \
                  reaches the deserializer"
    )]
    fn pull_request_summary_treats_null_body_as_empty() {
        let summary: PullRequestSummary = serde_json::from_value(serde_json::json!({
            "number": 7,
            "title": "fix: thing",
            "body": null,
            "head": {"ref": "feature", "sha": "abc"},
            "base": {"ref": "main", "sha": "def"}
        }))
        .expect("summary");

        assert_eq!(summary.body, "");
        assert_eq!(summary.state, "open");
    }

    #[test]
    #[expect(
        clippy::expect_used,
        reason = "a payload that fails to deserialize is the regression under test; expect aborts \
                  the test naming it"
    )]
    #[expect(
        clippy::default_numeric_fallback,
        reason = "json! integer literals stand in for host JSON numbers; their Rust type never \
                  reaches the deserializer"
    )]
    fn issue_summary_treats_null_or_missing_body_as_empty() {
        let null_body: IssueSummary = serde_json::from_value(serde_json::json!({
            "number": 12,
            "title": "bug: thing",
            "body": null,
            "state": "open",
            "is_pull_request": false,
            "html_url": "https://forge.example/o/r/issues/12"
        }))
        .expect("null body");
        let missing_body: IssueSummary = serde_json::from_value(serde_json::json!({
            "number": 12,
            "title": "bug: thing",
            "state": "open",
            "is_pull_request": false,
            "html_url": "https://forge.example/o/r/issues/12"
        }))
        .expect("missing body");

        assert_eq!(null_body.body, "");
        assert_eq!(missing_body, null_body);
    }

    #[tokio::test]
    #[expect(
        clippy::expect_used,
        reason = "the default get_issue must fail; expect_err aborts the test with that \
                  expectation as the failure message"
    )]
    async fn default_get_issue_returns_host_error_naming_the_issue() {
        let err = RequiredOnlyHost
            .get_issue("o", "r", 12)
            .await
            .expect_err("default get_issue must fail");

        assert!(err.message().contains("o/r#12"), "{}", err.message());
    }

    #[test]
    #[expect(
        clippy::expect_used,
        reason = "a payload that fails to deserialize is the regression under test; expect aborts \
                  the test naming it"
    )]
    #[expect(
        clippy::default_numeric_fallback,
        reason = "json! integer literals stand in for host JSON numbers; their Rust type never \
                  reaches the deserializer"
    )]
    fn pr_review_comment_reads_inline_thread_location_when_present() {
        let comment: PrReviewComment = serde_json::from_value(serde_json::json!({
            "id": 12,
            "body": "@auto-review help",
            "user": {"login": "bob"},
            "path": "src/lib.rs",
            "position": 4,
            "original_position": 0
        }))
        .expect("comment");

        assert_eq!(comment.path, "src/lib.rs");
        assert_eq!(comment.position, 4);
        assert_eq!(comment.original_position, 0);
    }

    #[test]
    #[expect(
        clippy::expect_used,
        reason = "a payload that fails to deserialize is the regression under test; expect aborts \
                  the test naming it"
    )]
    #[expect(
        clippy::default_numeric_fallback,
        reason = "json! integer literals stand in for host JSON numbers; their Rust type never \
                  reaches the deserializer"
    )]
    fn pr_review_comment_without_inline_location_still_deserializes() {
        let comment: PrReviewComment = serde_json::from_value(serde_json::json!({
            "id": 3,
            "body": "top-level",
            "user": {"login": "carol"}
        }))
        .expect("comment");

        assert_eq!(comment.path, "");
        assert_eq!(comment.position, 0);
        assert_eq!(comment.original_position, 0);
    }

    #[test]
    fn default_host_capabilities_support_nothing() {
        assert_eq!(
            HostCapabilities::default(),
            HostCapabilities {
                multi_line_comments: false,
                suggestion_blocks: false,
                thread_replies: false,
                resolve_threads: false,
            }
        );
    }
}

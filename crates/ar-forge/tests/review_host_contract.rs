use async_trait::async_trait;

#[derive(Default)]
struct FakeHost;

#[async_trait]
impl ar_forge::ReviewHost for FakeHost {
    async fn get_pull_request(
        &self,
        _owner: &str,
        _repo: &str,
        pr_number: u64,
    ) -> Result<ar_forge::PullRequestSummary, ar_forge::HostError> {
        Ok(ar_forge::PullRequestSummary {
            number: pr_number,
            title: "title".to_string(),
            body: "body".to_string(),
            draft: false,
            state: "open".to_string(),
            head: ar_forge::PullRequestRefSummary {
                ref_name: "feature".to_string(),
                sha: "head".to_string(),
            },
            base: ar_forge::PullRequestRefSummary {
                ref_name: "main".to_string(),
                sha: "base".to_string(),
            },
        })
    }

    async fn get_pr_diff(
        &self,
        _owner: &str,
        _repo: &str,
        _pr_number: u64,
    ) -> Result<String, ar_forge::HostError> {
        Ok("diff --git a/src/lib.rs b/src/lib.rs".to_string())
    }

    async fn get_compare_diff(
        &self,
        _owner: &str,
        _repo: &str,
        _base: &str,
        _head: &str,
    ) -> Result<String, ar_forge::HostError> {
        Ok("diff --git a/src/lib.rs b/src/lib.rs".to_string())
    }

    async fn list_changed_files(
        &self,
        _owner: &str,
        _repo: &str,
        _pr_number: u64,
    ) -> Result<Vec<ar_forge::ChangedFile>, ar_forge::HostError> {
        Ok(Vec::new())
    }

    async fn list_pr_review_comments(
        &self,
        _owner: &str,
        _repo: &str,
        _pr_number: u64,
    ) -> Result<Vec<ar_forge::PrReviewComment>, ar_forge::HostError> {
        Ok(Vec::new())
    }

    async fn list_pull_reviews(
        &self,
        _owner: &str,
        _repo: &str,
        _pr_number: u64,
    ) -> Result<Vec<ar_forge::PullReviewSummary>, ar_forge::HostError> {
        Ok(Vec::new())
    }

    async fn list_pull_review_comments(
        &self,
        _owner: &str,
        _repo: &str,
        _pr_number: u64,
        _review_id: u64,
    ) -> Result<Vec<ar_forge::PrReviewComment>, ar_forge::HostError> {
        Ok(Vec::new())
    }

    async fn update_pull_request(
        &self,
        _owner: &str,
        _repo: &str,
        _pr_number: u64,
        _title: Option<&str>,
        _body: Option<&str>,
    ) -> Result<(), ar_forge::HostError> {
        Ok(())
    }

    async fn post_commit_status(
        &self,
        _owner: &str,
        _repo: &str,
        _sha: &str,
        _status: &ar_forge::CommitStatus,
    ) -> Result<(), ar_forge::HostError> {
        Ok(())
    }

    async fn create_review(
        &self,
        _owner: &str,
        _repo: &str,
        _pr_number: u64,
        _request: &ar_forge::CreateReviewRequest,
    ) -> Result<ar_forge::CreatedReview, ar_forge::HostError> {
        Ok(ar_forge::CreatedReview {
            id: 1,
            state: "COMMENT".to_string(),
        })
    }
}

#[tokio::test]
async fn review_host_trait_is_implemented_from_common_crate() {
    let host: &dyn ar_forge::ReviewHost = &FakeHost;

    let diff = host.get_pr_diff("owner", "repo", 7).await.expect("diff");

    assert!(diff.starts_with("diff --git"));
}

#[tokio::test]
async fn default_capabilities_report_nothing_supported() {
    let host: &dyn ar_forge::ReviewHost = &FakeHost;

    assert_eq!(
        host.capabilities().await,
        ar_forge::HostCapabilities {
            multi_line_comments: false,
            suggestion_blocks: false,
            thread_replies: false,
            resolve_threads: false,
        }
    );
}

#[tokio::test]
async fn default_bot_login_is_an_error() {
    let host: &dyn ar_forge::ReviewHost = &FakeHost;

    let error = host.bot_login().await.expect_err("no default login");

    assert!(error.message().contains("bot login"), "{error}");
}

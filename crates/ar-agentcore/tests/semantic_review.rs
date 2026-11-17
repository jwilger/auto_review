extern crate alloc;

use alloc::sync::Arc;
use async_trait::async_trait;
use std::sync::Mutex;

#[derive(Default)]
struct FakeHost;

#[async_trait]
impl ar_forge::ReviewHost for FakeHost {
    async fn create_review(
        &self,
        _owner: &str,
        _repo: &str,
        _pr_number: u64,
        _request: &ar_forge::CreateReviewRequest,
    ) -> Result<ar_forge::CreatedReview, ar_forge::HostError> {
        Ok(ar_forge::CreatedReview {
            id: 1,
            state: "COMMENT".to_owned(),
        })
    }

    async fn get_compare_diff(
        &self,
        _owner: &str,
        _repo: &str,
        _base: &str,
        _head: &str,
    ) -> Result<String, ar_forge::HostError> {
        Ok(String::new())
    }

    async fn get_pr_diff(
        &self,
        _owner: &str,
        _repo: &str,
        _pr_number: u64,
    ) -> Result<String, ar_forge::HostError> {
        Ok(String::new())
    }

    async fn get_pull_request(
        &self,
        _owner: &str,
        _repo: &str,
        pr_number: u64,
    ) -> Result<ar_forge::PullRequestSummary, ar_forge::HostError> {
        Ok(ar_forge::PullRequestSummary {
            number: pr_number,
            title: "fix: semantic review".to_owned(),
            body: "review body".to_owned(),
            draft: false,
            state: "open".to_owned(),
            head: ar_forge::PullRequestRefSummary {
                ref_name: "feature".to_owned(),
                sha: "head-sha".to_owned(),
            },
            base: ar_forge::PullRequestRefSummary {
                ref_name: "main".to_owned(),
                sha: "base-sha".to_owned(),
            },
        })
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

    async fn list_pull_review_comments(
        &self,
        _owner: &str,
        _repo: &str,
        _pr_number: u64,
        _review_id: u64,
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

    async fn post_commit_status(
        &self,
        _owner: &str,
        _repo: &str,
        _sha: &str,
        _status: &ar_forge::CommitStatus,
    ) -> Result<(), ar_forge::HostError> {
        Ok(())
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
}

#[derive(Default)]
struct RecordingDispatcher {
    jobs: Mutex<Vec<ar_orchestrator::ReviewJob>>,
}

#[async_trait]
impl ar_orchestrator::JobDispatcher for RecordingDispatcher {
    #[expect(
        clippy::expect_used,
        reason = "JobDispatcher::dispatch returns (); a poisoned lock means another test thread \
                  already panicked, so panicking here is the honest test failure"
    )]
    async fn dispatch(&self, job: ar_orchestrator::ReviewJob) {
        self.jobs.lock().expect("jobs lock").push(job);
    }
}

#[tokio::test]
#[expect(
    clippy::tests_outside_test_module,
    reason = "files under tests/ are separate integration-test crates built only by cargo test; \
              there is no #[cfg(test)] module to place this in"
)]
#[expect(
    clippy::expect_used,
    reason = "handler results and lock guards are unwrapped inline so a failure names the step; \
              the assertions stay as written"
)]
#[expect(
    clippy::indexing_slicing,
    reason = "the preceding jobs.len() assertion proves jobs[0] exists; the assertions stay as written"
)]
async fn semantic_review_handler_dispatches_review_job_for_current_head() {
    let dispatcher = Arc::new(RecordingDispatcher::default());
    let handler = ar_agentcore::SemanticReviewHandler::new(
        Arc::new(FakeHost),
        Arc::<RecordingDispatcher>::clone(&dispatcher),
    );

    let outcome = ar_agentcore::InvocationHandler::handle(
        &handler,
        ar_agentcore::InvocationPayload {
            provider: ar_agentcore::Provider::Forgejo,
            kind: ar_agentcore::InvocationKind::SemanticReview,
            owner: "alice".to_owned(),
            repo: "widgets".to_owned(),
            pr_number: 42,
            head_sha: "head-sha".to_owned(),
            installation_id: None,
            force: Some(true),
            comment_id: None,
            comment_body: None,
        },
    )
    .await
    .expect("handler outcome");

    assert_eq!(outcome.status, "dispatched");
    let jobs = dispatcher.jobs.lock().expect("jobs lock");
    assert_eq!(jobs.len(), 1);
    assert_eq!(jobs[0].owner, "alice");
    assert_eq!(jobs[0].repo, "widgets");
    assert_eq!(jobs[0].pr_number, 42);
    assert_eq!(jobs[0].head_sha, "head-sha");
    assert_eq!(jobs[0].pr_title, "fix: semantic review");
    assert_eq!(jobs[0].pr_body, "review body");
    assert!(jobs[0].force);
}

#[tokio::test]
#[expect(
    clippy::tests_outside_test_module,
    reason = "files under tests/ are separate integration-test crates built only by cargo test; \
              there is no #[cfg(test)] module to place this in"
)]
#[expect(
    clippy::expect_used,
    reason = "handler results and lock guards are unwrapped inline so a failure names the step; \
              the assertions stay as written"
)]
async fn semantic_review_handler_rejects_stale_head_without_dispatching() {
    let dispatcher = Arc::new(RecordingDispatcher::default());
    let handler = ar_agentcore::SemanticReviewHandler::new(
        Arc::new(FakeHost),
        Arc::<RecordingDispatcher>::clone(&dispatcher),
    );

    let error = ar_agentcore::InvocationHandler::handle(
        &handler,
        ar_agentcore::InvocationPayload {
            provider: ar_agentcore::Provider::Forgejo,
            kind: ar_agentcore::InvocationKind::SemanticReview,
            owner: "alice".to_owned(),
            repo: "widgets".to_owned(),
            pr_number: 42,
            head_sha: "stale-sha".to_owned(),
            installation_id: None,
            force: None,
            comment_id: None,
            comment_body: None,
        },
    )
    .await
    .expect_err("stale head should reject");

    assert_eq!(error.kind, ar_agentcore::InvocationErrorKind::StaleHead);
    assert!(dispatcher.jobs.lock().expect("jobs lock").is_empty());
}

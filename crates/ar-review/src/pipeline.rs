use crate::error::ReviewError;
use crate::heal::{generate_with_self_heal, HealConfig};
use crate::mapping::output_to_review_request;
use ar_forgejo::Client as ForgejoClient;
use ar_llm::Router as LlmRouter;
use ar_prompts::{render_review_prompt, system_prompt, ReviewPromptInputs};

#[derive(Debug, Clone)]
pub struct ReviewOutcome {
    pub findings_count: usize,
    pub review_id: u64,
}

/// End-to-end review activity for one PR.
///
/// Fetches the diff and changed-file list, calls the reasoning LLM with
/// self-heal validation, maps the structured output to a Forgejo review
/// request, and posts it. Returns the review id and finding count on
/// success.
#[allow(clippy::too_many_arguments)]
pub async fn review_pull_request(
    forgejo: &ForgejoClient,
    llm: &LlmRouter,
    owner: &str,
    repo: &str,
    pr_number: u64,
    head_sha: &str,
    pr_title: &str,
    pr_body: &str,
) -> Result<ReviewOutcome, ReviewError> {
    let diff = forgejo.get_pr_diff(owner, repo, pr_number).await?;
    let files = forgejo.list_changed_files(owner, repo, pr_number).await?;
    let changed_filenames: Vec<String> = files.iter().map(|f| f.filename.clone()).collect();

    let prompt = render_review_prompt(&ReviewPromptInputs {
        repo_full_name: &format!("{owner}/{repo}"),
        pr_number,
        pr_title,
        pr_body,
        diff: &diff,
        changed_files: &changed_filenames,
    });

    let output =
        generate_with_self_heal(llm, system_prompt(), &prompt, HealConfig::default()).await?;
    let findings_count = output.findings.len();

    let req = output_to_review_request(&output, head_sha);
    let created = forgejo.create_review(owner, repo, pr_number, &req).await?;

    Ok(ReviewOutcome {
        findings_count,
        review_id: created.id,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use ar_llm::{
        CompleteRequest, CompleteResponse, Error as LlmError, LlmProvider, ModelTier, Router,
    };
    use async_trait::async_trait;
    use std::sync::{Arc, Mutex};
    use wiremock::matchers::{body_partial_json, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    struct CannedProvider(Mutex<Vec<String>>);

    #[async_trait]
    impl LlmProvider for CannedProvider {
        async fn complete(&self, _req: CompleteRequest) -> Result<CompleteResponse, LlmError> {
            let next = self
                .0
                .lock()
                .unwrap()
                .pop()
                .unwrap_or_else(|| "{}".to_string());
            Ok(CompleteResponse {
                content: next,
                input_tokens: 0,
                output_tokens: 0,
            })
        }
    }

    #[tokio::test]
    async fn review_pull_request_end_to_end_happy_path() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/repos/o/r/pulls/7.diff"))
            .respond_with(ResponseTemplate::new(200).set_body_string("@@ -1 +1 @@\n+x\n"))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v1/repos/o/r/pulls/7/files"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {"filename": "src/x.rs", "status": "modified"}
            ])))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/api/v1/repos/o/r/pulls/7/reviews"))
            .and(body_partial_json(serde_json::json!({
                "commit_id": "deadbeef",
                "event": "COMMENT",
                "body": "looks fine"
            })))
            .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({
                "id": 1234,
                "state": "COMMENT"
            })))
            .mount(&server)
            .await;

        let forgejo = ForgejoClient::new(&server.uri(), "tok").expect("client");

        let provider = Arc::new(CannedProvider(Mutex::new(vec![
            r#"{"summary":"looks fine","findings":[]}"#.to_string(),
        ])));
        let llm = Router::new().with(ModelTier::Reasoning, provider);

        let outcome = review_pull_request(&forgejo, &llm, "o", "r", 7, "deadbeef", "title", "body")
            .await
            .expect("review ok");

        assert_eq!(outcome.review_id, 1234);
        assert_eq!(outcome.findings_count, 0);
    }

    #[tokio::test]
    async fn review_pull_request_propagates_forgejo_404_on_diff() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/repos/o/r/pulls/7.diff"))
            .respond_with(ResponseTemplate::new(404).set_body_string("nope"))
            .mount(&server)
            .await;

        let forgejo = ForgejoClient::new(&server.uri(), "tok").expect("client");
        let provider = Arc::new(CannedProvider(Mutex::new(vec![])));
        let llm = Router::new().with(ModelTier::Reasoning, provider);

        let err = review_pull_request(&forgejo, &llm, "o", "r", 7, "x", "t", "b")
            .await
            .expect_err("err");
        assert!(matches!(err, ReviewError::Forgejo(_)));
    }

    #[tokio::test]
    async fn review_pull_request_request_changes_when_error_severity_present() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/repos/o/r/pulls/7.diff"))
            .respond_with(ResponseTemplate::new(200).set_body_string("d"))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v1/repos/o/r/pulls/7/files"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([])))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/api/v1/repos/o/r/pulls/7/reviews"))
            .and(body_partial_json(serde_json::json!({
                "event": "REQUEST_CHANGES"
            })))
            .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({
                "id": 99,
                "state": "REQUEST_CHANGES"
            })))
            .mount(&server)
            .await;

        let forgejo = ForgejoClient::new(&server.uri(), "tok").expect("client");
        let bad = r#"{"summary":"break","findings":[
            {"path":"a","line_start":1,"severity":"error","message":"oops"}
        ]}"#;
        let provider = Arc::new(CannedProvider(Mutex::new(vec![bad.to_string()])));
        let llm = Router::new().with(ModelTier::Reasoning, provider);

        let outcome = review_pull_request(&forgejo, &llm, "o", "r", 7, "sha", "t", "b")
            .await
            .expect("ok");
        assert_eq!(outcome.findings_count, 1);
    }
}

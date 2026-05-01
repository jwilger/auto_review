use crate::agentic_verify::verify_findings_agentic;
use crate::diff::{cap_diff, DEFAULT_MAX_DIFF_BYTES};
use crate::error::ReviewError;
use crate::heal::{generate_with_self_heal, HealConfig};
use crate::ignored::{filter_changed_files, filter_diff_paths};
use crate::mapping::output_to_review_request;
use crate::pre_merge::{evaluate as evaluate_pre_merge_checks, render_section as render_pre_merge_section};
use crate::verify::verify_findings;
use ar_forgejo::Client as ForgejoClient;
use ar_llm::Router as LlmRouter;
use ar_prompts::{render_review_prompt, system_prompt, ReviewPromptInputs};
use ar_tools::Finding;
use globset::GlobSet;
use std::path::Path;

/// Which verifier the pipeline runs after the reasoning model emits
/// candidate findings. The `Simple` verifier is one cheap-tier call
/// against the diff alone; `Agentic` runs a per-finding ReAct loop
/// with read-only workspace tools (read_file / search) and needs a
/// cloned workspace to inspect.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum VerifyMode {
    #[default]
    Simple,
    Agentic,
}

#[derive(Debug, Clone)]
pub struct ReviewOutcome {
    pub findings_count: usize,
    pub review_id: u64,
}

/// All inputs to [`review_pull_request`]. Bundling them into a struct
/// keeps the call sites readable and makes adding new context (RAG
/// snippets, learnings, etc.) a one-line change instead of churning
/// every test.
pub struct ReviewArgs<'a> {
    pub forgejo: &'a ForgejoClient,
    pub llm: &'a LlmRouter,
    pub owner: &'a str,
    pub repo: &'a str,
    pub pr_number: u64,
    pub head_sha: &'a str,
    pub pr_title: &'a str,
    pub pr_body: &'a str,
    pub linter_findings: &'a [Finding],
    pub ignored_paths: &'a GlobSet,
    pub guidelines: &'a str,
    /// RAG-retrieved markdown context (similar code, learnings,
    /// co-change neighbors). Empty string when the index hasn't
    /// been built or returned no matches.
    pub repo_context: &'a str,
    /// Pre-fetched diff to use instead of `forgejo.get_pr_diff`.
    /// `Some` for incremental reviews where the orchestrator already
    /// fetched a `compare_diff(previous_sha..head_sha)`. `None` for
    /// normal full reviews.
    pub diff_override: Option<&'a str>,
    /// Verifier strategy. `Agentic` requires `workspace_path` to be
    /// `Some`; if it's not, the pipeline silently downgrades to
    /// `Simple` rather than failing the review.
    pub verify_mode: VerifyMode,
    /// Path to the cloned PR workspace. Required for the agentic
    /// verifier; ignored by the simple one.
    pub workspace_path: Option<&'a Path>,
}

/// End-to-end review activity for one PR.
///
/// Fetches the diff and changed-file list, calls the reasoning LLM with
/// self-heal validation, maps the structured output to a Forgejo review
/// request, and posts it. The orchestrator is responsible for cloning the
/// repo and running linters; their findings are passed in via
/// `linter_findings` and surfaced to the LLM as supplementary context.
pub async fn review_pull_request(args: ReviewArgs<'_>) -> Result<ReviewOutcome, ReviewError> {
    let raw_diff = match args.diff_override {
        Some(d) => d.to_string(),
        None => {
            args.forgejo
                .get_pr_diff(args.owner, args.repo, args.pr_number)
                .await?
        }
    };
    let pruned = filter_diff_paths(&raw_diff, args.ignored_paths);
    let diff = cap_diff(&pruned, DEFAULT_MAX_DIFF_BYTES);
    if diff.len() < raw_diff.len() {
        tracing::info!(
            original = raw_diff.len(),
            after_ignore = pruned.len(),
            after_cap = diff.len(),
            "diff filtered/capped before sending to LLM"
        );
    }
    let raw_files = args
        .forgejo
        .list_changed_files(args.owner, args.repo, args.pr_number)
        .await?;
    let files = filter_changed_files(&raw_files, args.ignored_paths);
    let changed_filenames: Vec<String> = files.iter().map(|f| f.filename.clone()).collect();

    let repo_full = format!("{}/{}", args.owner, args.repo);
    let prompt = render_review_prompt(&ReviewPromptInputs {
        repo_full_name: &repo_full,
        pr_number: args.pr_number,
        pr_title: args.pr_title,
        pr_body: args.pr_body,
        diff: &diff,
        changed_files: &changed_filenames,
        linter_findings: args.linter_findings,
        guidelines: args.guidelines,
        repo_context: args.repo_context,
    });

    let output =
        generate_with_self_heal(args.llm, system_prompt(), &prompt, HealConfig::default()).await?;

    // Optional second pass: when a Cheap tier is configured, verify
    // each finding against the diff and drop the ones the verifier
    // doesn't corroborate. Fails open — verifier issues never drop
    // real findings.
    let output = match (args.verify_mode, args.workspace_path) {
        (VerifyMode::Agentic, Some(workspace)) => {
            verify_findings_agentic(args.llm, output, workspace, &diff).await?
        }
        // Agentic was requested but the orchestrator didn't supply a
        // workspace; downgrade to the simple verifier silently rather
        // than failing the review.
        _ => verify_findings(args.llm, output, &diff).await?,
    };
    let findings_count = output.findings.len();

    let mut req = output_to_review_request(&output, args.head_sha);

    // Pre-merge checks: deterministic gates (CHANGELOG / tests /
    // TODOs) appended to the review body as an advisory checklist.
    // Failing a check does not change the review event; it just
    // surfaces a nudge to the PR author.
    let pre_merge_results =
        evaluate_pre_merge_checks(&diff, &files, args.workspace_path);
    let section = render_pre_merge_section(&pre_merge_results);
    if !section.is_empty() {
        if !req.body.is_empty() {
            req.body.push_str("\n\n");
        }
        req.body.push_str(&section);
    }

    let created = args
        .forgejo
        .create_review(args.owner, args.repo, args.pr_number, &req)
        .await?;

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
    use ar_tools::Severity;
    use async_trait::async_trait;
    use std::sync::{Arc, Mutex};
    use wiremock::matchers::{body_partial_json, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    /// Provider that records each request it receives and returns canned
    /// content from a stack (popped LIFO so callers list responses
    /// last-to-first).
    struct CannedProvider {
        responses: Mutex<Vec<String>>,
        seen: Mutex<Vec<CompleteRequest>>,
    }

    impl CannedProvider {
        fn new(responses: Vec<&str>) -> Self {
            Self {
                responses: Mutex::new(responses.into_iter().map(String::from).collect()),
                seen: Mutex::new(Vec::new()),
            }
        }

        fn last_user_prompt(&self) -> Option<String> {
            let seen = self.seen.lock().unwrap();
            seen.first().and_then(|req| {
                req.messages
                    .iter()
                    .find(|m| matches!(m.role, ar_llm::Role::User))
                    .map(|m| m.content.clone())
            })
        }
    }

    #[async_trait]
    impl LlmProvider for CannedProvider {
        async fn complete(&self, req: CompleteRequest) -> Result<CompleteResponse, LlmError> {
            self.seen.lock().unwrap().push(req);
            let next = self
                .responses
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

    fn router_with(provider: Arc<CannedProvider>) -> Router {
        Router::new().with(ModelTier::Reasoning, provider)
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
        // Body now ends in `## Pre-merge checks` because the
        // pipeline appends the deterministic check section.
        // The body field stays excluded from the matcher so this
        // test focuses on the integration contract (event +
        // commit_id) — body composition is covered by
        // mapping/pre_merge tests.
        Mock::given(method("POST"))
            .and(path("/api/v1/repos/o/r/pulls/7/reviews"))
            .and(body_partial_json(serde_json::json!({
                "commit_id": "deadbeef",
                "event": "COMMENT"
            })))
            .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({
                "id": 1234,
                "state": "COMMENT"
            })))
            .mount(&server)
            .await;

        let forgejo = ForgejoClient::new(&server.uri(), "tok").expect("client");
        let provider = Arc::new(CannedProvider::new(vec![
            r#"{"summary":"looks fine","findings":[]}"#,
        ]));
        let llm = router_with(provider);

        let outcome = review_pull_request(ReviewArgs {
            forgejo: &forgejo,
            llm: &llm,
            owner: "o",
            repo: "r",
            pr_number: 7,
            head_sha: "deadbeef",
            pr_title: "title",
            pr_body: "body",
            linter_findings: &[],
            ignored_paths: &GlobSet::empty(),
            guidelines: "",
            repo_context: "",
            diff_override: None,
            verify_mode: VerifyMode::Simple,
            workspace_path: None,
        })
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
        let provider = Arc::new(CannedProvider::new(vec![]));
        let llm = router_with(provider);

        let err = review_pull_request(ReviewArgs {
            forgejo: &forgejo,
            llm: &llm,
            owner: "o",
            repo: "r",
            pr_number: 7,
            head_sha: "x",
            pr_title: "t",
            pr_body: "b",
            linter_findings: &[],
            ignored_paths: &GlobSet::empty(),
            guidelines: "",
            repo_context: "",
            diff_override: None,
            verify_mode: VerifyMode::Simple,
            workspace_path: None,
        })
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
        let provider = Arc::new(CannedProvider::new(vec![bad]));
        let llm = router_with(provider);

        let outcome = review_pull_request(ReviewArgs {
            forgejo: &forgejo,
            llm: &llm,
            owner: "o",
            repo: "r",
            pr_number: 7,
            head_sha: "sha",
            pr_title: "t",
            pr_body: "b",
            linter_findings: &[],
            ignored_paths: &GlobSet::empty(),
            guidelines: "",
            repo_context: "",
            diff_override: None,
            verify_mode: VerifyMode::Simple,
            workspace_path: None,
        })
        .await
        .expect("ok");
        assert_eq!(outcome.findings_count, 1);
    }

    #[tokio::test]
    async fn review_pull_request_threads_linter_findings_into_llm_prompt() {
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
            .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({
                "id": 1, "state": "COMMENT"
            })))
            .mount(&server)
            .await;

        let forgejo = ForgejoClient::new(&server.uri(), "tok").expect("client");
        let provider = Arc::new(CannedProvider::new(vec![
            r#"{"summary":"x","findings":[]}"#,
        ]));
        let llm = router_with(provider.clone());

        let findings = vec![Finding {
            source_tool: "shellcheck".into(),
            rule_id: Some("SC2034".into()),
            path: "build.sh".into(),
            line_start: 3,
            line_end: 3,
            severity: Severity::Warning,
            message: "var unused".into(),
        }];

        review_pull_request(ReviewArgs {
            forgejo: &forgejo,
            llm: &llm,
            owner: "o",
            repo: "r",
            pr_number: 7,
            head_sha: "sha",
            pr_title: "t",
            pr_body: "b",
            linter_findings: &findings,
            ignored_paths: &GlobSet::empty(),
            guidelines: "",
            repo_context: "",
            diff_override: None,
            verify_mode: VerifyMode::Simple,
            workspace_path: None,
        })
        .await
        .expect("ok");

        let prompt = provider
            .last_user_prompt()
            .expect("LLM should have been called");
        assert!(prompt.to_lowercase().contains("static-analysis findings"));
        assert!(prompt.contains("shellcheck"));
        assert!(prompt.contains("SC2034"));
        assert!(prompt.contains("build.sh:3"));
    }
}

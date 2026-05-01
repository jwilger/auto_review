use crate::agentic_verify::verify_findings_agentic;
use crate::diff::{cap_diff, DEFAULT_MAX_DIFF_BYTES};
use crate::error::ReviewError;
use crate::heal::{generate_with_self_heal, HealConfig};
use crate::ignored::{filter_changed_files, filter_diff_paths};
use crate::mapping::output_to_review_request;
use crate::config::ReviewMode;
use crate::linter_only::build_linter_only_output;
use crate::pre_merge::{evaluate as evaluate_pre_merge_checks, render_combined_section};
use crate::pre_merge_llm::evaluate_custom_checks;
use crate::verify::verify_findings;
use ar_forgejo::Client as ForgejoClient;
use ar_llm::Router as LlmRouter;
use ar_prompts::{render_review_prompt, system_prompt, ReviewPromptInputs, ReviewSeverity};
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

/// Rank for ordered comparison: higher = more severe. Lets the
/// severity-floor filter use a `>=` comparison.
fn severity_rank(s: ReviewSeverity) -> u8 {
    match s {
        ReviewSeverity::Note => 0,
        ReviewSeverity::Warning => 1,
        ReviewSeverity::Error => 2,
    }
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
    /// Repo-author free-form pre-merge checks (from
    /// `.auto_review.yaml`'s `pre_merge_checks:`). Each entry is
    /// evaluated against the diff by the cheap LLM tier and added
    /// to the review body's checklist. Empty slice = no custom
    /// checks; the built-in deterministic checks still run.
    pub custom_pre_merge_checks: &'a [String],
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
    /// Review behaviour. `Full` runs the LLM pipeline (default).
    /// `LinterOnly` posts linter findings as-is, no LLM call —
    /// zero token cost, no semantic review. Selected per-repo via
    /// `.auto_review.yaml`'s `mode:` field.
    pub review_mode: ReviewMode,
    /// Drop findings below this severity before posting. `Note`
    /// (default) posts everything. `Warning` suppresses Note-only
    /// nits. `Error` suppresses everything below high-confidence
    /// problems — useful for low-noise operations on big diffs
    /// where stylistic notes drown out real issues.
    pub min_severity: ReviewSeverity,
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

    let mut output = match args.review_mode {
        ReviewMode::LinterOnly => {
            // Skip the LLM entirely. The orchestrator already ran the
            // linters; map their findings straight to the review
            // output and continue. No verifier — there's nothing
            // hallucinated to drop.
            build_linter_only_output(args.linter_findings)
        }
        ReviewMode::Full => {
            let output = generate_with_self_heal(
                args.llm,
                system_prompt(),
                &prompt,
                HealConfig::default(),
            )
            .await?;
            // Optional second pass: when a Cheap tier is configured,
            // verify each finding against the diff and drop the ones
            // the verifier doesn't corroborate. Fails open — verifier
            // issues never drop real findings.
            match (args.verify_mode, args.workspace_path) {
                (VerifyMode::Agentic, Some(workspace)) => {
                    verify_findings_agentic(args.llm, output, workspace, &diff).await?
                }
                // Agentic was requested but the orchestrator didn't
                // supply a workspace; downgrade to the simple verifier
                // silently rather than failing the review.
                _ => verify_findings(args.llm, output, &diff).await?,
            }
        }
    };
    // Severity-floor filter. Drops findings strictly below
    // args.min_severity before posting. Order: Note < Warning < Error.
    let original_count = output.findings.len();
    output
        .findings
        .retain(|f| severity_rank(f.severity) >= severity_rank(args.min_severity));
    if output.findings.len() != original_count {
        tracing::info!(
            kept = output.findings.len(),
            dropped = original_count - output.findings.len(),
            min_severity = ?args.min_severity,
            "severity floor applied"
        );
    }

    let findings_count = output.findings.len();

    let mut req = output_to_review_request(&output, args.head_sha);

    // Pre-merge checks: deterministic gates (CHANGELOG / tests /
    // TODOs) plus repo-author-supplied natural-language checks
    // evaluated by the cheap LLM tier. Both surface as a single
    // markdown checklist appended to the review body. Failing a
    // check is advisory and does not change the review event.
    let pre_merge_results =
        evaluate_pre_merge_checks(&diff, &files, args.workspace_path);
    let custom_results = if args.custom_pre_merge_checks.is_empty() {
        Vec::new()
    } else {
        evaluate_custom_checks(args.llm, args.custom_pre_merge_checks, &diff).await
    };
    let section = render_combined_section(&pre_merge_results, &custom_results);
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

    #[test]
    fn severity_rank_is_total_order() {
        assert!(severity_rank(ReviewSeverity::Note) < severity_rank(ReviewSeverity::Warning));
        assert!(severity_rank(ReviewSeverity::Warning) < severity_rank(ReviewSeverity::Error));
    }

    #[tokio::test]
    async fn severity_floor_warning_drops_note_findings_before_posting() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/repos/o/r/pulls/7.diff"))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                "diff --git a/x b/x\n@@ -1,2 +1,2 @@\n-old1\n+new1\n-old2\n+new2\n",
            ))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v1/repos/o/r/pulls/7/files"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {"filename": "x", "status": "modified"}
            ])))
            .mount(&server)
            .await;
        // Capture the posted review request so we can inspect
        // which findings made it through the floor.
        Mock::given(method("POST"))
            .and(path("/api/v1/repos/o/r/pulls/7/reviews"))
            .respond_with(
                ResponseTemplate::new(201).set_body_json(serde_json::json!({
                    "id": 1, "state": "COMMENT"
                })),
            )
            .mount(&server)
            .await;

        let forgejo = ForgejoClient::new(&server.uri(), "tok").expect("client");
        // The reasoning model emits three findings spanning every
        // severity. Floor should drop the Note one.
        let provider = Arc::new(CannedProvider::new(vec![r#"{
            "summary": "mixed",
            "findings": [
                {"path":"x","line_start":1,"severity":"note","message":"style"},
                {"path":"x","line_start":2,"severity":"warning","message":"bad"},
                {"path":"x","line_start":2,"severity":"error","message":"unsafe"}
            ]
        }"#]));
        let llm = router_with(provider);

        let outcome = review_pull_request(ReviewArgs {
            forgejo: &forgejo,
            llm: &llm,
            owner: "o",
            repo: "r",
            pr_number: 7,
            head_sha: "deadbeef",
            pr_title: "t",
            pr_body: "b",
            linter_findings: &[],
            ignored_paths: &GlobSet::empty(),
            custom_pre_merge_checks: &[],
            guidelines: "",
            repo_context: "",
            diff_override: None,
            verify_mode: VerifyMode::Simple,
            workspace_path: None,
            review_mode: ReviewMode::Full,
            min_severity: ReviewSeverity::Warning,
        })
        .await
        .expect("review ok");
        // Note dropped; Warning + Error kept.
        assert_eq!(outcome.findings_count, 2);
    }

    #[tokio::test]
    async fn severity_floor_error_drops_everything_below_error() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/repos/o/r/pulls/7.diff"))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                "diff --git a/x b/x\n@@ -1 +1 @@\n+x\n",
            ))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v1/repos/o/r/pulls/7/files"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {"filename": "x", "status": "modified"}
            ])))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/api/v1/repos/o/r/pulls/7/reviews"))
            .respond_with(
                ResponseTemplate::new(201).set_body_json(serde_json::json!({
                    "id": 1, "state": "COMMENT"
                })),
            )
            .mount(&server)
            .await;

        let forgejo = ForgejoClient::new(&server.uri(), "tok").expect("client");
        let provider = Arc::new(CannedProvider::new(vec![r#"{
            "summary": "minor",
            "findings": [
                {"path":"x","line_start":1,"severity":"note","message":"a"},
                {"path":"x","line_start":1,"severity":"warning","message":"b"}
            ]
        }"#]));
        let llm = router_with(provider);

        let outcome = review_pull_request(ReviewArgs {
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
            custom_pre_merge_checks: &[],
            guidelines: "",
            repo_context: "",
            diff_override: None,
            verify_mode: VerifyMode::Simple,
            workspace_path: None,
            review_mode: ReviewMode::Full,
            min_severity: ReviewSeverity::Error,
        })
        .await
        .expect("review ok");
        // Both findings are below Error → both dropped.
        assert_eq!(outcome.findings_count, 0);
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
            custom_pre_merge_checks: &[],
            guidelines: "",
            repo_context: "",
            diff_override: None,
            verify_mode: VerifyMode::Simple,
            workspace_path: None,
            review_mode: ReviewMode::Full,
            min_severity: ReviewSeverity::Note,
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
            custom_pre_merge_checks: &[],
            guidelines: "",
            repo_context: "",
            diff_override: None,
            verify_mode: VerifyMode::Simple,
            workspace_path: None,
            review_mode: ReviewMode::Full,
            min_severity: ReviewSeverity::Note,
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
            custom_pre_merge_checks: &[],
            guidelines: "",
            repo_context: "",
            diff_override: None,
            verify_mode: VerifyMode::Simple,
            workspace_path: None,
            review_mode: ReviewMode::Full,
            min_severity: ReviewSeverity::Note,
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
            custom_pre_merge_checks: &[],
            guidelines: "",
            repo_context: "",
            diff_override: None,
            verify_mode: VerifyMode::Simple,
            workspace_path: None,
            review_mode: ReviewMode::Full,
            min_severity: ReviewSeverity::Note,
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

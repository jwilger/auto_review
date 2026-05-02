use crate::agentic_verify::verify_findings_agentic;
use crate::config::ReviewMode;
use crate::diff::{cap_diff, DEFAULT_MAX_DIFF_BYTES};
use crate::error::ReviewError;
use crate::heal::{generate_with_self_heal, HealConfig};
use crate::ignored::{filter_changed_files, filter_diff_paths};
use crate::linter_only::build_linter_only_output;
use crate::mapping::output_to_review_request;
use crate::pre_merge::{
    evaluate as evaluate_pre_merge_checks, render_combined_section, CheckResult, CheckStatus,
};
use crate::pre_merge_llm::{evaluate_custom_checks, CustomCheckResult};
use crate::verify::verify_findings;
use ar_forgejo::{Client as ForgejoClient, ReviewEvent};
use ar_llm::Router as LlmRouter;
use ar_prompts::{
    render_review_prompt, system_prompt, PreMergeCustomStatus, ReviewPromptInputs, ReviewSeverity,
};
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
    /// Per-severity breakdown of the findings actually posted.
    /// Sums to `findings_count`. Used to enrich the commit-status
    /// description with "1 error, 3 warnings, 1 note" rather than
    /// just a flat count.
    pub errors: usize,
    pub warnings: usize,
    pub notes: usize,
    /// Findings the verifier corrected away. Reasoning model
    /// emitted N findings; verifier kept (N - verifier_dropped).
    /// Surfaces as a counter so operators can chart their
    /// hallucination rate over time. Always 0 in LinterOnly mode
    /// (no verifier runs).
    pub verifier_dropped: usize,
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

/// Defense-in-depth: drop any finding whose `path` isn't in the
/// list of paths the PR actually touched. The verifier's LLM is
/// supposed to catch hallucinated paths, but when it misses,
/// this deterministic filter prevents the bot from posting an
/// inline comment on a file the PR never touched (Forgejo would
/// reject the comment, but losing the whole review post is worse
/// than dropping one finding).
///
/// Empty `changed_paths` is a soft fail-open: returns 0 without
/// filtering. The orchestrator only feeds an empty slice when the
/// changed-files API call returned nothing, in which case the LLM
/// shouldn't have any findings either; we'd rather post the
/// (likely empty) review than drop everything.
fn drop_findings_outside_changed_paths(
    output: &mut ar_prompts::ReviewOutput,
    changed_paths: &[String],
) -> usize {
    if changed_paths.is_empty() {
        return 0;
    }
    let valid: std::collections::HashSet<&str> = changed_paths.iter().map(|s| s.as_str()).collect();
    let before = output.findings.len();
    output.findings.retain(|f| valid.contains(f.path.as_str()));
    let dropped = before - output.findings.len();
    if dropped > 0 {
        tracing::warn!(
            dropped,
            "dropped findings whose path is not in the PR's changed-files list"
        );
    }
    dropped
}

/// In-place drop of findings strictly below `min`. Logs the
/// kept/dropped counts so operators can confirm the floor is
/// engaging on a per-review basis. Idempotent: a second
/// invocation with the same floor is a no-op.
fn apply_severity_floor(output: &mut ar_prompts::ReviewOutput, min: ReviewSeverity) {
    let before = output.findings.len();
    output
        .findings
        .retain(|f| severity_rank(f.severity) >= severity_rank(min));
    let after = output.findings.len();
    if after != before {
        tracing::info!(
            kept = after,
            dropped = before - after,
            min_severity = ?min,
            "severity floor applied"
        );
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
    pub linter_runs: &'a [LinterRunSummary],
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinterRunSummary {
    pub name: String,
    pub status: LinterRunStatus,
    pub findings: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinterRunStatus {
    Ok,
    Skipped(String),
    Failed(String),
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

    // Track the post-floor / pre-verifier count so we can report
    // how many findings the verifier dropped. Linter-only mode
    // doesn't run a verifier, so it stays at 0.
    let mut pre_verify_count: usize = 0;
    let mut output = match args.review_mode {
        ReviewMode::LinterOnly => {
            // Skip the LLM entirely. The orchestrator already ran the
            // linters; map their findings straight to the review
            // output and continue. No verifier — there's nothing
            // hallucinated to drop.
            build_linter_only_output(args.linter_findings)
        }
        ReviewMode::Full => {
            let mut output =
                generate_with_self_heal(args.llm, system_prompt(), &prompt, HealConfig::default())
                    .await?;
            // Severity-floor filter runs BEFORE the verifier so we
            // don't burn cheap-tier LLM calls verifying findings
            // we'll drop anyway. Operators who set
            // AR_SEVERITY_FLOOR=warning routinely save the
            // verifier-cost on every Note-level finding the
            // reasoning model emits.
            apply_severity_floor(&mut output, args.min_severity);
            pre_verify_count = output.findings.len();
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
    // Snapshot the post-verifier count BEFORE the severity-floor /
    // path-guard passes. `verifier_dropped` reports specifically
    // what the verifier removed, not what later filters did, or the
    // metric drifts every time we add a new post-verifier filter.
    let post_verify_count = output.findings.len();
    // Apply the severity floor again for the LinterOnly path
    // (which doesn't run a verifier, so the pre-verifier
    // application above wouldn't run). Idempotent for Full mode:
    // findings are already at-or-above the floor; this is a no-op.
    apply_severity_floor(&mut output, args.min_severity);

    // Last-mile path guard: the LLM may have emitted a finding
    // citing a path it inferred from RAG context rather than the
    // actual diff. The verifier's job is to catch that, but when
    // it misses, drop the finding here rather than letting Forgejo
    // 422 the entire review payload.
    drop_findings_outside_changed_paths(&mut output, &changed_filenames);

    let findings_count = output.findings.len();

    let mut req = output_to_review_request(&output, args.head_sha);

    // Pre-merge checks: deterministic gates (CHANGELOG / tests /
    // TODOs) plus repo-author-supplied natural-language checks
    // evaluated by the cheap LLM tier. Both surface as a single
    // markdown checklist appended to the review body. Any failed
    // check requests changes even when inline findings are below
    // Error severity.
    let pre_merge_results = evaluate_pre_merge_checks(&diff, &files, args.workspace_path);
    let custom_results = if args.custom_pre_merge_checks.is_empty() {
        Vec::new()
    } else {
        evaluate_custom_checks(args.llm, args.custom_pre_merge_checks, &diff).await
    };
    if pre_merge_has_failure(&pre_merge_results, &custom_results) {
        req.event = ReviewEvent::RequestChanges;
    }
    let section = render_combined_section(&pre_merge_results, &custom_results);
    if !section.is_empty() {
        if !req.body.is_empty() {
            req.body.push_str("\n\n");
        }
        req.body.push_str(&section);
    }
    let linter_section = render_linter_section(args.linter_runs);
    if !linter_section.is_empty() {
        if !req.body.is_empty() {
            req.body.push_str("\n\n");
        }
        req.body.push_str(&linter_section);
    }

    let created = args
        .forgejo
        .create_review(args.owner, args.repo, args.pr_number, &req)
        .await?;

    let mut errors = 0usize;
    let mut warnings = 0usize;
    let mut notes = 0usize;
    for f in &output.findings {
        match f.severity {
            ReviewSeverity::Error => errors += 1,
            ReviewSeverity::Warning => warnings += 1,
            ReviewSeverity::Note => notes += 1,
        }
    }
    let verifier_dropped = pre_verify_count.saturating_sub(post_verify_count);
    Ok(ReviewOutcome {
        findings_count,
        review_id: created.id,
        errors,
        warnings,
        notes,
        verifier_dropped,
    })
}

fn pre_merge_has_failure(built_in: &[CheckResult], custom: &[CustomCheckResult]) -> bool {
    built_in
        .iter()
        .any(|check| matches!(check.status, CheckStatus::Fail))
        || custom
            .iter()
            .any(|check| matches!(check.status, PreMergeCustomStatus::Fail))
}

fn render_linter_section(runs: &[LinterRunSummary]) -> String {
    if runs.is_empty() {
        return String::new();
    }
    let mut out = String::from("<details>\n<summary>Linters</summary>\n\n");
    for run in runs {
        let status = match &run.status {
            LinterRunStatus::Ok => format!(
                "ok, {} finding{}",
                run.findings,
                if run.findings == 1 { "" } else { "s" }
            ),
            LinterRunStatus::Skipped(reason) => format!(
                "skipped: {reason}, {} finding{}",
                run.findings,
                if run.findings == 1 { "" } else { "s" }
            ),
            LinterRunStatus::Failed(reason) => format!(
                "failed: {}, {} finding{}",
                cap_linter_reason(reason),
                run.findings,
                if run.findings == 1 { "" } else { "s" }
            ),
        };
        out.push_str(&format!("- {} — {status}\n", run.name));
    }
    out.push_str("\n</details>");
    out
}

fn cap_linter_reason(reason: &str) -> String {
    const MAX_BYTES: usize = 200;
    let normalized = reason.replace(['\r', '\n'], " ");
    if normalized.len() <= MAX_BYTES {
        return normalized;
    }
    let mut cut = MAX_BYTES;
    while cut > 0 && !normalized.is_char_boundary(cut) {
        cut -= 1;
    }
    format!("{}… [truncated]", &normalized[..cut])
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
    use wiremock::matchers::{body_partial_json, body_string_contains, method, path};
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

    fn finding(path: &str) -> ar_prompts::ReviewFinding {
        ar_prompts::ReviewFinding {
            path: path.into(),
            line_start: 1,
            line_end: None,
            severity: ReviewSeverity::Warning,
            message: "msg".into(),
        }
    }

    fn output_with_paths(paths: &[&str]) -> ar_prompts::ReviewOutput {
        ar_prompts::ReviewOutput {
            summary: String::new(),
            walkthrough: String::new(),
            mermaid: String::new(),
            findings: paths.iter().map(|p| finding(p)).collect(),
        }
    }

    #[test]
    fn drop_findings_outside_changed_paths_keeps_matching_paths() {
        let mut out = output_with_paths(&["src/a.rs", "src/b.rs"]);
        let changed = vec!["src/a.rs".to_string(), "src/b.rs".to_string()];
        let dropped = drop_findings_outside_changed_paths(&mut out, &changed);
        assert_eq!(dropped, 0);
        assert_eq!(out.findings.len(), 2);
    }

    #[test]
    fn drop_findings_outside_changed_paths_drops_hallucinated_paths() {
        let mut out = output_with_paths(&["src/a.rs", "src/hallucinated.rs", "src/b.rs"]);
        let changed = vec!["src/a.rs".to_string(), "src/b.rs".to_string()];
        let dropped = drop_findings_outside_changed_paths(&mut out, &changed);
        assert_eq!(dropped, 1);
        let kept: Vec<&str> = out.findings.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(kept, vec!["src/a.rs", "src/b.rs"]);
    }

    #[test]
    fn drop_findings_outside_changed_paths_fails_open_when_changed_list_empty() {
        // If the changed-files API returned nothing, don't drop
        // anything — the LLM probably has nothing to flag either,
        // and we'd rather post the (likely empty) review than nuke
        // legitimate findings on a transient API misread.
        let mut out = output_with_paths(&["src/a.rs"]);
        let dropped = drop_findings_outside_changed_paths(&mut out, &[]);
        assert_eq!(dropped, 0);
        assert_eq!(out.findings.len(), 1);
    }

    #[test]
    fn drop_findings_outside_changed_paths_is_case_sensitive() {
        // Forgejo paths are case-sensitive (POSIX-y). Treating
        // them as such avoids false positives on case-insensitive
        // filesystems and is consistent with how the LLM sees
        // them in the prompt.
        let mut out = output_with_paths(&["src/Foo.rs"]);
        let changed = vec!["src/foo.rs".to_string()];
        let dropped = drop_findings_outside_changed_paths(&mut out, &changed);
        assert_eq!(dropped, 1);
        assert!(out.findings.is_empty());
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
            .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({
                "id": 1, "state": "COMMENT"
            })))
            .mount(&server)
            .await;

        let forgejo = ForgejoClient::new(&server.uri(), "tok").expect("client");
        // The reasoning model emits three findings spanning every
        // severity. Floor should drop the Note one.
        let provider = Arc::new(CannedProvider::new(vec![
            r#"{
            "summary": "mixed",
            "findings": [
                {"path":"x","line_start":1,"severity":"note","message":"style"},
                {"path":"x","line_start":2,"severity":"warning","message":"bad"},
                {"path":"x","line_start":2,"severity":"error","message":"unsafe"}
            ]
        }"#,
        ]));
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
            linter_runs: &[],
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
    async fn severity_floor_runs_before_verifier_to_save_cheap_tier_calls() {
        // The reasoning model emits 3 findings (Note + Warning +
        // Error). The cheap-tier verifier should ONLY see the 2
        // above the Warning floor — that's the cost-saving claim.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/repos/o/r/pulls/7.diff"))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                "diff --git a/x b/x\n@@ -1,3 +1,3 @@\n-old1\n+new1\n-old2\n+new2\n-old3\n+new3\n",
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
            .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({
                "id": 1, "state": "COMMENT"
            })))
            .mount(&server)
            .await;

        let forgejo = ForgejoClient::new(&server.uri(), "tok").expect("client");
        let reasoning = Arc::new(CannedProvider::new(vec![
            r#"{
            "summary":"mixed",
            "findings": [
                {"path":"x","line_start":1,"severity":"note","message":"style"},
                {"path":"x","line_start":2,"severity":"warning","message":"bad"},
                {"path":"x","line_start":3,"severity":"error","message":"unsafe"}
            ]
        }"#,
        ]));
        // Cheap-tier verifier records what it's asked to verify.
        // It returns "keep" for everything (so post-verifier count
        // = post-floor count). The assertion is that it received
        // only 2 findings, not 3.
        let cheap = Arc::new(CannedProvider::new(vec![
            r#"{
            "verdicts": [
                {"finding_index":0,"keep":true,"reasoning":""},
                {"finding_index":1,"keep":true,"reasoning":""}
            ]
        }"#,
        ]));
        let llm = Router::new()
            .with(ModelTier::Reasoning, reasoning)
            .with(ModelTier::Cheap, cheap.clone());

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
            linter_runs: &[],
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
        // Floor dropped Note. Verifier kept the 2 remaining.
        assert_eq!(outcome.findings_count, 2);
        // The verifier prompt should mention only 2 findings,
        // never 3. Spot-check by confirming the user prompt
        // doesn't mention "style" (the Note message).
        let verifier_prompt = cheap.last_user_prompt().expect("verifier called");
        assert!(
            !verifier_prompt.contains("style"),
            "verifier saw the Note finding 'style' — floor didn't run before verifier. \
             Prompt was:\n{verifier_prompt}",
        );
        assert!(
            verifier_prompt.contains("bad") && verifier_prompt.contains("unsafe"),
            "verifier should see the kept Warning + Error findings",
        );
    }

    #[tokio::test]
    async fn severity_floor_error_drops_everything_below_error() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/repos/o/r/pulls/7.diff"))
            .respond_with(
                ResponseTemplate::new(200).set_body_string("diff --git a/x b/x\n@@ -1 +1 @@\n+x\n"),
            )
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
            .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({
                "id": 1, "state": "COMMENT"
            })))
            .mount(&server)
            .await;

        let forgejo = ForgejoClient::new(&server.uri(), "tok").expect("client");
        let provider = Arc::new(CannedProvider::new(vec![
            r#"{
            "summary": "minor",
            "findings": [
                {"path":"x","line_start":1,"severity":"note","message":"a"},
                {"path":"x","line_start":1,"severity":"warning","message":"b"}
            ]
        }"#,
        ]));
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
            linter_runs: &[],
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
        // pipeline appends the deterministic check section. This
        // source-only fixture has no test change, so the pre-merge
        // checklist asks for changes. The body field stays excluded
        // from the matcher so this test focuses on the integration
        // contract (event + commit_id) — body composition is covered
        // by mapping/pre_merge tests.
        Mock::given(method("POST"))
            .and(path("/api/v1/repos/o/r/pulls/7/reviews"))
            .and(body_partial_json(serde_json::json!({
                "commit_id": "deadbeef",
                "event": "REQUEST_CHANGES"
            })))
            .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({
                "id": 1234,
                "state": "REQUEST_CHANGES"
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
            linter_runs: &[],
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
            linter_runs: &[],
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
            linter_runs: &[],
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
    async fn review_pull_request_request_changes_when_pre_merge_check_fails() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/repos/o/r/pulls/7.diff"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string("diff --git a/src/x.rs b/src/x.rs\n@@ -1 +1 @@\n-old\n+new\n"),
            )
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
                "event": "REQUEST_CHANGES"
            })))
            .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({
                "id": 100,
                "state": "REQUEST_CHANGES"
            })))
            .mount(&server)
            .await;

        let forgejo = ForgejoClient::new(&server.uri(), "tok").expect("client");
        let provider = Arc::new(CannedProvider::new(vec![
            r#"{"summary":"no inline findings","findings":[]}"#,
        ]));
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
            linter_runs: &[],
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
        .expect("pre-merge failure should still post review");

        assert_eq!(outcome.review_id, 100);
        assert_eq!(outcome.findings_count, 0);
    }

    #[test]
    fn pre_merge_has_failure_includes_custom_checks() {
        let custom = vec![CustomCheckResult {
            check: "PR title explains why".into(),
            status: PreMergeCustomStatus::Fail,
            rationale: "title is too vague".into(),
        }];

        assert!(pre_merge_has_failure(&[], &custom));
    }

    #[tokio::test]
    async fn review_body_includes_linter_summary_section() {
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
            .and(body_string_contains("<details>"))
            .and(body_string_contains("<summary>Linters</summary>"))
            .and(body_string_contains("ruff — ok, 0 findings"))
            .and(body_string_contains("shellcheck — ok, 2 findings"))
            .and(body_string_contains(
                "eslint — skipped: disabled by repo config, 0 findings",
            ))
            .and(body_string_contains(
                "markdownlint — failed: parse failed, 0 findings",
            ))
            .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({
                "id": 101,
                "state": "COMMENT"
            })))
            .mount(&server)
            .await;

        let forgejo = ForgejoClient::new(&server.uri(), "tok").expect("client");
        let provider = Arc::new(CannedProvider::new(vec![
            r#"{"summary":"lint summary","findings":[]}"#,
        ]));
        let llm = router_with(provider);
        let linter_runs = vec![
            LinterRunSummary {
                name: "ruff".into(),
                status: LinterRunStatus::Ok,
                findings: 0,
            },
            LinterRunSummary {
                name: "shellcheck".into(),
                status: LinterRunStatus::Ok,
                findings: 2,
            },
            LinterRunSummary {
                name: "eslint".into(),
                status: LinterRunStatus::Skipped("disabled by repo config".into()),
                findings: 0,
            },
            LinterRunSummary {
                name: "markdownlint".into(),
                status: LinterRunStatus::Failed("parse failed".into()),
                findings: 0,
            },
        ];

        review_pull_request(ReviewArgs {
            forgejo: &forgejo,
            llm: &llm,
            owner: "o",
            repo: "r",
            pr_number: 7,
            head_sha: "sha",
            pr_title: "t",
            pr_body: "b",
            linter_findings: &[],
            linter_runs: &linter_runs,
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
        .expect("review should include linter section");
    }

    #[test]
    fn failed_linter_reason_is_single_line_and_capped() {
        let noisy_reason = format!("first line\n{}\nlast line", "x".repeat(260));
        let section = render_linter_section(&[LinterRunSummary {
            name: "markdownlint".into(),
            status: LinterRunStatus::Failed(noisy_reason),
            findings: 0,
        }]);

        let entry = section
            .lines()
            .find(|line| line.starts_with("- markdownlint"))
            .expect("linter entry rendered");
        assert!(!entry.contains('\n'));
        assert!(entry.contains("0 findings"));
        assert!(entry.contains("[truncated]"));
        assert!(!entry.contains("last line"));
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
            linter_runs: &[],
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

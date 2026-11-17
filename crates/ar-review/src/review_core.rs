//! Host-free review core.
//!
//! Runs the generate -> severity floor -> verify -> severity floor ->
//! changed-path guard sequence over an already-fetched diff without a
//! repository host. [`crate::pipeline::review_pull_request`] delegates to
//! it after fetching the diff and prior discussion; the bench and local
//! review surfaces can call it directly.

use crate::agentic_verify::verify_findings_agentic;
use crate::error::ReviewError;
use crate::heal::{generate_with_self_heal, HealConfig};
use crate::pipeline::{apply_severity_floor, drop_findings_outside_changed_paths, VerifyMode};
use crate::verify::verify_findings;
use ar_llm::{ModelTier, Router as LlmRouter};
use ar_prompts::{render_review_prompt, system_prompt, ReviewPromptInputs, ReviewSeverity};
use std::path::Path;

/// One LLM call's usage: tier, provider base URL, model, input tokens,
/// output tokens.
pub type ReviewUsageEntry = (ModelTier, String, String, u32, u32);

/// All inputs to [`run_review_core`]. The diff is already filtered and
/// capped and the prior discussion is already rendered; the core does
/// no host I/O.
pub struct ReviewCoreArgs<'a> {
    pub llm: &'a LlmRouter,
    /// Repository label rendered into the prompt (e.g. `owner/repo`).
    pub repo_full_name: &'a str,
    pub pr_number: u64,
    pub pr_title: &'a str,
    pub pr_body: &'a str,
    /// Unified diff, already filtered for ignored paths and capped.
    pub diff: &'a str,
    /// Paths the change touches; findings outside them are dropped.
    pub changed_files: &'a [String],
    pub guidelines: &'a str,
    pub repo_context: &'a str,
    pub previous_review_sha: Option<&'a str>,
    /// Prior PR discussion, already rendered for the prompt.
    pub prior_discussion: &'a str,
    /// Per-run nonce for the untrusted-data fences in the review prompt;
    /// the caller shares it with any other prompt it fences in the same run.
    pub fence_nonce: &'a str,
    /// Verifier strategy. `Agentic` without `workspace_path` falls back
    /// to `Simple`.
    pub verify_mode: VerifyMode,
    pub workspace_path: Option<&'a Path>,
    /// Drop findings below this severity.
    pub min_severity: ReviewSeverity,
}

/// Result of [`run_review_core`].
#[derive(Debug, Clone)]
pub struct ReviewDraft {
    /// Findings after both severity-floor passes and the path guard.
    pub output: ar_prompts::ReviewOutput,
    /// Findings after the first severity floor, before the verifier.
    pub pre_verify_count: usize,
    /// Findings straight after the verifier, before the second floor
    /// and the path guard.
    pub post_verify_count: usize,
    /// `pre_verify_count - post_verify_count`, saturating.
    pub verifier_dropped: usize,
    /// One entry per LLM call the core made.
    pub usage: Vec<ReviewUsageEntry>,
}

type UsageLog = std::sync::Arc<std::sync::Mutex<Vec<ReviewUsageEntry>>>;

/// Run the host-free review stages over an already-fetched diff.
pub async fn run_review_core(args: ReviewCoreArgs<'_>) -> Result<ReviewDraft, ReviewError> {
    let usage: UsageLog = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let llm = {
        let usage_capture = usage.clone();
        args.llm.clone().with_usage_collector(
            move |tier, base_url, model, input_tokens, output_tokens| {
                if let Ok(mut usage) = usage_capture.lock() {
                    usage.push((
                        tier,
                        base_url.to_string(),
                        model.to_string(),
                        input_tokens,
                        output_tokens,
                    ));
                }
            },
        )
    };
    let diff = args.diff;

    let prompt = render_review_prompt(&ReviewPromptInputs {
        repo_full_name: args.repo_full_name,
        pr_number: args.pr_number,
        pr_title: args.pr_title,
        pr_body: args.pr_body,
        diff,
        changed_files: args.changed_files,
        guidelines: args.guidelines,
        repo_context: args.repo_context,
        previous_review_sha: args.previous_review_sha,
        prior_discussion: args.prior_discussion,
        fence_nonce: args.fence_nonce,
    });

    // Track the post-floor / pre-verifier count so we can report how many
    // findings the verifier dropped.
    let mut output =
        generate_with_self_heal(&llm, system_prompt(), &prompt, HealConfig::default()).await?;
    apply_severity_floor(&mut output, args.min_severity);
    let pre_verify_count = output.findings.len();
    output = match (args.verify_mode, args.workspace_path) {
        (VerifyMode::Agentic, Some(workspace)) => {
            verify_findings_agentic(&llm, output, workspace, diff).await?
        }
        _ => verify_findings(&llm, output, diff).await?,
    };
    // Snapshot the post-verifier count BEFORE the severity-floor /
    // path-guard passes. `verifier_dropped` reports specifically
    // what the verifier removed, not what later filters did, or the
    // metric drifts every time we add a new post-verifier filter.
    let post_verify_count = output.findings.len();
    // Idempotent second pass after verification in case verifier rewrites
    // severity in future implementations.
    apply_severity_floor(&mut output, args.min_severity);

    // Last-mile path guard: the LLM may have emitted a finding
    // citing a path it inferred from RAG context rather than the
    // actual diff. The verifier's job is to catch that, but when
    // it misses, drop the finding here rather than letting Forgejo
    // 422 the entire review payload.
    drop_findings_outside_changed_paths(&mut output, args.changed_files);

    let usage = usage.lock().map(|u| u.clone()).unwrap_or_default();
    Ok(ReviewDraft {
        output,
        pre_verify_count,
        post_verify_count,
        verifier_dropped: pre_verify_count.saturating_sub(post_verify_count),
        usage,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use ar_llm::{CompleteRequest, CompleteResponse, Error as LlmError, LlmProvider};
    use async_trait::async_trait;
    use std::sync::{Arc, Mutex};

    /// Returns canned contents in call order.
    struct CannedProvider {
        responses: Mutex<Vec<String>>,
    }

    impl CannedProvider {
        fn new(responses: &[&str]) -> Arc<Self> {
            Arc::new(Self {
                responses: Mutex::new(responses.iter().rev().map(|s| s.to_string()).collect()),
            })
        }
    }

    #[async_trait]
    impl LlmProvider for CannedProvider {
        async fn complete(&self, _req: CompleteRequest) -> Result<CompleteResponse, LlmError> {
            let content = self.responses.lock().unwrap().pop().unwrap_or_default();
            Ok(CompleteResponse {
                content,
                input_tokens: 10,
                output_tokens: 5,
            })
        }
    }

    #[tokio::test]
    async fn applies_severity_floor_and_path_guard_without_a_host() {
        let generation = r#"{
            "summary": "s",
            "findings": [
                {"path": "src/a.rs", "line_start": 1, "severity": "note", "message": "nit"},
                {"path": "src/a.rs", "line_start": 2, "severity": "warning", "message": "real"},
                {"path": "src/not_in_diff.rs", "line_start": 3, "severity": "warning", "message": "hallucinated"}
            ]
        }"#;
        let verification =
            r#"{"verdicts":[{"finding_index":0,"keep":true},{"finding_index":1,"keep":true}]}"#;
        let router = LlmRouter::new()
            .with(ModelTier::Reasoning, CannedProvider::new(&[generation]))
            .with(ModelTier::Cheap, CannedProvider::new(&[verification]));
        let changed_files = vec!["src/a.rs".to_string()];

        let draft = run_review_core(ReviewCoreArgs {
            llm: &router,
            repo_full_name: "owner/repo",
            pr_number: 1,
            pr_title: "title",
            pr_body: "body",
            diff: "diff --git a/src/a.rs b/src/a.rs\n@@ -1 +1 @@\n-old\n+new\n",
            changed_files: &changed_files,
            guidelines: "",
            repo_context: "",
            previous_review_sha: None,
            prior_discussion: "",
            fence_nonce: "0123456789abcdef",
            verify_mode: VerifyMode::Simple,
            workspace_path: None,
            min_severity: ReviewSeverity::Warning,
        })
        .await
        .unwrap();

        assert_eq!(draft.output.findings.len(), 1);
        assert_eq!(draft.output.findings[0].path, "src/a.rs");
        assert_eq!(draft.output.findings[0].severity, ReviewSeverity::Warning);
        assert_eq!(draft.pre_verify_count, 2);
        assert_eq!(draft.post_verify_count, 2);
        assert_eq!(draft.verifier_dropped, 0);
        assert!(!draft.usage.is_empty());
    }
}

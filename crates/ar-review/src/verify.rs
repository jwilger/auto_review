//! Second-pass verification of LLM review findings.
//!
//! Calls the cheap-tier model with the diff + the candidate findings
//! and asks for per-finding "keep / drop" verdicts. Drops any finding
//! the verifier doesn't corroborate. Falls open (returns the input
//! unchanged) when the cheap tier isn't configured or the verifier's
//! response is malformed — verifier failures shouldn't drop real
//! findings.

use crate::diff::{cap_for_prompt, CHEAP_TIER_DIFF_CAP};
use crate::error::ReviewError;
use ar_llm::{CompleteRequest, Message, ModelTier, ResponseFormat, Router};
use ar_prompts::{
    validate_verification_output, verification_schema, verification_system_prompt, ReviewOutput,
};
use core::fmt::Write as _;
use std::collections::HashSet;

/// Run the verifier over `output.findings` against the diff that
/// produced them. Returns a new `ReviewOutput` with only the findings
/// the verifier corroborated.
///
/// Behavior when the verifier can't run (no Cheap tier configured,
/// LLM error, malformed JSON): returns the input unchanged. Verifier
/// failures must not silently drop real findings.
///
/// # Errors
///
/// Currently never returns an error: every verifier failure falls open
/// to the unchanged input. The `Result` keeps room for hard failures.
#[inline]
#[expect(
    clippy::module_name_repetitions,
    reason = "ar-cli (bench) calls this as ar_review::verify_findings; renaming it breaks that \
              crate"
)]
pub async fn verify_findings(
    router: &Router,
    output: ReviewOutput,
    diff: &str,
) -> Result<ReviewOutput, ReviewError> {
    if router.provider(ModelTier::Cheap).is_err() || output.findings.is_empty() {
        return Ok(output);
    }

    let prompt = render_user_prompt(&output, diff);
    let req = CompleteRequest {
        system: Some(verification_system_prompt().to_owned()),
        messages: vec![Message::user(prompt)],
        response_format: Some(ResponseFormat::JsonSchema {
            name: "Verification".to_owned(),
            schema: verification_schema().clone(),
        }),
        // Determinism: same finding + diff → same verdict.
        temperature: Some(0.0),
        ..Default::default()
    };

    let resp = match router.complete(ModelTier::Cheap, req).await {
        Ok(response) => response,
        Err(err) => {
            tracing::warn!(error = %err, "verifier LLM call failed; keeping all findings");
            return Ok(output);
        }
    };
    let verdicts = match validate_verification_output(&resp.content) {
        Ok(parsed) => parsed,
        Err(err) => {
            tracing::warn!(error = %err, "verifier output failed validation; keeping all findings");
            return Ok(output);
        }
    };

    let dropped_indices: HashSet<usize> = verdicts
        .verdicts
        .iter()
        .filter(|verdict| !verdict.keep)
        .map(|verdict| verdict.finding_index)
        .collect();

    let kept: Vec<_> = output
        .findings
        .into_iter()
        .enumerate()
        .filter_map(|(index, finding)| {
            if dropped_indices.contains(&index) {
                None
            } else {
                Some(finding)
            }
        })
        .collect();

    let dropped = dropped_indices.len();
    if dropped > 0 {
        tracing::info!(
            dropped,
            kept = kept.len(),
            "verifier dropped suspect findings"
        );
    }

    Ok(ReviewOutput {
        findings: kept,
        ..output
    })
}

/// Render the verifier's user prompt: numbered findings, then the capped diff.
fn render_user_prompt(output: &ReviewOutput, diff: &str) -> String {
    let capped = cap_for_prompt(diff, CHEAP_TIER_DIFF_CAP, "[diff truncated for verifier]");
    let mut out = String::with_capacity(capped.len().saturating_add(1024));
    out.push_str("Findings to verify:\n");
    for (index, finding) in output.findings.iter().enumerate() {
        #[expect(
            clippy::let_underscore_must_use,
            reason = "fmt::Write for String never returns Err, so there is no error to handle"
        )]
        #[expect(
            clippy::use_debug,
            reason = "the prompt names each severity by its variant name (Error, Warning, ...), \
                      which is what the Debug format prints"
        )]
        let _ = writeln!(
            out,
            "{index}. [{:?}] {}:{} — {}",
            finding.severity, finding.path, finding.line_start, finding.message
        );
    }
    out.push_str("\nUnified diff:\n```diff\n");
    out.push_str(&capped);
    if !capped.ends_with('\n') {
        out.push('\n');
    }
    out.push_str("```\n\nFor each finding above, decide whether the diff corroborates it.\n");
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::sync::Arc;
    use ar_llm::{CompleteResponse, Error as LlmError, LlmProvider};
    use ar_prompts::{ReviewFinding, ReviewSeverity};
    use async_trait::async_trait;
    use std::sync::Mutex;

    struct CannedProvider(Mutex<Option<String>>);

    #[async_trait]
    impl LlmProvider for CannedProvider {
        #[expect(
            clippy::unwrap_used,
            reason = "a failed setup, fixture or tool call should fail this test at that line; unwrap reports the error"
        )]
        async fn complete(&self, _req: CompleteRequest) -> Result<CompleteResponse, LlmError> {
            Ok(CompleteResponse {
                content: self.0.lock().unwrap().take().unwrap_or_default(),
                input_tokens: 0,
                output_tokens: 0,
            })
        }
    }

    fn finding(i: u32) -> ReviewFinding {
        ReviewFinding {
            path: format!("f{i}.rs"),
            line_start: i,
            line_end: None,
            severity: ReviewSeverity::Warning,
            message: format!("issue {i}"),
            title: String::new(),
            category: ar_prompts::FindingCategory::Correctness,
        }
    }

    fn out(n: u32) -> ReviewOutput {
        ReviewOutput {
            summary: "s".into(),
            walkthrough: String::new(),
            mermaid: String::new(),
            findings: (0..n).map(finding).collect(),
        }
    }

    #[tokio::test]
    #[expect(
        clippy::expect_used,
        reason = "a failed setup, fixture or tool call should fail this test at that line with the labelled message"
    )]
    async fn returns_input_unchanged_when_cheap_tier_missing() {
        let router = Router::new();
        let result = verify_findings(&router, out(2), "diff").await.expect("ok");
        assert_eq!(result.findings.len(), 2);
    }

    #[tokio::test]
    #[expect(
        clippy::assert_is_empty,
        reason = "the emptiness assertion stays as written; this lint-only change leaves assertions unedited"
    )]
    #[expect(
        clippy::expect_used,
        reason = "a failed setup, fixture or tool call should fail this test at that line with the labelled message"
    )]
    async fn returns_input_unchanged_when_findings_empty() {
        let provider = Arc::new(CannedProvider(Mutex::new(Some("ignored".into()))));
        let router = Router::new().with(ModelTier::Cheap, provider);
        let result = verify_findings(&router, out(0), "diff").await.expect("ok");
        assert!(result.findings.is_empty());
    }

    #[tokio::test]
    #[expect(
        clippy::expect_used,
        reason = "a failed setup, fixture or tool call should fail this test at that line with the labelled message"
    )]
    #[expect(
        clippy::indexing_slicing,
        reason = "indexes results whose length this test sets up or asserts; an out-of-range index fails the test, as it should"
    )]
    async fn drops_findings_the_verifier_says_to_drop() {
        // Mark index 0 as keep=false, index 1 as keep=true.
        let json = r#"{
            "verdicts": [
                {"finding_index": 0, "keep": false},
                {"finding_index": 1, "keep": true}
            ]
        }"#;
        let provider = Arc::new(CannedProvider(Mutex::new(Some(json.into()))));
        let router = Router::new().with(ModelTier::Cheap, provider);
        let result = verify_findings(&router, out(2), "diff").await.expect("ok");
        assert_eq!(result.findings.len(), 1);
        assert_eq!(result.findings[0].path, "f1.rs");
    }

    #[tokio::test]
    #[expect(
        clippy::expect_used,
        reason = "a failed setup, fixture or tool call should fail this test at that line with the labelled message"
    )]
    async fn keeps_all_findings_on_malformed_verifier_output() {
        let provider = Arc::new(CannedProvider(Mutex::new(Some("not json".into()))));
        let router = Router::new().with(ModelTier::Cheap, provider);
        let result = verify_findings(&router, out(2), "diff").await.expect("ok");
        // Fail-open: real findings preserved.
        assert_eq!(result.findings.len(), 2);
    }

    #[tokio::test]
    #[expect(
        clippy::expect_used,
        reason = "a failed setup, fixture or tool call should fail this test at that line with the labelled message"
    )]
    async fn missing_finding_indices_are_kept_implicitly() {
        // The verifier doesn't mention finding 1 — it gets kept.
        let json = r#"{"verdicts":[{"finding_index": 0, "keep": false}]}"#;
        let provider = Arc::new(CannedProvider(Mutex::new(Some(json.into()))));
        let router = Router::new().with(ModelTier::Cheap, provider);
        let result = verify_findings(&router, out(3), "diff").await.expect("ok");
        // Finding 0 dropped; 1 and 2 kept.
        assert_eq!(result.findings.len(), 2);
        let names: Vec<&str> = result
            .findings
            .iter()
            .map(|finding| finding.path.as_str())
            .collect();
        assert_eq!(names, vec!["f1.rs", "f2.rs"]);
    }

    #[tokio::test]
    #[expect(
        clippy::expect_used,
        reason = "a failed setup, fixture or tool call should fail this test at that line with the labelled message"
    )]
    #[expect(
        clippy::indexing_slicing,
        reason = "indexes results whose length this test sets up or asserts; an out-of-range index fails the test, as it should"
    )]
    async fn kept_findings_keep_their_title_and_category() {
        let json = r#"{"verdicts":[{"finding_index": 0, "keep": false},{"finding_index": 1, "keep": true}]}"#;
        let provider = Arc::new(CannedProvider(Mutex::new(Some(json.into()))));
        let router = Router::new().with(ModelTier::Cheap, provider);
        let mut input = out(2);
        input.findings[1].title = "Slow loop".into();
        input.findings[1].category = ar_prompts::FindingCategory::Performance;
        let expected = input.findings[1].clone();
        let result = verify_findings(&router, input, "diff").await.expect("ok");
        assert_eq!(result.findings, vec![expected]);
    }

    #[tokio::test]
    #[expect(
        clippy::expect_used,
        reason = "a failed setup, fixture or tool call should fail this test at that line with the labelled message"
    )]
    async fn preserves_summary_walkthrough_and_mermaid_unchanged() {
        let json = r#"{"verdicts":[{"finding_index": 0, "keep": true}]}"#;
        let provider = Arc::new(CannedProvider(Mutex::new(Some(json.into()))));
        let router = Router::new().with(ModelTier::Cheap, provider);
        let mut input = out(1);
        input.summary = "important summary".into();
        input.walkthrough = "walkthrough text".into();
        input.mermaid = "graph TD".into();
        let result = verify_findings(&router, input, "diff").await.expect("ok");
        assert_eq!(result.summary, "important summary");
        assert_eq!(result.walkthrough, "walkthrough text");
        assert_eq!(result.mermaid, "graph TD");
    }
}

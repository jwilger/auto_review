//! LLM-driven evaluation of repo-author-supplied custom pre-merge
//! checks (the second half of the Milestone 4 pre-merge spec).
//!
//! Repo authors put free-form English checks under
//! `pre_merge_checks:` in `.auto_review.yaml`; this module renders
//! the diff + checks into a single cheap-tier prompt, validates the
//! schema-conformant JSON response, and returns one
//! [`CustomCheckResult`] per supplied check.
//!
//! Failure mode: any LLM, schema, or length-mismatch error returns
//! `Ok(Vec::new())`. Custom checks are advisory and best-effort —
//! the review still posts.

use ar_llm::{
    CompleteRequest, ModelTier, ResponseFormat, Role, Router as LlmRouter,
};
use ar_prompts::{
    pre_merge_custom_schema, pre_merge_custom_system_prompt,
    validate_pre_merge_custom_output, PreMergeCustomStatus,
};

#[derive(Debug, Clone)]
pub struct CustomCheckResult {
    /// Verbatim from `.auto_review.yaml`, surfaced back so the PR
    /// author sees which rule fired.
    pub check: String,
    pub status: PreMergeCustomStatus,
    pub rationale: String,
}

/// Evaluate every custom check against the diff. Returns
/// `Vec::new()` when the cheap tier is unconfigured, when no
/// checks are supplied, or when the LLM call / schema validation
/// fails — custom checks are advisory.
pub async fn evaluate_custom_checks(
    llm: &LlmRouter,
    checks: &[String],
    diff: &str,
) -> Vec<CustomCheckResult> {
    if checks.is_empty() {
        return Vec::new();
    }
    let provider = match llm.provider(ModelTier::Cheap) {
        Ok(p) => p.clone(),
        Err(_) => {
            tracing::debug!(
                count = checks.len(),
                "custom pre-merge checks supplied but no Cheap tier configured; skipping"
            );
            return Vec::new();
        }
    };
    let user_prompt = render_user_prompt(checks, diff);
    let req = CompleteRequest {
        system: Some(pre_merge_custom_system_prompt().to_string()),
        messages: vec![ar_llm::Message {
            role: Role::User,
            content: user_prompt,
        }],
        max_tokens: Some(2048),
        temperature: Some(0.0),
        response_format: Some(ResponseFormat::JsonSchema {
            name: "pre_merge_custom".into(),
            schema: pre_merge_custom_schema().clone(),
        }),
    };
    let resp = match provider.complete(req).await {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(error = %e, "custom pre-merge LLM call failed; skipping");
            return Vec::new();
        }
    };
    let parsed = match validate_pre_merge_custom_output(&resp.content, checks.len()) {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!(error = %e, "custom pre-merge LLM output failed validation; skipping");
            return Vec::new();
        }
    };
    parsed
        .checks
        .into_iter()
        .zip(checks.iter())
        .map(|(result, check)| CustomCheckResult {
            check: check.clone(),
            status: result.status,
            rationale: result.rationale,
        })
        .collect()
}

/// Compose the user-side prompt: the diff, then a numbered list of
/// the custom checks. Numbering keeps the LLM aligned with input
/// order (the schema requires same-order results).
fn render_user_prompt(checks: &[String], diff: &str) -> String {
    let mut s = String::with_capacity(diff.len() + 256);
    s.push_str("Pull-request diff:\n\n```diff\n");
    s.push_str(diff);
    if !diff.ends_with('\n') {
        s.push('\n');
    }
    s.push_str("```\n\nCustom pre-merge checks (one result per check, same order):\n");
    for (i, c) in checks.iter().enumerate() {
        s.push_str(&format!("{}. {}\n", i + 1, c));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use ar_llm::{CompleteResponse, Error as LlmError, LlmProvider};
    use async_trait::async_trait;
    use std::sync::{Arc, Mutex};

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
            self.seen
                .lock()
                .unwrap()
                .first()
                .and_then(|req| {
                    req.messages
                        .iter()
                        .find(|m| matches!(m.role, Role::User))
                        .map(|m| m.content.clone())
                })
        }
    }
    #[async_trait]
    impl LlmProvider for CannedProvider {
        async fn complete(
            &self,
            req: CompleteRequest,
        ) -> Result<CompleteResponse, LlmError> {
            self.seen.lock().unwrap().push(req);
            let next = self
                .responses
                .lock()
                .unwrap()
                .pop()
                .unwrap_or_else(|| "{}".into());
            Ok(CompleteResponse {
                content: next,
                input_tokens: 0,
                output_tokens: 0,
            })
        }
    }

    fn router_with(p: Arc<CannedProvider>) -> LlmRouter {
        LlmRouter::new().with(ModelTier::Cheap, p)
    }

    #[tokio::test]
    async fn empty_checks_returns_empty_results() {
        let provider = Arc::new(CannedProvider::new(vec![]));
        let llm = router_with(provider.clone());
        let out = evaluate_custom_checks(&llm, &[], "diff").await;
        assert!(out.is_empty());
    }

    #[tokio::test]
    async fn no_cheap_tier_returns_empty_results() {
        let llm = LlmRouter::new();
        let out = evaluate_custom_checks(
            &llm,
            &["always use Result".to_string()],
            "+let x = 1;\n",
        )
        .await;
        assert!(out.is_empty());
    }

    #[tokio::test]
    async fn happy_path_returns_one_result_per_check_in_order() {
        let provider = Arc::new(CannedProvider::new(vec![
            r#"{"checks":[
                {"status":"pass","rationale":"every public fn has /// above it"},
                {"status":"fail","rationale":"sqlx::query! added at src/db.rs:42"}
            ]}"#,
        ]));
        let llm = router_with(provider.clone());
        let checks = vec![
            "All new public APIs have rustdoc".to_string(),
            "No raw SQL".to_string(),
        ];
        let out = evaluate_custom_checks(&llm, &checks, "@@ +1 @@").await;
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].check, "All new public APIs have rustdoc");
        assert_eq!(out[0].status, PreMergeCustomStatus::Pass);
        assert_eq!(out[1].check, "No raw SQL");
        assert_eq!(out[1].status, PreMergeCustomStatus::Fail);

        // The user prompt carries the diff and a numbered list.
        let prompt = provider.last_user_prompt().expect("captured");
        assert!(prompt.contains("```diff"));
        assert!(prompt.contains("1. All new public APIs have rustdoc"));
        assert!(prompt.contains("2. No raw SQL"));
    }

    #[tokio::test]
    async fn schema_violation_returns_empty_results() {
        let provider = Arc::new(CannedProvider::new(vec![r#"{"checks":[{"status":"maybe"}]}"#]));
        let llm = router_with(provider);
        let checks = vec!["x".to_string()];
        let out = evaluate_custom_checks(&llm, &checks, "diff").await;
        assert!(out.is_empty(), "schema violation must degrade gracefully");
    }

    #[tokio::test]
    async fn length_mismatch_returns_empty_results() {
        let provider = Arc::new(CannedProvider::new(vec![
            r#"{"checks":[{"status":"pass","rationale":"x"}]}"#,
        ]));
        let llm = router_with(provider);
        let checks = vec!["a".to_string(), "b".to_string()];
        let out = evaluate_custom_checks(&llm, &checks, "diff").await;
        assert!(
            out.is_empty(),
            "wrong result count must degrade rather than mis-align"
        );
    }
}

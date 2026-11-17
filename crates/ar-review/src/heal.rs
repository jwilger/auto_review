use crate::error::ReviewError;
use ar_llm::{CompleteRequest, Message, ModelTier, ResponseFormat, Role, Router};
use ar_prompts::{review_schema, validate_review_output, ReviewOutput};

#[derive(Debug, Clone, Copy)]
pub struct HealConfig {
    pub max_attempts: u32,
}

impl Default for HealConfig {
    fn default() -> Self {
        Self { max_attempts: 3 }
    }
}

/// Call the reasoning-tier LLM to generate a review, validate the JSON
/// output, and on failure feed the validation error back into a follow-up
/// completion. Returns the first valid `ReviewOutput`, or `Unhealable` if
/// every attempt fails.
pub async fn generate_with_self_heal(
    router: &Router,
    system: &str,
    user_prompt: &str,
    config: HealConfig,
) -> Result<ReviewOutput, ReviewError> {
    let mut messages: Vec<Message> = vec![Message::user(user_prompt)];

    // Clamp at-least-one so a misconfigured `max_attempts: 0` still
    // makes one attempt and reports `Unhealable { attempts: 1 }`.
    let max_attempts = config.max_attempts.max(1);
    let mut attempt: u32 = 0;
    loop {
        attempt += 1;
        let req = CompleteRequest {
            system: Some(system.to_string()),
            messages: messages.clone(),
            response_format: Some(ResponseFormat::JsonSchema {
                name: "Review".to_string(),
                schema: review_schema().clone(),
            }),
            // Deterministic output: same prompt → same findings.
            // Matters for bench fixtures (stable scoring across
            // runs), incremental reviews (no spurious re-flagging
            // on the same code), and provider-side prompt caching.
            // Slight tradeoff in phrasing variety, accepted.
            temperature: Some(0.0),
            ..Default::default()
        };
        let resp = router.complete(ModelTier::Reasoning, req).await?;

        match validate_review_output(&resp.content) {
            Ok(out) => {
                tracing::debug!(attempts = attempt, "review JSON validated");
                return Ok(out);
            }
            Err(e) => {
                tracing::warn!(attempt, error = %e, "review JSON failed validation");
                if attempt >= max_attempts {
                    return Err(ReviewError::Unhealable {
                        attempts: max_attempts,
                        last_error: e,
                    });
                }
                messages.push(Message {
                    role: Role::Assistant,
                    content: resp.content,
                });
                messages.push(Message::user(format!(
                    "That output failed validation: {e}\n\nReturn ONLY a JSON object that \
                     conforms to the schema. No prose, no markdown fences."
                )));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ar_llm::testing::ScriptedProvider;
    use std::sync::Arc;

    fn router_with(responses: Vec<&str>) -> (Router, Arc<ScriptedProvider>) {
        let provider = Arc::new(ScriptedProvider::new(responses));
        let router = Router::new().with(ModelTier::Reasoning, provider.clone());
        (router, provider)
    }

    const VALID: &str = r#"{"summary":"ok","findings":[]}"#;

    #[tokio::test]
    async fn returns_immediately_when_first_response_validates() {
        let (router, provider) = router_with(vec![VALID]);
        let out = generate_with_self_heal(&router, "sys", "user", HealConfig::default())
            .await
            .expect("ok");
        assert_eq!(out.summary, "ok");
        assert_eq!(provider.call_count(), 1);
    }

    #[tokio::test]
    async fn retries_when_first_response_is_invalid_json() {
        let (router, provider) = router_with(vec!["not json", VALID]);
        let out = generate_with_self_heal(&router, "sys", "user", HealConfig::default())
            .await
            .expect("ok");
        assert_eq!(out.summary, "ok");
        assert_eq!(provider.call_count(), 2);
    }

    #[tokio::test]
    async fn retries_when_first_response_violates_schema() {
        let bad = r#"{"summary":"x","findings":[{"path":"a","line_start":1,"severity":"oops","message":"m"}]}"#;
        let (router, provider) = router_with(vec![bad, VALID]);
        let out = generate_with_self_heal(&router, "sys", "user", HealConfig::default())
            .await
            .expect("ok");
        assert_eq!(out.summary, "ok");
        assert_eq!(provider.call_count(), 2);
    }

    #[tokio::test]
    async fn max_attempts_zero_clamps_to_one_attempt_no_panic() {
        // Defence in depth: a misconfigured `max_attempts: 0`
        // should not panic. The previous implementation called
        // `.expect("loop ran at least once")` on an unset
        // last_error when the loop range was empty.
        let (router, provider) = router_with(vec!["not json"]);
        let err = generate_with_self_heal(&router, "sys", "user", HealConfig { max_attempts: 0 })
            .await
            .expect_err("should fail");
        match err {
            ReviewError::Unhealable { attempts, .. } => assert_eq!(attempts, 1),
            other => panic!("unexpected: {other:?}"),
        }
        // Loop ran exactly once despite the zero-attempts request.
        assert_eq!(provider.call_count(), 1);
    }

    #[tokio::test]
    async fn gives_up_after_max_attempts() {
        // Always returns the first item without consuming if pop is empty;
        // here we feed three "not json"s and set max_attempts = 3.
        let (router, provider) = router_with(vec!["not json", "not json", "not json"]);
        let err = generate_with_self_heal(&router, "sys", "user", HealConfig { max_attempts: 3 })
            .await
            .expect_err("should fail");
        match err {
            ReviewError::Unhealable { attempts, .. } => assert_eq!(attempts, 3),
            other => panic!("unexpected: {other:?}"),
        }
        assert_eq!(provider.call_count(), 3);
    }
}

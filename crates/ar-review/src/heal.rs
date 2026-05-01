use crate::error::ReviewError;
use ar_llm::{CompleteRequest, Message, ModelTier, ResponseFormat, Role, Router};
use ar_prompts::{review_schema, validate_review_output, ReviewOutput, ValidationError};

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
    let mut last_error: Option<ValidationError> = None;

    // Clamp at-least-one so the loop body always runs once. A
    // misconfigured `max_attempts: 0` would otherwise leave
    // `last_error == None` and trip the `.expect` below.
    let max_attempts = config.max_attempts.max(1);
    for attempt in 1..=max_attempts {
        let req = CompleteRequest {
            system: Some(system.to_string()),
            messages: messages.clone(),
            response_format: Some(ResponseFormat::JsonSchema {
                name: "Review".to_string(),
                schema: review_schema().clone(),
            }),
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
                messages.push(Message {
                    role: Role::Assistant,
                    content: resp.content,
                });
                messages.push(Message::user(format!(
                    "That output failed validation: {e}\n\nReturn ONLY a JSON object that \
                     conforms to the schema. No prose, no markdown fences."
                )));
                last_error = Some(e);
            }
        }
    }

    Err(ReviewError::Unhealable {
        attempts: max_attempts,
        last_error: last_error.expect("loop ran at least once"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use ar_llm::{CompleteResponse, Error, LlmProvider};
    use async_trait::async_trait;
    use std::sync::Arc;
    use std::sync::Mutex;

    struct ScriptedProvider {
        responses: Mutex<Vec<String>>,
        calls: Mutex<u32>,
    }

    impl ScriptedProvider {
        fn new(responses: Vec<&str>) -> Self {
            Self {
                responses: Mutex::new(responses.into_iter().map(String::from).collect()),
                calls: Mutex::new(0),
            }
        }
        fn call_count(&self) -> u32 {
            *self.calls.lock().unwrap()
        }
    }

    #[async_trait]
    impl LlmProvider for ScriptedProvider {
        async fn complete(&self, _req: CompleteRequest) -> Result<CompleteResponse, Error> {
            *self.calls.lock().unwrap() += 1;
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

    fn router_with(responses: Vec<&str>) -> (Router, Arc<ScriptedProvider>) {
        // ScriptedProvider returns responses in REVERSE order (Vec::pop), so
        // pass them last-to-first.
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
        let (router, provider) = router_with(vec![VALID, "not json"]);
        let out = generate_with_self_heal(&router, "sys", "user", HealConfig::default())
            .await
            .expect("ok");
        assert_eq!(out.summary, "ok");
        assert_eq!(provider.call_count(), 2);
    }

    #[tokio::test]
    async fn retries_when_first_response_violates_schema() {
        let bad = r#"{"summary":"x","findings":[{"path":"a","line_start":1,"severity":"oops","message":"m"}]}"#;
        let (router, provider) = router_with(vec![VALID, bad]);
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

//! Shared LLM test fakes.
//!
//! Compiled only with the `test-support` cargo feature (and in this
//! crate's own unit tests), so release builds never link it. Downstream
//! crates enable the feature from their `[dev-dependencies]`.
//!
//! This code is compiled outside `cfg(test)` for downstream crates, so
//! it must not panic: locks recover from poisoning and an exhausted
//! script returns an error instead of aborting the test.

use crate::types::{CompleteRequest, CompleteResponse, Error, LlmProvider};
use async_trait::async_trait;
use std::collections::VecDeque;
use std::sync::{Mutex, PoisonError};

/// Body of the error returned once every scripted response is used.
pub const EXHAUSTED_BODY: &str = "ScriptedProvider: no scripted responses left";

#[derive(Default)]
struct State {
    responses: VecDeque<CompleteResponse>,
    requests: Vec<CompleteRequest>,
}

/// Provider that answers calls from a script, in call order (FIFO:
/// the first scripted response answers the first call), and records
/// every request it receives.
///
/// Once the script is exhausted every further call returns
/// `Error::Provider { status: 500, body: EXHAUSTED_BODY }`.
#[derive(Default)]
pub struct ScriptedProvider {
    state: Mutex<State>,
}

impl ScriptedProvider {
    /// Script plain-text responses in call order, each reporting zero
    /// input and output tokens.
    pub fn new(responses: Vec<&str>) -> Self {
        Self::with_responses(responses.into_iter().map(|content| CompleteResponse {
            content: content.to_string(),
            input_tokens: 0,
            output_tokens: 0,
        }))
    }

    /// Script full responses in call order, so a test can set
    /// per-response `input_tokens` / `output_tokens`.
    pub fn with_responses(responses: impl IntoIterator<Item = CompleteResponse>) -> Self {
        Self {
            state: Mutex::new(State {
                responses: responses.into_iter().collect(),
                requests: Vec::new(),
            }),
        }
    }

    /// Every request received so far, in call order.
    pub fn requests(&self) -> Vec<CompleteRequest> {
        self.lock().requests.clone()
    }

    /// Number of `complete` calls received so far, including calls
    /// made after the script ran out.
    pub fn call_count(&self) -> usize {
        self.lock().requests.len()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

#[async_trait]
impl LlmProvider for ScriptedProvider {
    async fn complete(&self, req: CompleteRequest) -> Result<CompleteResponse, Error> {
        let mut state = self.lock();
        state.requests.push(req);
        state.responses.pop_front().ok_or_else(|| Error::Provider {
            status: 500,
            body: EXHAUSTED_BODY.to_string(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Message;

    fn req(text: &str) -> CompleteRequest {
        CompleteRequest {
            messages: vec![Message::user(text)],
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn answers_calls_in_fifo_order() {
        let provider = ScriptedProvider::new(vec!["first", "second"]);
        let a = provider.complete(req("a")).await.expect("first");
        let b = provider.complete(req("b")).await.expect("second");
        assert_eq!(a.content, "first");
        assert_eq!(b.content, "second");
    }

    #[tokio::test]
    async fn records_requests_in_call_order() {
        let provider = ScriptedProvider::new(vec!["x", "y"]);
        provider.complete(req("one")).await.expect("one");
        provider.complete(req("two")).await.expect("two");
        let seen: Vec<String> = provider
            .requests()
            .into_iter()
            .map(|r| r.messages[0].content.clone())
            .collect();
        assert_eq!(seen, vec!["one", "two"]);
        assert_eq!(provider.call_count(), 2);
    }

    #[tokio::test]
    async fn reports_scripted_usage_tokens() {
        let provider = ScriptedProvider::with_responses(vec![CompleteResponse {
            content: "c".into(),
            input_tokens: 11,
            output_tokens: 7,
        }]);
        let resp = provider.complete(req("q")).await.expect("ok");
        assert_eq!((resp.input_tokens, resp.output_tokens), (11, 7));
    }

    #[tokio::test]
    async fn plain_responses_default_to_zero_usage() {
        let provider = ScriptedProvider::new(vec!["c"]);
        let resp = provider.complete(req("q")).await.expect("ok");
        assert_eq!((resp.input_tokens, resp.output_tokens), (0, 0));
    }

    #[tokio::test]
    async fn exhausted_script_returns_provider_error_without_panicking() {
        let provider = ScriptedProvider::new(vec![]);
        let err = provider.complete(req("q")).await.expect_err("exhausted");
        match err {
            Error::Provider { status, body } => {
                assert_eq!(status, 500);
                assert_eq!(body, EXHAUSTED_BODY);
            }
            other => panic!("unexpected error: {other:?}"),
        }
        assert_eq!(provider.call_count(), 1);
    }
}

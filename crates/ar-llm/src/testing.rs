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
use alloc::collections::VecDeque;
use async_trait::async_trait;
use std::sync::{Mutex, MutexGuard, PoisonError};

/// Body of the error returned once every scripted response is used.
pub const EXHAUSTED_BODY: &str = "ScriptedProvider: no scripted responses left";

/// Script and call log behind the [`ScriptedProvider`] lock.
#[derive(Default)]
struct State {
    /// Every request received so far, in call order.
    requests: Vec<CompleteRequest>,
    /// Responses still to hand out, first call first.
    responses: VecDeque<CompleteResponse>,
}

/// Provider that answers calls from a script, in call order (FIFO:
/// the first scripted response answers the first call), and records
/// every request it receives.
///
/// Once the script is exhausted every further call returns
/// `Error::Provider { status: 500, body: EXHAUSTED_BODY }`.
#[derive(Default)]
pub struct ScriptedProvider {
    /// Script and call log, shared by concurrent calls.
    state: Mutex<State>,
}

impl ScriptedProvider {
    /// Number of `complete` calls received so far, including calls
    /// made after the script ran out.
    #[inline]
    pub fn call_count(&self) -> usize {
        self.lock().requests.len()
    }

    /// Lock the state, recovering it if a panicking test poisoned the lock.
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Script plain-text responses in call order, each reporting zero
    /// input and output tokens.
    #[inline]
    #[must_use = "the scripted provider is the only output"]
    pub fn new(responses: Vec<&str>) -> Self {
        Self::with_responses(responses.into_iter().map(|content| CompleteResponse {
            content: content.to_owned(),
            input_tokens: 0,
            output_tokens: 0,
        }))
    }

    /// Every request received so far, in call order.
    #[inline]
    pub fn requests(&self) -> Vec<CompleteRequest> {
        self.lock().requests.clone()
    }

    /// Script full responses in call order, so a test can set
    /// per-response `input_tokens` / `output_tokens`.
    #[inline]
    pub fn with_responses(responses: impl IntoIterator<Item = CompleteResponse>) -> Self {
        Self {
            state: Mutex::new(State {
                requests: Vec::new(),
                responses: responses.into_iter().collect(),
            }),
        }
    }
}

#[async_trait]
impl LlmProvider for ScriptedProvider {
    #[inline]
    async fn complete(&self, req: CompleteRequest) -> Result<CompleteResponse, Error> {
        let mut state = self.lock();
        state.requests.push(req);
        state.responses.pop_front().ok_or_else(|| Error::Provider {
            status: 500,
            body: EXHAUSTED_BODY.to_owned(),
        })
    }
}

#[cfg(test)]
#[expect(
    clippy::expect_used,
    reason = "tests call the fake with expect so an unexpected error fails the test at that line \
              with a label"
)]
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
    #[expect(
        clippy::min_ident_chars,
        reason = "`a` and `b` are read by the assertions below; renaming them would edit those \
                  assertion lines, which this lint-only change leaves untouched"
    )]
    async fn answers_calls_in_fifo_order() {
        let provider = ScriptedProvider::new(vec!["first", "second"]);
        let a = provider.complete(req("a")).await.expect("first");
        let b = provider.complete(req("b")).await.expect("second");
        assert_eq!(a.content, "first");
        assert_eq!(b.content, "second");
    }

    #[tokio::test]
    #[expect(
        clippy::indexing_slicing,
        reason = "every request built by req() carries exactly one message, so index 0 exists"
    )]
    async fn records_requests_in_call_order() {
        let provider = ScriptedProvider::new(vec!["x", "y"]);
        provider.complete(req("one")).await.expect("one");
        provider.complete(req("two")).await.expect("two");
        let seen: Vec<String> = provider
            .requests()
            .into_iter()
            .map(|request| request.messages[0].content.clone())
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
    #[expect(
        clippy::panic,
        reason = "the catch-all arm fails the test and names the unexpected error variant"
    )]
    #[expect(
        clippy::wildcard_enum_match_arm,
        reason = "every variant other than Provider is a test failure, so one catch-all arm \
                  reports them all"
    )]
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

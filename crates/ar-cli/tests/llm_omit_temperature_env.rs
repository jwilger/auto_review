//! End-to-end check that `LLM_OMIT_TEMPERATURE` reaches the provider a
//! CLI command builds. Spawns the real `auto-review` binary so the
//! environment variable is read by clap exactly as in production,
//! without mutating this test process's environment.

use core::error::Error;
use std::path::PathBuf;
use std::process::{Command, Output};

use tokio::task::spawn_blocking;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// Boxed error so every test step can propagate failures with `?`.
type TestResult<T = ()> = Result<T, Box<dyn Error + Send + Sync>>;

/// Decoded JSON bodies of every `/v1/chat/completions` request the mock
/// LLM received.
#[expect(
    clippy::expect_used,
    reason = "the assertions call this helper inline and stay as written, so it cannot return a Result; a missing recording or non-JSON body must fail the test"
)]
async fn chat_bodies(server: &MockServer) -> Vec<serde_json::Value> {
    server
        .received_requests()
        .await
        .expect("request recording enabled")
        .iter()
        .filter(|request| request.url.path() == "/v1/chat/completions")
        .map(|request| serde_json::from_slice(&request.body).expect("json body"))
        .collect()
}

/// Mock LLM that answers every chat completion with an empty review.
async fn mock_llm() -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "choices": [{"message": {"content": "{\"summary\":\"ok\",\"findings\":[]}"}}]
        })))
        .mount(&server)
        .await;
    server
}

/// Run `auto-review bench run` against `llm_base_url` with
/// `LLM_OMIT_TEMPERATURE` set to `value` (or removed when `None`).
async fn run_bench(llm_base_url: String, value: Option<&'static str>) -> TestResult<Output> {
    let output = spawn_blocking(move || {
        let mut command = Command::new(env!("CARGO_BIN_EXE_auto-review"));
        command
            .args(["bench", "run", "--json", "--llm-model", "gpt-5.1"])
            .arg("--llm-base-url")
            .arg(llm_base_url)
            .arg(shipped_fixture())
            .env_remove("LLM_API_KEY")
            .env_remove("LLM_CHEAP_MODEL")
            .env("RUST_LOG", "off");
        match value {
            Some(setting) => command.env("LLM_OMIT_TEMPERATURE", setting),
            None => command.env_remove("LLM_OMIT_TEMPERATURE"),
        };
        command.output()
    })
    .await??;
    Ok(output)
}

/// Path to a fixture shipped in the repository's `bench/fixtures`.
fn shipped_fixture() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../bench/fixtures/typo-in-error-message.json")
}

#[expect(
    clippy::tests_outside_test_module,
    reason = "files under tests/ are separate integration-test crates built only by cargo test; a #[cfg(test)] module wrapper would add nothing"
)]
#[expect(
    clippy::panic_in_result_fn,
    reason = "the test reports setup and request failures through `?` and its checks through the assertions, which stay as written"
)]
#[tokio::test]
async fn env_true_omits_temperature_from_bench_requests() -> TestResult {
    let server = mock_llm().await;
    let output = run_bench(server.uri(), Some("true")).await?;
    let bodies = chat_bodies(&server).await;
    assert!(
        !bodies.is_empty(),
        "bench should call the LLM; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    for body in bodies {
        assert!(
            body.get("temperature").is_none(),
            "temperature must be omitted: {body}"
        );
    }
    Ok(())
}

#[expect(
    clippy::tests_outside_test_module,
    reason = "files under tests/ are separate integration-test crates built only by cargo test; a #[cfg(test)] module wrapper would add nothing"
)]
#[expect(
    clippy::panic_in_result_fn,
    reason = "the test reports setup and request failures through `?` and its checks through the assertions, which stay as written"
)]
#[expect(
    clippy::default_numeric_fallback,
    reason = "the json! number is the untyped JSON temperature value the assertion expects on the wire; a suffix would trip separated_literal_suffix or unseparated_literal_suffix instead"
)]
#[tokio::test]
async fn env_unset_keeps_temperature_zero_in_bench_requests() -> TestResult {
    let server = mock_llm().await;
    let output = run_bench(server.uri(), None).await?;
    let bodies = chat_bodies(&server).await;
    assert!(
        !bodies.is_empty(),
        "bench should call the LLM; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    for body in bodies {
        assert_eq!(
            body.get("temperature"),
            Some(&serde_json::json!(0.0)),
            "{body}"
        );
    }
    Ok(())
}

#[expect(
    clippy::tests_outside_test_module,
    reason = "files under tests/ are separate integration-test crates built only by cargo test; a #[cfg(test)] module wrapper would add nothing"
)]
#[expect(
    clippy::panic_in_result_fn,
    reason = "the test reports setup and request failures through `?` and its checks through the assertions, which stay as written"
)]
#[expect(
    clippy::default_numeric_fallback,
    reason = "the expected exit code literal is compared against Option<i32>; a suffix would trip separated_literal_suffix or unseparated_literal_suffix instead"
)]
#[tokio::test]
async fn env_invalid_fails_argument_parsing_naming_the_setting() -> TestResult {
    let server = mock_llm().await;
    let output = run_bench(server.uri(), Some("bogus")).await?;
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(2), "stderr: {stderr}");
    assert!(
        stderr.contains("--llm-omit-temperature") && stderr.contains("LLM_OMIT_TEMPERATURE"),
        "stderr: {stderr}"
    );
    assert!(
        chat_bodies(&server).await.is_empty(),
        "nothing may reach the LLM when the setting is invalid"
    );
    Ok(())
}

use ar_llm::pricing::{default_openai_price_table, load_openai_price_table};
use core::error::Error;
use std::fs;
use std::path::Path;
use std::{env, process};

#[test]
#[expect(
    clippy::tests_outside_test_module,
    reason = "files under tests/ are separate integration-test crates built only by cargo test; \
              there is no #[cfg(test)] module to place this in"
)]
#[expect(
    clippy::panic_in_result_fn,
    reason = "the assertions are the test; the Result return only propagates temp-file and JSON \
              setup errors with ?"
)]
fn openai_price_table_has_defaults_and_operator_overrides() -> Result<(), Box<dyn Error>> {
    let load_price_table = load_openai_price_table;

    let default_table = load_price_table(None::<&Path>)?;
    let defaults = serde_json::to_string_pretty(&default_table)?;

    assert!(defaults.contains("\"gpt-4o-mini\""));
    assert!(defaults.contains("\"text-embedding-3-small\""));

    let mut override_path = env::temp_dir();
    let pid = process::id();
    override_path.push(format!("ar-llm-openai-pricing-test-{pid}.override.json"));

    let overridden_model_rate: f64 = 1_234_567.0;
    let override_body = serde_json::json!({
        "gpt-4o-mini": { "input": overridden_model_rate }
    });
    fs::write(
        &override_path,
        serde_json::to_string_pretty(&override_body)?,
    )?;

    let overridden_table = load_price_table(Some(override_path.as_path()))?;
    let overridden = serde_json::to_string_pretty(&overridden_table)?;

    assert!(overridden.contains("\"gpt-4o-mini\""));
    assert!(overridden.contains("1234567"));

    fs::remove_file(&override_path)?;

    Ok(())
}

#[test]
#[expect(
    clippy::tests_outside_test_module,
    reason = "files under tests/ are separate integration-test crates built only by cargo test; \
              there is no #[cfg(test)] module to place this in"
)]
#[expect(
    clippy::expect_used,
    reason = "a missing estimate fails the test at this line with a label"
)]
#[expect(
    clippy::default_numeric_fallback,
    reason = "the expected USD value is inside the assertion below, which this lint-only change \
              leaves untouched"
)]
fn price_table_estimates_usage_by_provider_and_model() {
    let table = default_openai_price_table();

    let estimated_usd = table
        .estimate_usage_usd(
            "https://api.openai.com/v1",
            "gpt-4o-mini",
            1_000_000,
            500_000,
            0,
        )
        .expect("provider/model pricing estimates should be available");

    assert_eq!(estimated_usd, 0.45);
}

#[test]
#[expect(
    clippy::tests_outside_test_module,
    reason = "files under tests/ are separate integration-test crates built only by cargo test; \
              there is no #[cfg(test)] module to place this in"
)]
#[expect(
    clippy::panic_in_result_fn,
    reason = "the assertion is the test; the Result return only propagates temp-file and JSON \
              setup errors with ?"
)]
#[expect(
    clippy::default_numeric_fallback,
    reason = "untyped zero rates inside the json! override body are JSON values written to the \
              override file, not Rust arithmetic"
)]
fn estimate_usage_uses_provider_qualified_override_before_model_fallback(
) -> Result<(), Box<dyn Error>> {
    let mut override_path = env::temp_dir();
    let pid = process::id();
    override_path.push(format!(
        "ar-llm-openai-pricing-provider-override-test-{pid}.override.json"
    ));

    let provider_specific_input_rate: f64 = 9.0;
    let override_body = serde_json::json!({
        "https://api.openai.com/v1|gpt-4o-mini": {
            "input": provider_specific_input_rate,
            "output": 0.0,
            "embedding": 0.0,
        }
    });
    fs::write(
        &override_path,
        serde_json::to_string_pretty(&override_body)?,
    )?;

    let table = load_openai_price_table(Some(override_path.as_path()))?;
    let estimated_usd = table
        .estimate_usage_usd("https://api.openai.com/v1", "gpt-4o-mini", 1_000_000, 0, 0)
        .ok_or("provider/model pricing estimates should be available")?;

    assert_eq!(estimated_usd, provider_specific_input_rate);

    fs::remove_file(&override_path)?;
    Ok(())
}

use std::error::Error;
use std::fs;
use std::path::PathBuf;

#[test]
fn openai_price_table_has_defaults_and_operator_overrides() -> Result<(), Box<dyn Error>> {
    let load_price_table = ar_llm::pricing::load_openai_price_table;

    let defaults = load_price_table(None::<&std::path::Path>)?;
    let defaults = serde_json::to_string_pretty(&defaults)?;

    assert!(defaults.contains("\"gpt-4o-mini\""));
    assert!(defaults.contains("\"text-embedding-3-small\""));

    let mut override_path = PathBuf::from(std::env::temp_dir());
    let pid = std::process::id();
    override_path.push(format!("ar-llm-openai-pricing-test-{pid}.override.json"));

    let overridden_model_rate = 1_234_567.0;
    let override_body = serde_json::json!({
        "gpt-4o-mini": { "input": overridden_model_rate }
    });
    fs::write(&override_path, serde_json::to_string_pretty(&override_body)?)?;

    let overridden = load_price_table(Some(override_path.as_path()))?;
    let overridden = serde_json::to_string_pretty(&overridden)?;

    assert!(overridden.contains("\"gpt-4o-mini\""));
    assert!(overridden.contains("1234567"));

    fs::remove_file(&override_path)?;

    Ok(())
}

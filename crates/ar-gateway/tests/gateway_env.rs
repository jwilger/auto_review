use std::process::Command;

#[test]
fn gateway_startup_requires_gateway_specific_forgejo_token() {
    let output = Command::new(env!("CARGO_BIN_EXE_ar-gateway"))
        .env("WEBHOOK_SECRET", "0123456789abcdef0123456789abcdef")
        .env("FORGEJO_BASE_URL", "https://forgejo.example.invalid")
        .env_remove("AR_FORGEJO_TOKEN")
        .env_remove("FORGEJO_TOKEN")
        .output()
        .expect("run ar-gateway binary");

    assert!(
        !output.status.success(),
        "gateway should fail startup when AR_FORGEJO_TOKEN is unset"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("AR_FORGEJO_TOKEN is required"),
        "missing-token startup error should name AR_FORGEJO_TOKEN, got: {stderr}"
    );
}

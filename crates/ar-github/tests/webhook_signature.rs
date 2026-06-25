use ar_github::{verify_webhook_signature, WebhookSignatureError};
use hmac::{Hmac, Mac};
use sha2::Sha256;

type HmacSha256 = Hmac<Sha256>;

fn signature(secret: &str, body: &[u8]) -> String {
    let mut mac = HmacSha256::new_from_slice(secret.as_bytes()).expect("valid key");
    mac.update(body);
    format!("sha256={}", hex::encode(mac.finalize().into_bytes()))
}

#[test]
fn github_signature_256_passes_with_expected_prefix() {
    let body = br#"{"action":"opened"}"#;

    verify_webhook_signature("secret", body, &signature("secret", body)).expect("valid signature");
}

#[test]
fn github_signature_256_rejects_missing_prefix() {
    let body = br#"{"action":"opened"}"#;
    let without_prefix = signature("secret", body)
        .strip_prefix("sha256=")
        .expect("prefix")
        .to_string();

    let error =
        verify_webhook_signature("secret", body, &without_prefix).expect_err("missing prefix");

    assert!(matches!(error, WebhookSignatureError::InvalidFormat));
}

#[test]
fn github_signature_256_rejects_wrong_secret() {
    let body = br#"{"action":"opened"}"#;

    let error =
        verify_webhook_signature("secret", body, &signature("other", body)).expect_err("mismatch");

    assert!(matches!(error, WebhookSignatureError::Mismatch));
}

//! GitHub webhook signature helpers.

use hmac::{Hmac, Mac};
use sha2::Sha256;
use thiserror::Error;

type HmacSha256 = Hmac<Sha256>;

const SHA256_PREFIX: &str = "sha256=";

#[derive(Debug, Error, PartialEq, Eq)]
pub enum WebhookSignatureError {
    #[error("signature header must use sha256=<hex> format")]
    InvalidFormat,
    #[error("signature hex is invalid")]
    InvalidHex,
    #[error("signature does not match")]
    Mismatch,
    #[error("webhook secret is invalid")]
    InvalidSecret,
}

/// Verify GitHub's `X-Hub-Signature-256` header value.
///
/// GitHub signs the raw request body as `sha256=<hex hmac>`. The comparison is
/// delegated to the HMAC crate's constant-time verifier.
pub fn verify_webhook_signature(
    secret: &str,
    body: &[u8],
    signature_header: &str,
) -> Result<(), WebhookSignatureError> {
    let signature_hex = signature_header
        .trim()
        .strip_prefix(SHA256_PREFIX)
        .ok_or(WebhookSignatureError::InvalidFormat)?;
    let provided = hex::decode(signature_hex).map_err(|_| WebhookSignatureError::InvalidHex)?;
    let mut mac = HmacSha256::new_from_slice(secret.as_bytes())
        .map_err(|_| WebhookSignatureError::InvalidSecret)?;
    mac.update(body);
    mac.verify_slice(&provided)
        .map_err(|_| WebhookSignatureError::Mismatch)
}

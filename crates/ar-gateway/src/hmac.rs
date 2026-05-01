//! Constant-time HMAC-SHA256 verification for Forgejo webhooks.
//!
//! Forgejo sends `X-Forgejo-Signature` as a hex-encoded HMAC-SHA256 digest
//! of the raw request body, keyed by the configured webhook secret.

use ::hmac::{Hmac, Mac};
use sha2::Sha256;
use subtle::ConstantTimeEq;

type HmacSha256 = Hmac<Sha256>;

#[derive(Debug, thiserror::Error)]
pub enum HmacError {
    #[error("missing signature header")]
    Missing,
    #[error("signature header is not valid hex")]
    NotHex,
    #[error("signature does not match")]
    Mismatch,
    #[error("invalid secret")]
    InvalidSecret,
}

/// Verify a hex-encoded HMAC-SHA256 signature against the given body.
///
/// Returns `Ok(())` iff the signature is valid. Comparison is constant-time.
pub fn verify(secret: &str, body: &[u8], signature_hex: &str) -> Result<(), HmacError> {
    let mut mac =
        HmacSha256::new_from_slice(secret.as_bytes()).map_err(|_| HmacError::InvalidSecret)?;
    mac.update(body);
    let expected = mac.finalize().into_bytes();
    let provided = hex::decode(signature_hex.trim()).map_err(|_| HmacError::NotHex)?;
    if expected.len() != provided.len() {
        return Err(HmacError::Mismatch);
    }
    if expected.as_slice().ct_eq(&provided).into() {
        Ok(())
    } else {
        Err(HmacError::Mismatch)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sign(secret: &str, body: &[u8]) -> String {
        let mut mac = HmacSha256::new_from_slice(secret.as_bytes()).unwrap();
        mac.update(body);
        hex::encode(mac.finalize().into_bytes())
    }

    #[test]
    fn valid_signature_passes() {
        let body = br#"{"action":"opened"}"#;
        let sig = sign("s3cret", body);
        assert!(verify("s3cret", body, &sig).is_ok());
    }

    #[test]
    fn tampered_body_fails() {
        let body = br#"{"action":"opened"}"#;
        let sig = sign("s3cret", body);
        let tampered = br#"{"action":"closed"}"#;
        assert!(matches!(
            verify("s3cret", tampered, &sig),
            Err(HmacError::Mismatch)
        ));
    }

    #[test]
    fn wrong_secret_fails() {
        let body = b"x";
        let sig = sign("a", body);
        assert!(matches!(verify("b", body, &sig), Err(HmacError::Mismatch)));
    }

    #[test]
    fn non_hex_signature_fails() {
        let body = b"x";
        assert!(matches!(
            verify("s", body, "not-hex"),
            Err(HmacError::NotHex)
        ));
    }
}

//! X-Hub-Signature-256 verification. Never skipped — Meta signs every
//! webhook POST with the app secret over the raw body.

use hmac::{Hmac, Mac};
use sha2::Sha256;

/// `header` is the raw `X-Hub-Signature-256` value (`sha256=<hex>`).
/// Comparison is constant-time via `Mac::verify_slice`.
pub fn verify(app_secret: &str, body: &[u8], header: Option<&str>) -> bool {
    let Some(hex_sig) = header.and_then(|h| h.strip_prefix("sha256=")) else {
        return false;
    };
    let Ok(expected) = hex::decode(hex_sig) else {
        return false;
    };
    let mut mac = Hmac::<Sha256>::new_from_slice(app_secret.as_bytes())
        .expect("hmac accepts any key length");
    mac.update(body);
    mac.verify_slice(&expected).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use hmac::{Hmac, Mac};
    use sha2::Sha256;

    fn sign(secret: &str, body: &[u8]) -> String {
        let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).unwrap();
        mac.update(body);
        format!("sha256={}", hex::encode(mac.finalize().into_bytes()))
    }

    #[test]
    fn accepts_valid_signature() {
        let body = br#"{"object":"whatsapp_business_account"}"#;
        let sig = sign("topsecret", body);
        assert!(verify("topsecret", body, Some(&sig)));
    }

    #[test]
    fn rejects_bad_signature_missing_header_and_wrong_secret() {
        let body = b"payload";
        let sig = sign("topsecret", body);
        assert!(!verify("topsecret", body, None));
        assert!(!verify("topsecret", body, Some("sha256=deadbeef")));
        assert!(!verify("othersecret", body, Some(&sig)));
        assert!(!verify("topsecret", b"tampered", Some(&sig)));
    }
}

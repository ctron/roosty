use std::time::{Duration, SystemTime, UNIX_EPOCH};

use base64::{
    Engine,
    engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD},
};
use p256::{
    SecretKey,
    ecdsa::{Signature, SigningKey, signature::Signer},
    pkcs8::DecodePrivateKey,
};
use serde::Serialize;
use url::Url;

use crate::WebPushError;

/// Stable P-256 identity used to authenticate an application server to push services.
#[derive(Clone, Debug)]
pub struct VapidIdentity {
    signing_key: SigningKey,
    subject: String,
}

impl VapidIdentity {
    /// Decode base64 PKCS#8 DER and validate the VAPID subject URI.
    pub fn from_base64_pkcs8(
        value: &str,
        subject: impl Into<String>,
    ) -> Result<Self, WebPushError> {
        let der = STANDARD
            .decode(value)
            .map_err(|_| WebPushError::InvalidVapidKey)?;
        let secret = SecretKey::from_pkcs8_der(&der).map_err(|_| WebPushError::InvalidVapidKey)?;
        let subject = subject.into();
        let subject_url = Url::parse(&subject).map_err(|_| WebPushError::InvalidVapidKey)?;
        if subject_url.scheme() != "https" && subject_url.scheme() != "mailto" {
            return Err(WebPushError::InvalidVapidKey);
        }
        Ok(Self {
            signing_key: SigningKey::from(secret),
            subject,
        })
    }

    pub fn public_key_base64(&self) -> String {
        URL_SAFE_NO_PAD.encode(
            self.signing_key
                .verifying_key()
                .to_sec1_point(false)
                .as_bytes(),
        )
    }

    pub(crate) fn authorization(&self, endpoint: &Url) -> Result<String, WebPushError> {
        let audience = endpoint.origin().ascii_serialization();
        let expiration = SystemTime::now()
            .checked_add(Duration::from_secs(12 * 60 * 60))
            .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
            .ok_or(WebPushError::VapidSigning)?
            .as_secs();
        let header = URL_SAFE_NO_PAD.encode(br#"{"typ":"JWT","alg":"ES256"}"#);
        let claims = URL_SAFE_NO_PAD.encode(
            serde_json::to_vec(&Claims {
                aud: &audience,
                exp: expiration,
                sub: &self.subject,
            })
            .map_err(|_| WebPushError::VapidSigning)?,
        );
        let signing_input = format!("{header}.{claims}");
        let signature: Signature = self.signing_key.sign(signing_input.as_bytes());
        let token = format!(
            "{signing_input}.{}",
            URL_SAFE_NO_PAD.encode(signature.to_bytes())
        );
        Ok(format!("vapid t={token}, k={}", self.public_key_base64()))
    }
}

#[derive(Serialize)]
struct Claims<'a> {
    aud: &'a str,
    exp: u64,
    sub: &'a str,
}

#[cfg(test)]
mod tests {
    use super::VapidIdentity;
    use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
    use p256::ecdsa::{Signature, signature::Verifier};
    use url::Url;

    const PRIVATE_KEY: &str = "MIGHAgEAMBMGByqGSM49AgEGCCqGSM49AwEHBG0wawIBAQQg7ki2JNeU+GLhnNacatYTpVJNFd3uIKWr+Inj/vYFMAShRANCAAQyUFnxhJ7CSBxmKk5Qj6d0UWOBJ68nwsB+XAxsp4hAJ/mVfmeryWYGKx9JaZaAWBfSybFhK0inH6o1XIJH5CRW";

    #[test]
    fn authorization_contains_a_verifiable_origin_scoped_jwt() {
        let identity = VapidIdentity::from_base64_pkcs8(PRIVATE_KEY, "https://social.example")
            .unwrap_or_else(|error| unreachable!("fixture key is valid: {error}"));
        let endpoint = Url::parse("https://push.example/messages/1")
            .unwrap_or_else(|error| unreachable!("fixture URL is valid: {error}"));
        let authorization = identity
            .authorization(&endpoint)
            .unwrap_or_else(|error| unreachable!("authorization succeeds: {error}"));
        let token = authorization
            .strip_prefix("vapid t=")
            .and_then(|value| value.split_once(", k=").map(|(token, _)| token))
            .unwrap_or_else(|| unreachable!("authorization has VAPID fields"));
        let mut segments = token.split('.');
        let header = segments
            .next()
            .unwrap_or_else(|| unreachable!("JWT header"));
        let claims = segments
            .next()
            .unwrap_or_else(|| unreachable!("JWT claims"));
        let signature = segments
            .next()
            .unwrap_or_else(|| unreachable!("JWT signature"));
        assert!(segments.next().is_none());
        let signature = URL_SAFE_NO_PAD
            .decode(signature)
            .ok()
            .and_then(|bytes| Signature::from_slice(&bytes).ok())
            .unwrap_or_else(|| unreachable!("signature is raw ES256"));
        identity
            .signing_key
            .verifying_key()
            .verify(format!("{header}.{claims}").as_bytes(), &signature)
            .unwrap_or_else(|error| unreachable!("signature verifies: {error}"));
        let claims: serde_json::Value = URL_SAFE_NO_PAD
            .decode(claims)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_else(|| unreachable!("claims decode"));
        assert_eq!(claims["aud"], "https://push.example");
        assert_eq!(claims["sub"], "https://social.example");
        assert!(claims["exp"].as_u64().is_some());
    }

    #[test]
    fn rejects_malformed_keys_and_subjects() {
        for key in ["not-base64", "AQID"] {
            assert!(VapidIdentity::from_base64_pkcs8(key, "https://social.example").is_err());
        }
        for subject in ["social.example", "http://social.example"] {
            assert!(VapidIdentity::from_base64_pkcs8(PRIVATE_KEY, subject).is_err());
        }
        assert!(VapidIdentity::from_base64_pkcs8(PRIVATE_KEY, "mailto:admin@example.com").is_ok());
    }
}

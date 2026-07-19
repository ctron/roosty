#![deny(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

//! Web Push encryption, VAPID authentication, and hardened delivery.

mod crypto;
mod endpoint;
mod vapid;

use std::{
    borrow::Cow,
    time::{Duration, SystemTime},
};

use reqwest::{StatusCode, header};
use thiserror::Error;
use url::Url;

pub use vapid::VapidIdentity;

/// Resolve an endpoint and reject any non-public destination before it is persisted.
pub async fn validate_endpoint(endpoint: &Url) -> Result<(), WebPushError> {
    endpoint::resolve_public(endpoint).await.map(|_| ())
}

/// Web Push content-encoding profile selected by the subscriber.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum Encoding {
    /// RFC 8291 and RFC 8188 encoding.
    #[default]
    Aes128Gcm,
    /// Legacy Mastodon-compatible draft encoding.
    AesGcm,
}

/// Validated Web Push subscription material.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Subscription {
    endpoint: Url,
    p256dh: [u8; 65],
    auth: [u8; 16],
    encoding: Encoding,
}

impl Subscription {
    /// Validate and construct a subscription received from a client.
    pub fn new(
        endpoint: Url,
        p256dh: &[u8],
        auth: &[u8],
        encoding: Encoding,
    ) -> Result<Self, WebPushError> {
        endpoint::validate_url(&endpoint)?;
        let p256dh: [u8; 65] = p256dh
            .try_into()
            .map_err(|_| WebPushError::InvalidSubscriberKey)?;
        p256::PublicKey::from_sec1_bytes(&p256dh)
            .map_err(|_| WebPushError::InvalidSubscriberKey)?;
        let auth: [u8; 16] = auth
            .try_into()
            .map_err(|_| WebPushError::InvalidAuthSecret)?;
        Ok(Self {
            endpoint,
            p256dh,
            auth,
            encoding,
        })
    }

    /// Return the push-service URL.
    pub fn endpoint(&self) -> &Url {
        &self.endpoint
    }

    /// Return the selected content encoding.
    pub fn encoding(&self) -> Encoding {
        self.encoding
    }
}

/// Per-message delivery controls.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SendOptions {
    /// How long a push service may retain the message.
    pub ttl: Duration,
    /// Delivery urgency from RFC 8030.
    pub urgency: Urgency,
}

impl Default for SendOptions {
    fn default() -> Self {
        Self {
            ttl: Duration::from_secs(48 * 60 * 60),
            urgency: Urgency::Normal,
        }
    }
}

/// RFC 8030 delivery urgency.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum Urgency {
    VeryLow,
    Low,
    #[default]
    Normal,
    High,
}

impl Urgency {
    fn as_str(self) -> &'static str {
        match self {
            Self::VeryLow => "very-low",
            Self::Low => "low",
            Self::Normal => "normal",
            Self::High => "high",
        }
    }
}

/// Action the caller should take after a completed HTTP exchange.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DeliveryOutcome {
    Success,
    Retryable {
        status: Option<u16>,
        retry_after: Option<Duration>,
    },
    PermanentFailure {
        status: u16,
    },
}

/// Reusable VAPID-authenticated sender.
#[derive(Clone, Debug)]
pub struct Client {
    vapid: VapidIdentity,
    connect_timeout: Duration,
    request_timeout: Duration,
}

impl Client {
    pub fn new(vapid: VapidIdentity) -> Self {
        Self {
            vapid,
            connect_timeout: Duration::from_secs(10),
            request_timeout: Duration::from_secs(20),
        }
    }

    /// Encrypt and deliver one message after resolving the endpoint to public addresses.
    pub async fn send(
        &self,
        subscription: &Subscription,
        payload: &[u8],
        options: SendOptions,
    ) -> Result<DeliveryOutcome, WebPushError> {
        let resolved = endpoint::resolve_public(subscription.endpoint()).await?;
        let host = subscription
            .endpoint()
            .host_str()
            .ok_or(WebPushError::InvalidEndpoint("host is missing".into()))?;
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(self.connect_timeout)
            .timeout(self.request_timeout)
            .resolve_to_addrs(host, &resolved)
            .build()?;

        let encrypted = crypto::encrypt(subscription, payload)?;
        let authorization = self.vapid.authorization(subscription.endpoint())?;
        let mut request = client
            .post(subscription.endpoint().clone())
            .header(header::AUTHORIZATION, authorization)
            .header(header::CONTENT_ENCODING, encrypted.content_encoding)
            .header(header::CONTENT_TYPE, "application/octet-stream")
            .header("TTL", options.ttl.as_secs().to_string())
            .header("Urgency", options.urgency.as_str())
            .body(encrypted.body);
        for (name, value) in encrypted.extra_headers {
            let value = if name == "Crypto-Key" {
                format!("{value}; p256ecdsa={}", self.vapid.public_key_base64())
            } else {
                value
            };
            request = request.header(name, value);
        }
        let response = match request.send().await {
            Ok(response) => response,
            Err(error) if error.is_timeout() || error.is_connect() || error.is_request() => {
                return Ok(DeliveryOutcome::Retryable {
                    status: None,
                    retry_after: None,
                });
            }
            Err(error) => return Err(error.into()),
        };
        Ok(classify_response(
            response.status(),
            response
                .headers()
                .get(header::RETRY_AFTER)
                .and_then(|value| value.to_str().ok()),
            SystemTime::now(),
        ))
    }

    pub fn vapid_public_key(&self) -> String {
        self.vapid.public_key_base64()
    }
}

fn classify_response(
    status: StatusCode,
    retry_after: Option<&str>,
    now: SystemTime,
) -> DeliveryOutcome {
    if status.is_success() {
        return DeliveryOutcome::Success;
    }
    if status == StatusCode::REQUEST_TIMEOUT
        || status == StatusCode::TOO_MANY_REQUESTS
        || status.is_server_error()
    {
        let retry_after = retry_after.and_then(|value| {
            value
                .parse::<u64>()
                .map(Duration::from_secs)
                .ok()
                .or_else(|| {
                    httpdate::parse_http_date(value)
                        .ok()
                        .and_then(|time| time.duration_since(now).ok())
                })
        });
        return DeliveryOutcome::Retryable {
            status: Some(status.as_u16()),
            retry_after,
        };
    }
    DeliveryOutcome::PermanentFailure {
        status: status.as_u16(),
    }
}

/// Typed protocol and transport failures.
#[derive(Debug, Error)]
pub enum WebPushError {
    #[error("invalid push endpoint: {0}")]
    InvalidEndpoint(Cow<'static, str>),
    #[error("push endpoint resolves to a non-public address")]
    UnsafeEndpoint,
    #[error("push endpoint could not be resolved")]
    UnresolvedEndpoint,
    #[error("invalid subscriber P-256 public key")]
    InvalidSubscriberKey,
    #[error("invalid subscriber authentication secret")]
    InvalidAuthSecret,
    #[error("invalid VAPID private key")]
    InvalidVapidKey,
    #[error("push payload exceeds the single-record limit")]
    PayloadTooLarge,
    #[error("Web Push encryption failed")]
    Encryption,
    #[error("VAPID signing failed")]
    VapidSigning,
    #[error(transparent)]
    Http(#[from] reqwest::Error),
}

#[cfg(test)]
mod tests {
    use super::{DeliveryOutcome, Encoding, Subscription, WebPushError, classify_response};
    use p256::{SecretKey, elliptic_curve::sec1::ToSec1Point};
    use reqwest::StatusCode;
    use std::time::{Duration, SystemTime};
    use url::Url;

    fn subscriber_key() -> Vec<u8> {
        SecretKey::from_slice(&[7_u8; 32])
            .unwrap()
            .public_key()
            .to_sec1_point(false)
            .as_bytes()
            .to_vec()
    }

    #[test]
    fn subscription_accepts_valid_standard_and_legacy_material() {
        let endpoint = Url::parse("https://1.1.1.1/push").unwrap();
        for encoding in [Encoding::Aes128Gcm, Encoding::AesGcm] {
            let subscription =
                Subscription::new(endpoint.clone(), &subscriber_key(), &[9_u8; 16], encoding)
                    .unwrap();
            assert_eq!(subscription.encoding(), encoding);
        }
    }

    #[test]
    fn subscription_rejects_invalid_key_material() {
        let endpoint = Url::parse("https://1.1.1.1/push").unwrap();
        assert!(matches!(
            Subscription::new(
                endpoint.clone(),
                &[4_u8; 64],
                &[0_u8; 16],
                Encoding::Aes128Gcm
            ),
            Err(WebPushError::InvalidSubscriberKey)
        ));
        assert!(matches!(
            Subscription::new(
                endpoint.clone(),
                &[4_u8; 65],
                &[0_u8; 16],
                Encoding::Aes128Gcm
            ),
            Err(WebPushError::InvalidSubscriberKey)
        ));
        assert!(matches!(
            Subscription::new(
                endpoint,
                &subscriber_key(),
                &[0_u8; 15],
                Encoding::Aes128Gcm
            ),
            Err(WebPushError::InvalidAuthSecret)
        ));
    }

    #[test]
    fn classifies_push_service_responses_and_retry_after() {
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000);
        assert_eq!(
            classify_response(StatusCode::CREATED, None, now),
            DeliveryOutcome::Success
        );
        assert_eq!(
            classify_response(StatusCode::TOO_MANY_REQUESTS, Some("30"), now),
            DeliveryOutcome::Retryable {
                status: Some(429),
                retry_after: Some(Duration::from_secs(30))
            }
        );
        let date = httpdate::fmt_http_date(now + Duration::from_secs(60));
        assert_eq!(
            classify_response(StatusCode::SERVICE_UNAVAILABLE, Some(&date), now),
            DeliveryOutcome::Retryable {
                status: Some(503),
                retry_after: Some(Duration::from_secs(60))
            }
        );
        assert_eq!(
            classify_response(StatusCode::TEMPORARY_REDIRECT, None, now),
            DeliveryOutcome::PermanentFailure { status: 307 }
        );
        assert_eq!(
            classify_response(StatusCode::GONE, None, now),
            DeliveryOutcome::PermanentFailure { status: 410 }
        );
    }
}

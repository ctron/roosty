use std::{
    borrow::Cow, collections::HashMap, fmt, future::Future, pin::Pin, str::FromStr, sync::Arc,
};

use axum::{
    Form, Json, Router,
    extract::{FromRequest, Request, State},
    http::{StatusCode, header},
    response::{IntoResponse, Response},
    routing::get,
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use rand_core::{OsRng, RngCore};
use ring::{
    aead::{AES_256_GCM, Aad, LessSafeKey, Nonce, UnboundKey},
    hmac as ring_hmac,
};
use roosty_core::{Result, RoostyError};
use roosty_db::{
    LocalNotificationType, PushAlerts, PushPolicy, PushSubscription, PushSubscriptionEncoding,
};
use roosty_web_push::{
    Client, DeliveryOutcome, Encoding, SendOptions, Subscription, VapidIdentity,
};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use thiserror::Error;
use url::Url;
use uuid::Uuid;

use crate::{auth::AuthenticatedAccessToken, http::AppState};

const ALERT_PREFIX: &str = "data[alerts][";
const TOKEN_ENCRYPTION_CONTEXT: &[u8] = b"roosty/web-push/access-token/v1";

/// Application Web Push component with no dependency on aggregate HTTP state.
#[derive(Clone)]
pub struct PushService {
    db: roosty_db::DbConnection,
    client: Option<Arc<dyn PushSender>>,
    token_key: [u8; 32],
    public_base_url: Url,
}

impl PushService {
    pub fn new(config: &crate::config::Config, db: roosty_db::DbConnection) -> Self {
        let derivation_key =
            ring_hmac::Key::new(ring_hmac::HMAC_SHA256, config.session_secret.as_bytes());
        let derived = ring_hmac::sign(&derivation_key, TOKEN_ENCRYPTION_CONTEXT);
        let mut token_key = [0_u8; 32];
        token_key.copy_from_slice(derived.as_ref());
        let subject = config.public_base_url.origin().ascii_serialization();
        let client = config
            .vapid_private_key
            .as_deref()
            .and_then(|key| VapidIdentity::from_base64_pkcs8(key, subject).ok())
            .map(|identity| Arc::new(Client::new(identity)) as Arc<dyn PushSender>);
        Self {
            db,
            client,
            token_key,
            public_base_url: config.public_base_url.clone(),
        }
    }

    #[cfg(test)]
    pub(crate) fn with_sender(
        config: &crate::config::Config,
        db: roosty_db::DbConnection,
        sender: Arc<dyn PushSender>,
    ) -> Self {
        let mut service = Self::new(config, db);
        service.client = Some(sender);
        service
    }

    fn client(&self) -> Option<&dyn PushSender> {
        self.client.as_deref()
    }

    pub fn public_key(&self) -> Option<String> {
        self.client.as_ref().map(|client| client.public_key())
    }

    fn token_cipher(&self) -> Result<LessSafeKey> {
        let key = UnboundKey::new(&AES_256_GCM, &self.token_key)
            .map_err(|_| RoostyError::Configuration("push token key is invalid".to_owned()))?;
        Ok(LessSafeKey::new(key))
    }

    fn encrypt_access_token(&self, token_id: Uuid, token: &str) -> Result<(Vec<u8>, Vec<u8>)> {
        let mut nonce = [0_u8; 12];
        OsRng.fill_bytes(&mut nonce);
        let mut ciphertext = token.as_bytes().to_vec();
        self.token_cipher()?
            .seal_in_place_append_tag(
                Nonce::assume_unique_for_key(nonce),
                Aad::from(token_id.as_bytes()),
                &mut ciphertext,
            )
            .map_err(|_| {
                RoostyError::Configuration("failed to encrypt push access token".to_owned())
            })?;
        Ok((nonce.to_vec(), ciphertext))
    }

    fn decrypt_access_token(&self, subscription: &PushSubscription) -> Result<String> {
        let nonce: [u8; 12] = subscription
            .access_token_nonce
            .as_slice()
            .try_into()
            .map_err(|_| {
                RoostyError::InvalidInput("stored push token nonce is invalid".to_owned())
            })?;
        let mut ciphertext = subscription.access_token_ciphertext.clone();
        let plaintext = self
            .token_cipher()?
            .open_in_place(
                Nonce::assume_unique_for_key(nonce),
                Aad::from(subscription.access_token_id.as_bytes()),
                &mut ciphertext,
            )
            .map_err(|_| {
                RoostyError::InvalidInput("stored push access token cannot be decrypted".to_owned())
            })?;
        String::from_utf8(plaintext.to_vec()).map_err(|_| {
            RoostyError::InvalidInput("stored push access token is not UTF-8".to_owned())
        })
    }

    /// Execute one durable push delivery job.
    pub async fn deliver(&self, payload: Value) -> std::result::Result<(), PushDeliveryError> {
        let job: DeliveryJob = serde_json::from_value(payload)
            .map_err(|error| RoostyError::InvalidInput(format!("invalid Web Push job: {error}")))?;
        let Some((notification, subscription)) =
            roosty_db::push_delivery(&self.db, job.notification_id, job.subscription_id).await?
        else {
            return Ok(());
        };
        if !subscription.alerts.enabled(notification.notification_type)
            || !roosty_db::push_policy_allows(&self.db, &notification, subscription.policy).await?
        {
            return Ok(());
        }
        let Some(client) = self.client() else {
            return Ok(());
        };
        let access_token = match self.decrypt_access_token(&subscription) {
            Ok(token) => token,
            Err(_) => {
                roosty_db::delete_push_subscription_by_id(&self.db, subscription.id).await?;
                return Ok(());
            }
        };
        let payload = crate::notifications::push_payload(
            &self.db,
            &self.public_base_url,
            notification,
            access_token,
        )
        .await?;
        let endpoint = Url::from_str(&subscription.endpoint)
            .map_err(|_| RoostyError::InvalidInput("stored push endpoint is invalid".to_owned()))?;
        let encoding = match subscription.encoding {
            PushSubscriptionEncoding::Standard => Encoding::Aes128Gcm,
            PushSubscriptionEncoding::Legacy => Encoding::AesGcm,
        };
        let web_subscription =
            match Subscription::new(endpoint, &subscription.p256dh, &subscription.auth, encoding) {
                Ok(subscription) => subscription,
                Err(_) => {
                    roosty_db::delete_push_subscription_by_id(&self.db, subscription.id).await?;
                    return Ok(());
                }
            };
        let serialized = serde_json::to_vec(&payload)
            .map_err(|error| RoostyError::InvalidInput(error.to_string()))?;
        match client
            .send(&web_subscription, &serialized, SendOptions::default())
            .await
        {
            Ok(DeliveryOutcome::Success) => Ok(()),
            Ok(DeliveryOutcome::PermanentFailure { .. }) => {
                roosty_db::delete_push_subscription_by_id(&self.db, subscription.id).await?;
                Ok(())
            }
            Ok(DeliveryOutcome::Retryable { status, .. }) => {
                Err(PushDeliveryError::Retryable { status })
            }
            Err(roosty_web_push::WebPushError::UnsafeEndpoint) => {
                roosty_db::delete_push_subscription_by_id(&self.db, subscription.id).await?;
                Ok(())
            }
            Err(error) => Err(error.into()),
        }
    }
}

impl fmt::Debug for PushService {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PushService")
            .field("db", &self.db)
            .field("configured", &self.client.is_some())
            .field("public_base_url", &self.public_base_url)
            .finish_non_exhaustive()
    }
}

pub(crate) trait PushSender: Send + Sync {
    fn send<'a>(
        &'a self,
        subscription: &'a Subscription,
        payload: &'a [u8],
        options: SendOptions,
    ) -> Pin<
        Box<
            dyn Future<Output = std::result::Result<DeliveryOutcome, roosty_web_push::WebPushError>>
                + Send
                + 'a,
        >,
    >;

    fn public_key(&self) -> String;
}

impl PushSender for Client {
    fn send<'a>(
        &'a self,
        subscription: &'a Subscription,
        payload: &'a [u8],
        options: SendOptions,
    ) -> Pin<
        Box<
            dyn Future<Output = std::result::Result<DeliveryOutcome, roosty_web_push::WebPushError>>
                + Send
                + 'a,
        >,
    > {
        Box::pin(Client::send(self, subscription, payload, options))
    }

    fn public_key(&self) -> String {
        self.vapid_public_key()
    }
}

/// Typed outcome that tells the durable worker whether a push should be retried.
#[derive(Debug, Error)]
pub enum PushDeliveryError {
    #[error(transparent)]
    Storage(#[from] RoostyError),
    #[error(transparent)]
    Protocol(#[from] roosty_web_push::WebPushError),
    #[error("retryable Web Push delivery failure with status {status:?}")]
    Retryable { status: Option<u16> },
}

/// Mastodon-compatible Web Push subscription routes.
pub fn router() -> Router<AppState> {
    Router::new().route(
        "/api/v1/push/subscription",
        get(get_subscription)
            .post(create_subscription)
            .put(update_subscription)
            .delete(delete_subscription),
    )
}

#[derive(Serialize)]
struct SubscriptionResponse {
    id: String,
    endpoint: String,
    standard: bool,
    alerts: PushAlerts,
    server_key: String,
    policy: PushPolicy,
}

#[derive(Serialize)]
struct ErrorResponse {
    error: String,
}

/// Mastodon API failures rendered uniformly at the HTTP boundary.
#[derive(Debug, Error)]
enum PushApiError {
    #[error("Record not found")]
    NotFound,
    #[error("{0}")]
    InvalidInput(Cow<'static, str>),
    #[error("push subscriptions require JSON or form-encoded data")]
    UnsupportedMediaType,
    #[error("This action requires the push OAuth scope")]
    InsufficientScope,
    #[error(transparent)]
    Database(#[from] RoostyError),
    #[error(transparent)]
    Protocol(#[from] roosty_web_push::WebPushError),
}

impl IntoResponse for PushApiError {
    fn into_response(self) -> Response {
        let status = match self {
            Self::NotFound => StatusCode::NOT_FOUND,
            Self::InvalidInput(_) | Self::Protocol(_) => StatusCode::UNPROCESSABLE_ENTITY,
            Self::UnsupportedMediaType => StatusCode::UNSUPPORTED_MEDIA_TYPE,
            Self::InsufficientScope => StatusCode::FORBIDDEN,
            Self::Database(_) => StatusCode::INTERNAL_SERVER_ERROR,
        };
        (
            status,
            Json(ErrorResponse {
                error: self.to_string(),
            }),
        )
            .into_response()
    }
}

#[derive(Deserialize)]
struct DeliveryJob {
    notification_id: Uuid,
    subscription_id: Uuid,
}

/// Request body accepted from both JSON clients such as Elk and form clients such as Tusky.
enum PushRequest<T> {
    Json(T),
    Form(HashMap<String, String>),
}

impl<S, T> FromRequest<S> for PushRequest<T>
where
    S: Send + Sync,
    T: DeserializeOwned,
{
    type Rejection = PushApiError;

    async fn from_request(request: Request, state: &S) -> Result<Self, Self::Rejection> {
        let content_type = request
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default();
        if content_type.starts_with("application/json") {
            return Json::<T>::from_request(request, state)
                .await
                .map(|Json(value)| Self::Json(value))
                .map_err(|error| PushApiError::InvalidInput(error.to_string().into()));
        }
        if content_type.starts_with("application/x-www-form-urlencoded") {
            return Form::<HashMap<String, String>>::from_request(request, state)
                .await
                .map(|Form(fields)| Self::Form(fields))
                .map_err(|error| PushApiError::InvalidInput(error.to_string().into()));
        }
        Err(PushApiError::UnsupportedMediaType)
    }
}

#[derive(Deserialize)]
struct CreateSubscriptionJson {
    subscription: SubscriptionJson,
    #[serde(default)]
    data: PushDataJson,
    #[serde(default)]
    policy: Option<PushPolicy>,
}

#[derive(Deserialize)]
struct SubscriptionJson {
    endpoint: String,
    keys: SubscriptionKeysJson,
    #[serde(default)]
    standard: bool,
}

#[derive(Deserialize)]
struct SubscriptionKeysJson {
    p256dh: String,
    auth: String,
}

#[derive(Default, Deserialize)]
struct PushDataJson {
    #[serde(default)]
    alerts: PushAlertChanges,
    #[serde(default)]
    policy: Option<PushPolicy>,
}

#[derive(Default, Deserialize)]
struct PushAlertChanges {
    mention: Option<bool>,
    favourite: Option<bool>,
    follow: Option<bool>,
    follow_request: Option<bool>,
    reblog: Option<bool>,
    status: Option<bool>,
    update: Option<bool>,
    quote: Option<bool>,
    quoted_update: Option<bool>,
}

impl PushAlertChanges {
    fn apply(self, alerts: &mut PushAlerts) {
        macro_rules! apply {
            ($field:ident) => {
                if let Some(enabled) = self.$field {
                    alerts.$field = enabled;
                }
            };
        }
        apply!(mention);
        apply!(favourite);
        apply!(follow);
        apply!(follow_request);
        apply!(reblog);
        apply!(status);
        apply!(update);
        apply!(quote);
        apply!(quoted_update);
    }
}

#[derive(Deserialize)]
struct UpdateSubscriptionJson {
    #[serde(default)]
    data: PushDataJson,
}

struct CreateSubscriptionInput {
    endpoint: Url,
    p256dh: Vec<u8>,
    auth: Vec<u8>,
    encoding: PushSubscriptionEncoding,
    policy: PushPolicy,
    alerts: PushAlerts,
}

async fn get_subscription(
    State(state): State<AppState>,
    token: AuthenticatedAccessToken,
) -> std::result::Result<Json<SubscriptionResponse>, PushApiError> {
    require_push_scope(&token)?;
    let client = state.push.client().ok_or(PushApiError::NotFound)?;
    let subscription =
        roosty_db::push_subscription_for_access_token(&state.push.db, token.grant.id)
            .await?
            .ok_or(PushApiError::NotFound)?;
    Ok(Json(subscription_response(subscription, client)))
}

async fn create_subscription(
    State(state): State<AppState>,
    token: AuthenticatedAccessToken,
    request: PushRequest<CreateSubscriptionJson>,
) -> std::result::Result<Json<SubscriptionResponse>, PushApiError> {
    require_push_scope(&token)?;
    let client = state.push.client().ok_or(PushApiError::NotFound)?;
    let input = create_subscription_input(request)?;
    let CreateSubscriptionInput {
        endpoint,
        p256dh,
        auth,
        encoding,
        policy,
        alerts,
    } = input;
    let wire_encoding = match encoding {
        PushSubscriptionEncoding::Standard => Encoding::Aes128Gcm,
        PushSubscriptionEncoding::Legacy => Encoding::AesGcm,
    };
    Subscription::new(endpoint.clone(), &p256dh, &auth, wire_encoding)?;
    roosty_web_push::validate_endpoint(&endpoint).await?;
    let (access_token_nonce, access_token_ciphertext) = state
        .push
        .encrypt_access_token(token.grant.id, &token.raw_token)?;
    let input = roosty_db::NewPushSubscription {
        access_token_id: token.grant.id,
        account_id: token.grant.account.id,
        endpoint: endpoint.to_string(),
        p256dh,
        auth,
        encoding,
        policy,
        alerts,
        access_token_ciphertext,
        access_token_nonce,
    };
    let subscription = roosty_db::upsert_push_subscription(&state.push.db, input).await?;
    Ok(Json(subscription_response(subscription, client)))
}

async fn update_subscription(
    State(state): State<AppState>,
    token: AuthenticatedAccessToken,
    request: PushRequest<UpdateSubscriptionJson>,
) -> std::result::Result<Json<SubscriptionResponse>, PushApiError> {
    require_push_scope(&token)?;
    let client = state.push.client().ok_or(PushApiError::NotFound)?;
    let existing = roosty_db::push_subscription_for_access_token(&state.push.db, token.grant.id)
        .await?
        .ok_or(PushApiError::NotFound)?;
    let (alerts, policy) = match request {
        PushRequest::Json(request) => {
            let mut alerts = existing.alerts;
            request.data.alerts.apply(&mut alerts);
            (alerts, request.data.policy.unwrap_or(existing.policy))
        }
        PushRequest::Form(fields) => (
            alerts_with_defaults(&fields, existing.alerts)?,
            optional_policy(&fields)?.unwrap_or(existing.policy),
        ),
    };
    let subscription =
        roosty_db::update_push_subscription(&state.push.db, token.grant.id, alerts, policy)
            .await?
            .ok_or(PushApiError::NotFound)?;
    Ok(Json(subscription_response(subscription, client)))
}

async fn delete_subscription(
    State(state): State<AppState>,
    token: AuthenticatedAccessToken,
) -> std::result::Result<Json<Value>, PushApiError> {
    require_push_scope(&token)?;
    roosty_db::delete_push_subscription(&state.push.db, token.grant.id).await?;
    Ok(Json(json!({})))
}

fn require_push_scope(token: &AuthenticatedAccessToken) -> std::result::Result<(), PushApiError> {
    token
        .grant
        .scopes
        .split_whitespace()
        .any(|scope| scope == "push")
        .then_some(())
        .ok_or(PushApiError::InsufficientScope)
}

fn subscription_response(
    subscription: PushSubscription,
    client: &dyn PushSender,
) -> SubscriptionResponse {
    SubscriptionResponse {
        id: subscription.id.to_string(),
        endpoint: subscription.endpoint,
        standard: subscription.encoding == PushSubscriptionEncoding::Standard,
        alerts: subscription.alerts,
        server_key: client.public_key(),
        policy: subscription.policy,
    }
}

fn create_subscription_input(
    request: PushRequest<CreateSubscriptionJson>,
) -> std::result::Result<CreateSubscriptionInput, PushApiError> {
    match request {
        PushRequest::Json(request) => {
            let endpoint = parse_endpoint(&request.subscription.endpoint)?;
            let p256dh = decode_key_value(&request.subscription.keys.p256dh, 65)?;
            let auth = decode_key_value(&request.subscription.keys.auth, 16)?;
            let encoding = if request.subscription.standard {
                PushSubscriptionEncoding::Standard
            } else {
                PushSubscriptionEncoding::Legacy
            };
            let mut alerts = PushAlerts::default();
            request.data.alerts.apply(&mut alerts);
            Ok(CreateSubscriptionInput {
                endpoint,
                p256dh,
                auth,
                encoding,
                policy: request.data.policy.or(request.policy).unwrap_or_default(),
                alerts,
            })
        }
        PushRequest::Form(fields) => {
            let endpoint = parse_endpoint(required_field(&fields, "subscription[endpoint]")?)?;
            let p256dh = decode_key(&fields, "subscription[keys][p256dh]", 65)?;
            let auth = decode_key(&fields, "subscription[keys][auth]", 16)?;
            let encoding = if boolean_field(&fields, "subscription[standard]")?.unwrap_or(false) {
                PushSubscriptionEncoding::Standard
            } else {
                PushSubscriptionEncoding::Legacy
            };
            Ok(CreateSubscriptionInput {
                endpoint,
                p256dh,
                auth,
                encoding,
                policy: policy(&fields)?,
                alerts: alerts(&fields)?,
            })
        }
    }
}

fn parse_endpoint(value: &str) -> std::result::Result<Url, PushApiError> {
    Url::parse(value)
        .map_err(|_| PushApiError::InvalidInput("subscription endpoint is invalid".into()))
}

fn required_field<'a>(
    fields: &'a HashMap<String, String>,
    name: &'static str,
) -> std::result::Result<&'a str, PushApiError> {
    fields
        .get(name)
        .map(String::as_str)
        .filter(|value| !value.is_empty())
        .ok_or(PushApiError::InvalidInput(
            "required subscription field is missing".into(),
        ))
}

fn decode_key(
    fields: &HashMap<String, String>,
    name: &'static str,
    length: usize,
) -> std::result::Result<Vec<u8>, PushApiError> {
    let value = required_field(fields, name)?;
    decode_key_value(value, length)
}

fn decode_key_value(value: &str, length: usize) -> std::result::Result<Vec<u8>, PushApiError> {
    let decoded = URL_SAFE_NO_PAD.decode(value).map_err(|_| {
        PushApiError::InvalidInput("subscription key is not valid base64url".into())
    })?;
    if decoded.len() != length {
        return Err(PushApiError::InvalidInput(
            "subscription key has an invalid length".into(),
        ));
    }
    Ok(decoded)
}

fn policy(fields: &HashMap<String, String>) -> std::result::Result<PushPolicy, PushApiError> {
    let value = fields
        .get("data[policy]")
        .map(String::as_str)
        .unwrap_or("all");
    PushPolicy::from_str(value)
        .map_err(|_| PushApiError::InvalidInput("push policy is invalid".into()))
}

fn optional_policy(
    fields: &HashMap<String, String>,
) -> std::result::Result<Option<PushPolicy>, PushApiError> {
    fields
        .get("data[policy]")
        .map(|value| {
            PushPolicy::from_str(value)
                .map_err(|_| PushApiError::InvalidInput("push policy is invalid".into()))
        })
        .transpose()
}

fn boolean_field(
    fields: &HashMap<String, String>,
    name: &str,
) -> std::result::Result<Option<bool>, PushApiError> {
    fields
        .get(name)
        .map(|value| match value.as_str() {
            "true" | "1" => Ok(true),
            "false" | "0" => Ok(false),
            _ => Err(PushApiError::InvalidInput(
                "push boolean field is invalid".into(),
            )),
        })
        .transpose()
}

fn alerts(fields: &HashMap<String, String>) -> std::result::Result<PushAlerts, PushApiError> {
    alerts_with_defaults(fields, PushAlerts::default())
}

fn alerts_with_defaults(
    fields: &HashMap<String, String>,
    mut alerts: PushAlerts,
) -> std::result::Result<PushAlerts, PushApiError> {
    for (key, value) in fields {
        let Some(name) = key
            .strip_prefix(ALERT_PREFIX)
            .and_then(|key| key.strip_suffix(']'))
        else {
            continue;
        };
        let enabled = match value.as_str() {
            "true" | "1" => true,
            "false" | "0" => false,
            _ => {
                return Err(PushApiError::InvalidInput(
                    "push boolean field is invalid".into(),
                ));
            }
        };
        let Ok(notification_type) = LocalNotificationType::from_str(name) else {
            continue;
        };
        match notification_type {
            LocalNotificationType::Mention => alerts.mention = enabled,
            LocalNotificationType::Favourite => alerts.favourite = enabled,
            LocalNotificationType::Follow => alerts.follow = enabled,
            LocalNotificationType::FollowRequest => alerts.follow_request = enabled,
            LocalNotificationType::Reblog => alerts.reblog = enabled,
            LocalNotificationType::Status => alerts.status = enabled,
            LocalNotificationType::Update => alerts.update = enabled,
            LocalNotificationType::Quote => alerts.quote = enabled,
            LocalNotificationType::QuotedUpdate => alerts.quoted_update = enabled,
        }
    }
    Ok(alerts)
}

#[cfg(test)]
mod tests {
    use super::{
        PushApiError, PushDeliveryError, PushService, alerts, alerts_with_defaults, decode_key,
        optional_policy, policy,
    };
    use axum::response::IntoResponse;
    use roosty_core::RoostyError;
    use roosty_db::{PushAlerts, PushPolicy, PushSubscription, PushSubscriptionEncoding};
    use sea_orm::DatabaseConnection;
    use std::collections::HashMap;
    use url::Url;
    use uuid::Uuid;

    #[test]
    fn form_alerts_are_converted_to_the_closed_type() {
        let fields = HashMap::from([
            ("data[alerts][mention]".to_owned(), "true".to_owned()),
            ("data[alerts][follow]".to_owned(), "false".to_owned()),
            ("data[alerts][unknown]".to_owned(), "true".to_owned()),
        ]);
        let parsed = alerts(&fields).unwrap();
        assert!(parsed.mention);
        assert!(!parsed.follow);
        assert_eq!(
            parsed,
            PushAlerts {
                mention: true,
                ..PushAlerts::default()
            }
        );
    }

    #[test]
    fn update_fields_preserve_omitted_typed_settings() {
        let existing = PushAlerts {
            mention: true,
            follow: true,
            ..PushAlerts::default()
        };
        let fields = HashMap::from([("data[alerts][mention]".to_owned(), "false".to_owned())]);
        let updated = alerts_with_defaults(&fields, existing).unwrap();
        assert!(!updated.mention);
        assert!(updated.follow);
        assert_eq!(optional_policy(&fields).ok().flatten(), None);
        let policy_fields = HashMap::from([("data[policy]".to_owned(), "followed".to_owned())]);
        assert_eq!(
            optional_policy(&policy_fields).ok().flatten(),
            Some(PushPolicy::Followed)
        );
    }

    #[test]
    fn typed_api_errors_select_the_http_status() {
        assert_eq!(
            PushApiError::NotFound.into_response().status(),
            axum::http::StatusCode::NOT_FOUND
        );
        assert_eq!(
            PushApiError::InvalidInput("bad subscription".into())
                .into_response()
                .status(),
            axum::http::StatusCode::UNPROCESSABLE_ENTITY
        );
        assert_eq!(
            PushApiError::InsufficientScope.into_response().status(),
            axum::http::StatusCode::FORBIDDEN
        );
    }

    #[test]
    fn malformed_boolean_fields_are_rejected() {
        let fields = HashMap::from([("data[alerts][mention]".to_owned(), "definitely".to_owned())]);
        assert!(matches!(
            alerts(&fields),
            Err(PushApiError::InvalidInput(_))
        ));
    }

    #[test]
    fn invalid_policy_and_key_encodings_are_typed_errors() {
        let policy_fields = HashMap::from([("data[policy]".to_owned(), "friends".to_owned())]);
        assert!(matches!(
            policy(&policy_fields),
            Err(PushApiError::InvalidInput(_))
        ));
        let malformed = HashMap::from([("key".to_owned(), "not base64".to_owned())]);
        assert!(matches!(
            decode_key(&malformed, "key", 16),
            Err(PushApiError::InvalidInput(_))
        ));
        let short = HashMap::from([("key".to_owned(), "AQID".to_owned())]);
        assert!(matches!(
            decode_key(&short, "key", 16),
            Err(PushApiError::InvalidInput(_))
        ));
    }

    #[test]
    fn access_token_encryption_authenticates_ciphertext_nonce_and_token_id() {
        let service = PushService {
            db: DatabaseConnection::Disconnected,
            client: None,
            token_key: [7_u8; 32],
            public_base_url: Url::parse("https://social.example").unwrap(),
        };
        let token_id = Uuid::now_v7();
        let (nonce, ciphertext) = service
            .encrypt_access_token(token_id, "bearer-token")
            .unwrap();
        let subscription =
            |access_token_id, nonce: Vec<u8>, ciphertext: Vec<u8>| PushSubscription {
                id: Uuid::now_v7(),
                access_token_id,
                account_id: roosty_core::AccountId(Uuid::now_v7()),
                endpoint: "https://1.1.1.1/push".to_owned(),
                p256dh: vec![0; 65],
                auth: vec![0; 16],
                encoding: PushSubscriptionEncoding::Standard,
                policy: PushPolicy::All,
                alerts: PushAlerts::default(),
                access_token_ciphertext: ciphertext,
                access_token_nonce: nonce,
            };
        let valid = subscription(token_id, nonce.clone(), ciphertext.clone());
        assert_eq!(
            service.decrypt_access_token(&valid).unwrap(),
            "bearer-token"
        );

        let wrong_id = subscription(Uuid::now_v7(), nonce.clone(), ciphertext.clone());
        assert!(matches!(
            service.decrypt_access_token(&wrong_id),
            Err(RoostyError::InvalidInput(_))
        ));
        let invalid_nonce = subscription(token_id, vec![0; 11], ciphertext.clone());
        assert!(matches!(
            service.decrypt_access_token(&invalid_nonce),
            Err(RoostyError::InvalidInput(_))
        ));
        let mut corrupted = ciphertext;
        corrupted[0] ^= 1;
        let corrupted = subscription(token_id, nonce, corrupted);
        assert!(matches!(
            service.decrypt_access_token(&corrupted),
            Err(RoostyError::InvalidInput(_))
        ));
    }

    #[tokio::test]
    async fn malformed_delivery_jobs_return_a_typed_error() {
        let service = PushService {
            db: DatabaseConnection::Disconnected,
            client: None,
            token_key: [7_u8; 32],
            public_base_url: Url::parse("https://social.example").unwrap(),
        };
        assert!(matches!(
            service
                .deliver(serde_json::json!({ "notification_id": "invalid" }))
                .await,
            Err(PushDeliveryError::Storage(RoostyError::InvalidInput(_)))
        ));
    }
}

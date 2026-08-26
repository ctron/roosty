//! Mastodon-compatible public account registration.

use std::{borrow::Cow, collections::BTreeMap};

use axum::{
    Extension, Json, Router,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::post,
};
use roosty_core::{AccountId, RoostyError};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{
    account_validation,
    auth::{FormOrJson, TokenResponse, bearer_token},
    config::RegistrationMode,
    http::{AppState, DatabaseContext},
    password,
};

pub(crate) fn router() -> Router<AppState> {
    Router::new().route("/api/v1/accounts", post(register_account))
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct RegistrationRequest {
    username: String,
    email: String,
    password: String,
    agreement: bool,
    locale: String,
    reason: Option<String>,
    date_of_birth: Option<String>,
}

#[derive(Debug, Error)]
enum RegistrationError {
    #[error("The access token is invalid")]
    InvalidToken,
    #[error("This action is not allowed")]
    RegistrationsClosed,
    #[error("This action is outside the authorized scopes")]
    InsufficientScope,
    #[error("registration input is invalid")]
    Validation(ValidationDetails),
    #[error(transparent)]
    Internal(RoostyError),
}

impl From<sea_orm::DbErr> for RegistrationError {
    fn from(error: sea_orm::DbErr) -> Self {
        Self::Internal(error.into())
    }
}

impl From<RoostyError> for RegistrationError {
    fn from(error: RoostyError) -> Self {
        Self::Internal(error)
    }
}

impl IntoResponse for RegistrationError {
    fn into_response(self) -> Response {
        match self {
            Self::InvalidToken => (
                StatusCode::UNAUTHORIZED,
                Json(serde_json::json!({ "error": "The access token is invalid" })),
            )
                .into_response(),
            Self::RegistrationsClosed => (
                StatusCode::FORBIDDEN,
                Json(serde_json::json!({ "error": "This action is not allowed" })),
            )
                .into_response(),
            Self::InsufficientScope => (
                StatusCode::FORBIDDEN,
                Json(serde_json::json!({
                    "error": "This action is outside the authorized scopes"
                })),
            )
                .into_response(),
            Self::Validation(details) => {
                let summary = details
                    .values()
                    .flatten()
                    .map(|error| error.description.as_ref())
                    .collect::<Vec<_>>()
                    .join(", ");
                (
                    StatusCode::UNPROCESSABLE_ENTITY,
                    Json(serde_json::json!({
                        "error": format!("Validation failed: {summary}"),
                        "details": details,
                    })),
                )
                    .into_response()
            }
            Self::Internal(error) => {
                tracing::error!(%error, "account registration failed");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(serde_json::json!({ "error": "Internal server error" })),
                )
                    .into_response()
            }
        }
    }
}

type ValidationDetails = BTreeMap<&'static str, Vec<RegistrationFieldError>>;

#[derive(Debug, Serialize)]
struct RegistrationFieldError {
    error: &'static str,
    description: Cow<'static, str>,
}

async fn register_account(
    State(state): State<AppState>,
    Extension(database): Extension<DatabaseContext>,
    headers: HeaderMap,
    FormOrJson(request): FormOrJson<RegistrationRequest>,
) -> Result<Response, RegistrationError> {
    let raw_token = bearer_token(&headers).ok_or(RegistrationError::InvalidToken)?;
    let txn = database.begin_read().await?;
    let grant =
        roosty_db::find_application_access_token_grant(&txn, &state.config.token_pepper, raw_token)
            .await?
            .ok_or(RegistrationError::InvalidToken)?;
    txn.commit().await?;

    if !grant.scopes.split_whitespace().any(write_accounts_scope) {
        return Err(RegistrationError::InsufficientScope);
    }
    if state.config.registration_mode != RegistrationMode::Open {
        return Err(RegistrationError::RegistrationsClosed);
    }

    let details = validate(&request);
    if !details.is_empty() {
        return Err(RegistrationError::Validation(details));
    }
    let password_hash =
        password::hash_password(&request.password).map_err(RegistrationError::Internal)?;

    let txn = database.begin_write().await?;
    let account_id = match roosty_db::create_local_account_in_transaction(
        &txn,
        &request.username,
        &request.email,
        &password_hash,
    )
    .await
    {
        Ok(account_id) => account_id,
        Err(RoostyError::InvalidInput(reason)) => {
            return Err(RegistrationError::Validation(taken_details(&reason)));
        }
        Err(error) => return Err(RegistrationError::Internal(error)),
    };
    let token = roosty_db::create_access_token(
        &txn,
        &state.config.token_pepper,
        AccountId(account_id),
        grant.application.id,
        &grant.application.scopes,
    )
    .await?;
    txn.commit().await?;

    Ok(Json(TokenResponse::from(token)).into_response())
}

fn write_accounts_scope(scope: &str) -> bool {
    matches!(scope, "write" | "write:accounts")
}

fn validate(request: &RegistrationRequest) -> ValidationDetails {
    let mut details = ValidationDetails::new();
    if request.username.is_empty() {
        field_error(
            &mut details,
            "username",
            "ERR_BLANK",
            "can't be blank".into(),
        );
    } else if let Err(reason) = account_validation::username(&request.username) {
        field_error(&mut details, "username", "ERR_INVALID", reason);
    }
    if request.email.is_empty() {
        field_error(&mut details, "email", "ERR_BLANK", "can't be blank".into());
    } else if let Err(reason) = account_validation::email(&request.email) {
        field_error(&mut details, "email", "ERR_INVALID", reason);
    }
    if request.password.is_empty() {
        field_error(
            &mut details,
            "password",
            "ERR_BLANK",
            "can't be blank".into(),
        );
    } else if request.password.chars().count() < 8 {
        field_error(
            &mut details,
            "password",
            "ERR_TOO_SHORT",
            "must be at least 8 characters".into(),
        );
    }
    if !request.agreement {
        field_error(
            &mut details,
            "agreement",
            "ERR_ACCEPTED",
            "must be accepted".into(),
        );
    }
    if request.locale.trim().is_empty() {
        field_error(&mut details, "locale", "ERR_BLANK", "can't be blank".into());
    }
    let _ = (&request.reason, &request.date_of_birth);
    details
}

fn taken_details(reason: &str) -> ValidationDetails {
    let mut details = ValidationDetails::new();
    let field = if reason.contains("email") {
        "email"
    } else {
        "username"
    };
    field_error(
        &mut details,
        field,
        "ERR_TAKEN",
        "has already been taken".into(),
    );
    details
}

fn field_error(
    details: &mut ValidationDetails,
    field: &'static str,
    error: &'static str,
    description: Cow<'static, str>,
) {
    details
        .entry(field)
        .or_default()
        .push(RegistrationFieldError { error, description });
}

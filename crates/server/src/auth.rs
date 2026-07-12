use std::{fmt, path::Path};

use askama::Template;
use axum::{
    Form, Json, Router,
    body::to_bytes,
    extract::{FromRef, FromRequest, FromRequestParts, Query, Request, State},
    http::{HeaderMap, StatusCode, header, request::Parts},
    response::{Html, IntoResponse, Redirect, Response},
    routing::{get, patch, post},
};
use axum_params::{Params, UploadFile};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use hmac::{Hmac, Mac};
use roosty_core::{AccountId, RoostyError};
use serde::{
    Deserialize, Serialize,
    de::{self, DeserializeOwned, MapAccess, Visitor},
};
use serde_json::{Value, json};
use sha2::Sha256;
use time::{Duration, OffsetDateTime};
use url::form_urlencoded;
use uuid::Uuid;

use crate::{http::AppState, password};

type HmacSha256 = Hmac<Sha256>;

const SESSION_COOKIE: &str = "roosty_session";

/// Authenticated local account extracted from an OAuth bearer token.
pub(crate) struct AuthenticatedAccount(pub roosty_db::LocalAccount);

/// Optional local account extracted from an OAuth bearer token when present.
pub(crate) struct OptionalAuthenticatedAccount(pub Option<roosty_db::LocalAccount>);

impl<S> FromRequestParts<S> for AuthenticatedAccount
where
    AppState: FromRef<S>,
    S: Send + Sync,
{
    type Rejection = Response;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let state = AppState::from_ref(state);
        authenticated_account(&state, &parts.headers)
            .await
            .map(Self)
    }
}

impl<S> FromRequestParts<S> for OptionalAuthenticatedAccount
where
    AppState: FromRef<S>,
    S: Send + Sync,
{
    type Rejection = Response;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let state = AppState::from_ref(state);
        optional_authenticated_account(&state, &parts.headers)
            .await
            .map(Self)
    }
}

struct FormOrJson<T>(T);

impl<S, T> FromRequest<S> for FormOrJson<T>
where
    S: Send + Sync,
    T: DeserializeOwned,
{
    type Rejection = Response;

    async fn from_request(request: Request, _state: &S) -> Result<Self, Self::Rejection> {
        let content_type = request
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_owned();
        let body = to_bytes(request.into_body(), 1024 * 1024)
            .await
            .map_err(|error| {
                oauth_error(
                    StatusCode::BAD_REQUEST,
                    "invalid_request",
                    &format!("invalid request body: {error}"),
                )
            })?;

        let value = if content_type.contains("application/json") {
            serde_json::from_slice(&body).map_err(|error| error.to_string())
        } else {
            serde_urlencoded::from_bytes(&body).map_err(|error| error.to_string())
        }
        .map_err(|error| {
            oauth_error(
                StatusCode::BAD_REQUEST,
                "invalid_request",
                &format!("invalid request body: {error}"),
            )
        })?;

        Ok(Self(value))
    }
}

/// Build browser login, OAuth, and account verification routes.
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/login", get(login_form).post(login))
        .route("/logout", post(logout))
        .route("/api/v1/apps", post(register_app))
        .route("/oauth/authorize", get(authorize_form).post(authorize))
        .route("/oauth/token", post(token))
        .route("/oauth/revoke", post(revoke))
        .route(
            "/api/v1/accounts/verify_credentials",
            get(verify_credentials),
        )
        .route(
            "/api/v1/accounts/update_credentials",
            patch(update_credentials),
        )
        .route("/api/v1/preferences", get(preferences))
}

#[derive(Deserialize)]
struct LoginQuery {
    next: Option<String>,
}

#[derive(Deserialize)]
struct LoginForm {
    login: String,
    password: String,
    next: Option<String>,
}

#[derive(Template)]
#[template(
    source = r#"<!doctype html>
<html lang="en">
<head><meta charset="utf-8"><title>Sign in</title></head>
<body>
<main>
<h1>Sign in</h1>
{% if let Some(message) = error %}<p>{{ message }}</p>{% endif %}
<form method="post" action="{{ action }}">
<input type="hidden" name="next" value="{{ next }}">
<label>Username or email <input name="login" autocomplete="username" required></label>
<label>Password <input name="password" type="password" autocomplete="current-password" required></label>
<button type="submit">Sign in</button>
</form>
</main>
</body>
</html>"#,
    ext = "html"
)]
struct LoginTemplate<'a> {
    action: &'a str,
    next: &'a str,
    error: Option<&'a str>,
}

async fn login_form(State(state): State<AppState>, Query(query): Query<LoginQuery>) -> Response {
    render_login(&state, query.next.as_deref().unwrap_or("/"), None)
}

async fn login(State(state): State<AppState>, Form(form): Form<LoginForm>) -> Response {
    let next = sanitize_next(form.next.as_deref());
    let account = match roosty_db::find_local_account_by_login(&state.db, &form.login).await {
        Ok(Some(account)) => account,
        Ok(None) => return render_login(&state, &next, Some("Invalid username or password.")),
        Err(error) => return server_error(error),
    };

    match password::verify_password(&form.password, &account.password_hash) {
        Ok(true) => {
            let cookie = match session_cookie(&state, account.id) {
                Ok(cookie) => cookie,
                Err(error) => return server_error(error),
            };
            (
                [(header::SET_COOKIE, cookie)],
                Redirect::to(&public_url(&state, &next)),
            )
                .into_response()
        }
        Ok(false) => render_login(&state, &next, Some("Invalid username or password.")),
        Err(error) => server_error(error),
    }
}

async fn logout(State(state): State<AppState>) -> Response {
    (
        [(
            header::SET_COOKIE,
            format!("{SESSION_COOKIE}=; Path=/; Max-Age=0; HttpOnly; Secure; SameSite=Lax"),
        )],
        Redirect::to(&public_url(&state, "/login")),
    )
        .into_response()
}

fn render_login(state: &AppState, next: &str, error: Option<&str>) -> Response {
    let action = public_url(state, "/login");
    match (LoginTemplate {
        action: &action,
        next,
        error,
    })
    .render()
    {
        Ok(html) => Html(html).into_response(),
        Err(error) => server_error(RoostyError::InvalidInput(error.to_string())),
    }
}

#[derive(Deserialize)]
struct CreateAppForm {
    client_name: String,
    redirect_uris: String,
    scopes: Option<String>,
    website: Option<String>,
}

#[derive(Serialize)]
struct CreateAppResponse {
    id: String,
    name: String,
    website: Option<String>,
    redirect_uri: String,
    client_id: String,
    client_secret: String,
    vapid_key: String,
}

async fn register_app(
    State(state): State<AppState>,
    FormOrJson(form): FormOrJson<CreateAppForm>,
) -> Response {
    if form.client_name.trim().is_empty() || form.redirect_uris.trim().is_empty() {
        return oauth_error(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            "missing client metadata",
        );
    }

    let scopes = form.scopes.as_deref().unwrap_or("read write follow push");
    match roosty_db::create_oauth_application(
        &state.db,
        &form.client_name,
        &form.redirect_uris,
        scopes,
        form.website.as_deref(),
        &state.config.token_pepper,
    )
    .await
    {
        Ok((app, client_secret)) => Json(CreateAppResponse {
            id: app.id.to_string(),
            name: app.name,
            website: app.website,
            redirect_uri: app.redirect_uri,
            client_id: app.client_id,
            client_secret,
            vapid_key: String::new(),
        })
        .into_response(),
        Err(error) => server_error(error),
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct AuthorizeParams {
    response_type: String,
    client_id: String,
    redirect_uri: String,
    scope: Option<String>,
    state: Option<String>,
    code_challenge: Option<String>,
    code_challenge_method: Option<String>,
}

#[derive(Template)]
#[template(
    source = r#"<!doctype html>
<html lang="en">
<head><meta charset="utf-8"><title>Authorize</title></head>
<body>
<main>
<h1>Authorize {{ client_name }}</h1>
<p>This application is requesting access to your Roosty account.</p>
<form method="post" action="{{ action }}">
<input type="hidden" name="response_type" value="{{ params.response_type }}">
<input type="hidden" name="client_id" value="{{ params.client_id }}">
<input type="hidden" name="redirect_uri" value="{{ params.redirect_uri }}">
<input type="hidden" name="scope" value="{{ scope }}">
<input type="hidden" name="state" value="{{ state }}">
<input type="hidden" name="code_challenge" value="{{ code_challenge }}">
<input type="hidden" name="code_challenge_method" value="{{ code_challenge_method }}">
<button type="submit">Authorize</button>
</form>
</main>
</body>
</html>"#,
    ext = "html"
)]
struct AuthorizeTemplate<'a> {
    action: &'a str,
    client_name: &'a str,
    params: &'a AuthorizeParams,
    scope: &'a str,
    state: &'a str,
    code_challenge: &'a str,
    code_challenge_method: &'a str,
}

async fn authorize_form(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(params): Query<AuthorizeParams>,
) -> Response {
    let account_id = match account_id_from_session(&state, &headers) {
        Ok(Some(account_id)) => account_id,
        Ok(None) => {
            let next = format!("/oauth/authorize?{}", authorize_query_string(&params));
            return Redirect::to(&public_url(
                &state,
                &format!("/login?next={}", url_encode(&next)),
            ))
            .into_response();
        }
        Err(error) => return server_error(error),
    };

    if let Err(response) = validate_authorize_request(&state, &params).await {
        return response;
    }

    let app =
        match roosty_db::find_oauth_application_by_client_id(&state.db, &params.client_id).await {
            Ok(Some(app)) => app,
            Ok(None) => {
                return oauth_error(StatusCode::BAD_REQUEST, "invalid_client", "unknown client");
            }
            Err(error) => return server_error(error),
        };

    if roosty_db::find_local_account_by_id(&state.db, account_id)
        .await
        .ok()
        .flatten()
        .is_none()
    {
        return Redirect::to(&public_url(&state, "/login")).into_response();
    }

    let scope = params.scope.as_deref().unwrap_or(app.scopes.as_str());
    let state_value = params.state.as_deref().unwrap_or_default();
    let challenge = optional_non_empty(params.code_challenge.as_deref()).unwrap_or_default();
    let method = optional_non_empty(params.code_challenge_method.as_deref()).unwrap_or_default();
    let action = public_url(&state, "/oauth/authorize");
    match (AuthorizeTemplate {
        action: &action,
        client_name: &app.name,
        params: &params,
        scope,
        state: state_value,
        code_challenge: challenge,
        code_challenge_method: method,
    })
    .render()
    {
        Ok(html) => Html(html).into_response(),
        Err(error) => server_error(RoostyError::InvalidInput(error.to_string())),
    }
}

async fn authorize(
    State(state): State<AppState>,
    headers: HeaderMap,
    Form(params): Form<AuthorizeParams>,
) -> Response {
    let account_id = match account_id_from_session(&state, &headers) {
        Ok(Some(account_id)) => account_id,
        Ok(None) => return Redirect::to(&public_url(&state, "/login")).into_response(),
        Err(error) => return server_error(error),
    };

    let app = match validate_authorize_request(&state, &params).await {
        Ok(app) => app,
        Err(response) => return response,
    };

    let scope = params.scope.as_deref().unwrap_or(app.scopes.as_str());
    let challenge = optional_non_empty(params.code_challenge.as_deref()).unwrap_or_default();
    let method = optional_non_empty(params.code_challenge_method.as_deref()).unwrap_or_default();
    let code = match roosty_db::create_authorization_code(
        &state.db,
        &state.config.token_pepper,
        roosty_db::NewAuthorizationCode {
            account_id,
            application_id: app.id,
            redirect_uri: &params.redirect_uri,
            scopes: scope,
            code_challenge: challenge,
            code_challenge_method: method,
        },
    )
    .await
    {
        Ok(code) => code,
        Err(error) => return server_error(error),
    };

    let separator = if params.redirect_uri.contains('?') {
        '&'
    } else {
        '?'
    };
    let state_query = params
        .state
        .as_deref()
        .map(|state| format!("&state={}", url_encode(state)))
        .unwrap_or_default();
    Redirect::to(&format!(
        "{}{separator}code={}{}",
        params.redirect_uri,
        url_encode(&code),
        state_query
    ))
    .into_response()
}

async fn validate_authorize_request(
    state: &AppState,
    params: &AuthorizeParams,
) -> Result<roosty_db::OAuthApplication, Response> {
    if params.response_type != "code" {
        return Err(oauth_error(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            "unsupported authorization request",
        ));
    }
    let challenge = optional_non_empty(params.code_challenge.as_deref());
    let method = optional_non_empty(params.code_challenge_method.as_deref());
    if challenge.is_some() && method.unwrap_or("S256") != "S256" {
        return Err(oauth_error(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            "PKCE S256 is required",
        ));
    }

    let app = roosty_db::find_oauth_application_by_client_id(&state.db, &params.client_id)
        .await
        .map_err(server_error)?
        .ok_or_else(|| oauth_error(StatusCode::BAD_REQUEST, "invalid_client", "unknown client"))?;
    if !redirect_uri_matches(&app.redirect_uri, &params.redirect_uri) {
        return Err(oauth_error(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            "redirect_uri mismatch",
        ));
    }

    Ok(app)
}

#[derive(Deserialize)]
struct TokenForm {
    grant_type: String,
    code: String,
    client_id: String,
    client_secret: Option<String>,
    redirect_uri: String,
    code_verifier: Option<String>,
}

#[derive(Serialize)]
struct TokenResponse {
    access_token: String,
    token_type: &'static str,
    scope: String,
    created_at: i64,
}

async fn token(State(state): State<AppState>, FormOrJson(form): FormOrJson<TokenForm>) -> Response {
    if form.grant_type != "authorization_code" {
        return oauth_error(
            StatusCode::BAD_REQUEST,
            "unsupported_grant_type",
            "only authorization_code is supported",
        );
    }

    let app = match roosty_db::find_oauth_application_by_client_id(&state.db, &form.client_id).await
    {
        Ok(Some(app)) => app,
        Ok(None) => {
            return oauth_error(StatusCode::BAD_REQUEST, "invalid_client", "unknown client");
        }
        Err(error) => return server_error(error),
    };
    if !redirect_uri_matches(&app.redirect_uri, &form.redirect_uri) {
        return oauth_error(
            StatusCode::BAD_REQUEST,
            "invalid_grant",
            "redirect_uri mismatch",
        );
    }
    if let Some(secret) = form.client_secret.as_deref() {
        let supplied_hash = match roosty_db::secret_hash(&state.config.token_pepper, secret) {
            Ok(hash) => hash,
            Err(error) => return server_error(error),
        };
        if supplied_hash != app.client_secret_hash {
            return oauth_error(
                StatusCode::UNAUTHORIZED,
                "invalid_client",
                "invalid client secret",
            );
        }
    }

    let Some((account_id, scopes, challenge, method)) =
        (match roosty_db::consume_authorization_code(
            &state.db,
            &state.config.token_pepper,
            &form.code,
            app.id,
            &form.redirect_uri,
        )
        .await
        {
            Ok(result) => result,
            Err(error) => return server_error(error),
        })
    else {
        return oauth_error(
            StatusCode::BAD_REQUEST,
            "invalid_grant",
            "invalid authorization code",
        );
    };

    if !challenge.is_empty()
        && (method != "S256"
            || form
                .code_verifier
                .as_deref()
                .is_none_or(|verifier| roosty_db::pkce_s256_challenge(verifier) != challenge))
    {
        return oauth_error(
            StatusCode::BAD_REQUEST,
            "invalid_grant",
            "invalid PKCE verifier",
        );
    }

    match roosty_db::create_access_token(
        &state.db,
        &state.config.token_pepper,
        account_id,
        app.id,
        &scopes,
    )
    .await
    {
        Ok(token) => Json(TokenResponse {
            access_token: token.token,
            token_type: token.token_type,
            scope: token.scope,
            created_at: token.created_at,
        })
        .into_response(),
        Err(error) => server_error(error),
    }
}

#[derive(Deserialize)]
struct RevokeForm {
    token: String,
}

async fn revoke(
    State(state): State<AppState>,
    FormOrJson(form): FormOrJson<RevokeForm>,
) -> Response {
    match roosty_db::revoke_access_token(&state.db, &state.config.token_pepper, &form.token).await {
        Ok(()) => StatusCode::OK.into_response(),
        Err(error) => server_error(error),
    }
}

#[derive(Serialize)]
pub(crate) struct AccountResponse {
    id: String,
    username: String,
    acct: String,
    display_name: String,
    locked: bool,
    bot: bool,
    discoverable: bool,
    group: bool,
    created_at: String,
    note: String,
    url: String,
    avatar: String,
    avatar_static: String,
    header: String,
    header_static: String,
    fields: Vec<Value>,
    emojis: Vec<Value>,
    followers_count: u64,
    following_count: u64,
    statuses_count: u64,
    last_status_at: Option<String>,
    source: AccountSource,
    role: AccountRole,
}

#[derive(Serialize)]
pub(crate) struct AccountSource {
    note: String,
    fields: Vec<serde_json::Value>,
    privacy: String,
    sensitive: bool,
    language: String,
    quote_policy: String,
    follow_requests_count: u64,
}

#[derive(Serialize)]
pub(crate) struct AccountRole {
    id: String,
    name: String,
    color: String,
    permissions: String,
    highlighted: bool,
}

/// Mastodon preference response keyed by compatibility field names.
#[derive(Serialize)]
struct PreferencesResponse {
    #[serde(rename = "posting:default:visibility")]
    posting_default_visibility: String,
    #[serde(rename = "posting:default:sensitive")]
    posting_default_sensitive: bool,
    #[serde(rename = "posting:default:language")]
    posting_default_language: Option<String>,
    #[serde(rename = "posting:default:quote_policy")]
    posting_default_quote_policy: String,
    #[serde(rename = "reading:expand:media")]
    reading_expand_media: &'static str,
    #[serde(rename = "reading:expand:spoilers")]
    reading_expand_spoilers: bool,
}

/// Parsed account settings update from JSON or form input.
#[derive(Default)]
struct UpdateCredentialsInput {
    display_name: Option<String>,
    note: Option<String>,
    locked: Option<bool>,
    bot: Option<bool>,
    discoverable: Option<bool>,
    default_visibility: Option<String>,
    default_sensitive: Option<bool>,
    default_language: Option<Option<String>>,
    default_quote_policy: Option<String>,
    profile_fields: Option<Value>,
    avatar: Option<ProfileImageUpload>,
    header: Option<ProfileImageUpload>,
    avatar_file_path: Option<String>,
    header_file_path: Option<String>,
}

/// Profile image upload bytes kept after axum-params temp files are dropped.
struct ProfileImageUpload {
    content_type: String,
    bytes: Vec<u8>,
}

/// Account settings payload accepted by Mastodon's update credentials endpoint.
#[derive(Default, Deserialize)]
struct UpdateCredentialsParams {
    display_name: Option<String>,
    note: Option<String>,
    locked: Option<Value>,
    bot: Option<Value>,
    discoverable: Option<Value>,
    source: Option<UpdateCredentialsSourceParams>,
    #[serde(rename = "source[privacy]")]
    source_privacy: Option<String>,
    #[serde(rename = "source[sensitive]")]
    source_sensitive: Option<Value>,
    #[serde(rename = "source[language]")]
    source_language: Option<Value>,
    #[serde(rename = "source[quote_policy]")]
    source_quote_policy: Option<String>,
    fields_attributes: Option<Value>,
    #[serde(default, deserialize_with = "deserialize_optional_upload_file")]
    avatar: Option<UploadFile>,
    #[serde(default, deserialize_with = "deserialize_optional_upload_file")]
    header: Option<UploadFile>,
}

/// Deserialize optional profile upload fields while accepting null-like text sentinels.
fn deserialize_optional_upload_file<'de, D>(deserializer: D) -> Result<Option<UploadFile>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    deserializer.deserialize_any(OptionalUploadFileVisitor)
}

struct OptionalUploadFileVisitor;

impl<'de> Visitor<'de> for OptionalUploadFileVisitor {
    type Value = Option<UploadFile>;

    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("a multipart upload file or a null-like text field")
    }

    fn visit_unit<E>(self) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        Ok(None)
    }

    fn visit_none<E>(self) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        Ok(None)
    }

    fn visit_some<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        deserialize_optional_upload_file(deserializer)
    }

    fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        if matches!(value.trim(), "" | "null" | "undefined") {
            Ok(None)
        } else {
            Err(E::custom("expected upload file"))
        }
    }

    fn visit_string<E>(self, value: String) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        self.visit_str(value.as_str())
    }

    fn visit_map<A>(self, map: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        UploadFile::deserialize(de::value::MapAccessDeserializer::new(map)).map(Some)
    }
}

/// Nested default posting settings from Mastodon profile update requests.
#[derive(Default, Deserialize)]
struct UpdateCredentialsSourceParams {
    privacy: Option<String>,
    sensitive: Option<Value>,
    language: Option<Value>,
    quote_policy: Option<String>,
}

/// Validation errors for Mastodon account settings updates.
#[derive(Debug, thiserror::Error)]
enum UpdateCredentialsError {
    #[error("boolean value is invalid")]
    Boolean,
    #[error("source[privacy] is invalid")]
    Visibility,
    #[error("source[quote_policy] is invalid")]
    QuotePolicy,
    #[error("source[language] is invalid")]
    Language,
    #[error("profile image is invalid")]
    ProfileImage,
}

impl<S> FromRequest<S> for UpdateCredentialsInput
where
    S: Send + Sync,
{
    type Rejection = Response;

    async fn from_request(request: Request, state: &S) -> Result<Self, Self::Rejection> {
        let Params(params, _temp_files) =
            Params::<UpdateCredentialsParams>::from_request(request, state)
                .await
                .map_err(|error| bad_request(&format!("invalid request body: {error:?}")))?;

        update_credentials_input_from_params(params)
            .await
            .map_err(|error| bad_request(&error.to_string()))
    }
}

async fn verify_credentials(
    State(state): State<AppState>,
    AuthenticatedAccount(account): AuthenticatedAccount,
) -> Response {
    match account_response(&state, account).await {
        Ok(account) => Json(account).into_response(),
        Err(error) => server_error(error),
    }
}

async fn preferences(AuthenticatedAccount(account): AuthenticatedAccount) -> Response {
    Json(PreferencesResponse {
        posting_default_visibility: account.default_visibility,
        posting_default_sensitive: account.default_sensitive,
        posting_default_language: account.default_language,
        posting_default_quote_policy: account.default_quote_policy,
        reading_expand_media: "default",
        reading_expand_spoilers: false,
    })
    .into_response()
}

async fn update_credentials(
    State(state): State<AppState>,
    AuthenticatedAccount(account): AuthenticatedAccount,
    mut input: UpdateCredentialsInput,
) -> Response {
    if let Some(avatar) = input.avatar.take() {
        match store_profile_image(&state, account.id, "avatar", avatar).await {
            Ok(path) => input.avatar_file_path = Some(path),
            Err(error) => return server_error(error),
        }
    }
    if let Some(header) = input.header.take() {
        match store_profile_image(&state, account.id, "header", header).await {
            Ok(path) => input.header_file_path = Some(path),
            Err(error) => return server_error(error),
        }
    }
    let update = match settings_update_from_input(input) {
        Ok(update) => update,
        Err(error) => return bad_request(&error.to_string()),
    };

    match roosty_db::update_local_account_settings(&state.db, account.id, update).await {
        Ok(account) => match account_response(&state, account).await {
            Ok(account) => Json(account).into_response(),
            Err(error) => server_error(error),
        },
        Err(error) => server_error(error),
    }
}

/// Resolve an OAuth bearer token to the authenticated local account.
pub(crate) async fn authenticated_account(
    state: &AppState,
    headers: &HeaderMap,
) -> Result<roosty_db::LocalAccount, Response> {
    let bearer = bearer_token(headers).ok_or_else(|| {
        oauth_error(
            StatusCode::UNAUTHORIZED,
            "invalid_token",
            "missing bearer token",
        )
    })?;

    account_from_bearer_token(state, bearer).await
}

/// Resolve an OAuth bearer token to an account when the request has one.
pub(crate) async fn optional_authenticated_account(
    state: &AppState,
    headers: &HeaderMap,
) -> Result<Option<roosty_db::LocalAccount>, Response> {
    let Some(bearer) = bearer_token(headers) else {
        return Ok(None);
    };

    account_from_bearer_token(state, bearer).await.map(Some)
}

/// Resolve a raw OAuth bearer token to the authenticated local account.
pub(crate) async fn account_from_bearer_token(
    state: &AppState,
    bearer: &str,
) -> Result<roosty_db::LocalAccount, Response> {
    roosty_db::find_account_by_access_token(&state.db, &state.config.token_pepper, bearer)
        .await
        .map_err(server_error)?
        .map(|(account, _scopes)| account)
        .ok_or_else(|| oauth_error(StatusCode::UNAUTHORIZED, "invalid_token", "invalid token"))
}

/// Build the Mastodon-compatible credential account response.
pub(crate) async fn account_response(
    state: &AppState,
    account: roosty_db::LocalAccount,
) -> Result<AccountResponse, RoostyError> {
    let account_url = match state
        .config
        .public_base_url
        .join(&format!("@{}", account.username))
    {
        Ok(url) => url.to_string(),
        Err(_) => format!("{}/@{}", state.config.public_base_url, account.username),
    };
    let profile_fields = profile_fields_from_value(&account.profile_fields);
    let display_name = if account.display_name.is_empty() {
        account.username.clone()
    } else {
        account.display_name.clone()
    };
    let statuses_count = roosty_db::count_local_statuses_by_account(&state.db, account.id).await?;
    let followers_count = roosty_db::count_local_followers(&state.db, account.id).await?
        + roosty_db::count_remote_followers(&state.db, account.id).await?;
    let following_count = roosty_db::count_local_following(&state.db, account.id).await?;
    let last_status_at = roosty_db::last_local_status_at(&state.db, account.id)
        .await?
        .map(|timestamp| DateOnly(timestamp).to_string());
    let avatar = account
        .avatar_file_path
        .as_deref()
        .map(|path| crate::media::media_url(state, path))
        .unwrap_or_default();
    let header = account
        .header_file_path
        .as_deref()
        .map(|path| crate::media::media_url(state, path))
        .unwrap_or_default();

    Ok(AccountResponse {
        id: account.id.0.to_string(),
        username: account.username.clone(),
        acct: account.username.clone(),
        display_name,
        locked: account.locked,
        bot: account.bot,
        discoverable: account.discoverable,
        group: false,
        created_at: "1970-01-01T00:00:00.000Z".to_owned(),
        note: account.note.clone(),
        url: account_url,
        avatar: avatar.clone(),
        avatar_static: avatar,
        header: header.clone(),
        header_static: header,
        fields: profile_fields.clone(),
        emojis: Vec::new(),
        followers_count,
        following_count,
        statuses_count,
        last_status_at,
        source: AccountSource {
            note: account.note,
            fields: profile_fields,
            privacy: account.default_visibility,
            sensitive: account.default_sensitive,
            language: account.default_language.unwrap_or_default(),
            quote_policy: account.default_quote_policy,
            follow_requests_count: 0,
        },
        role: AccountRole {
            id: if account.is_admin { "1" } else { "0" }.to_owned(),
            name: if account.is_admin { "Admin" } else { "User" }.to_owned(),
            color: String::new(),
            permissions: "0".to_owned(),
            highlighted: account.is_admin,
        },
    })
}

/// Convert stored profile fields to the array shape expected by account APIs.
fn profile_fields_from_value(value: &Value) -> Vec<Value> {
    value.as_array().cloned().unwrap_or_default()
}

/// Date-only display wrapper used by Mastodon's `last_status_at` field.
struct DateOnly(OffsetDateTime);

impl fmt::Display for DateOnly {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{:04}-{:02}-{:02}",
            self.0.year(),
            u8::from(self.0.month()),
            self.0.day()
        )
    }
}

/// Convert parsed update input into a validated database update.
fn settings_update_from_input(
    input: UpdateCredentialsInput,
) -> Result<roosty_db::LocalAccountSettingsUpdate, UpdateCredentialsError> {
    if let Some(visibility) = input.default_visibility.as_deref() {
        validate_visibility(visibility)?;
    }
    if let Some(quote_policy) = input.default_quote_policy.as_deref() {
        validate_quote_policy(quote_policy)?;
    }
    if let Some(Some(language)) = input.default_language.as_ref() {
        validate_language(language)?;
    }

    Ok(roosty_db::LocalAccountSettingsUpdate {
        display_name: input.display_name,
        note: input.note,
        locked: input.locked,
        bot: input.bot,
        discoverable: input.discoverable,
        default_visibility: input.default_visibility,
        default_sensitive: input.default_sensitive,
        default_language: input.default_language,
        default_quote_policy: input.default_quote_policy,
        profile_fields: input.profile_fields,
        avatar_file_path: input.avatar_file_path,
        header_file_path: input.header_file_path,
    })
}

/// Convert extracted account update parameters into an internal update request.
async fn update_credentials_input_from_params(
    params: UpdateCredentialsParams,
) -> Result<UpdateCredentialsInput, UpdateCredentialsError> {
    let source = params.source.unwrap_or_default();
    let avatar = profile_image_upload(params.avatar)
        .await
        .map_err(|_| UpdateCredentialsError::ProfileImage)?;
    let header = profile_image_upload(params.header)
        .await
        .map_err(|_| UpdateCredentialsError::ProfileImage)?;

    Ok(UpdateCredentialsInput {
        display_name: params.display_name,
        note: params.note,
        locked: optional_bool(params.locked.as_ref())?,
        bot: optional_bool(params.bot.as_ref())?,
        discoverable: optional_bool(params.discoverable.as_ref())?,
        default_visibility: source.privacy.or(params.source_privacy),
        default_sensitive: match optional_bool(source.sensitive.as_ref())? {
            Some(value) => Some(value),
            None => optional_bool(params.source_sensitive.as_ref())?,
        },
        default_language: json_language(source.language.as_ref())
            .or_else(|| json_language(params.source_language.as_ref())),
        default_quote_policy: source.quote_policy.or(params.source_quote_policy),
        profile_fields: json_profile_fields(params.fields_attributes.as_ref()),
        avatar,
        header,
        avatar_file_path: None,
        header_file_path: None,
    })
}

/// Read an optional profile image upload before its temporary file is removed.
async fn profile_image_upload(
    upload: Option<UploadFile>,
) -> Result<Option<ProfileImageUpload>, std::io::Error> {
    let Some(upload) = upload else {
        return Ok(None);
    };
    let mut file = upload.open().await?;
    let mut bytes = Vec::new();
    tokio::io::AsyncReadExt::read_to_end(&mut file, &mut bytes).await?;
    Ok(Some(ProfileImageUpload {
        content_type: upload.content_type,
        bytes,
    }))
}

/// Store a validated profile image under the local media root.
async fn store_profile_image(
    state: &AppState,
    account_id: AccountId,
    kind: &str,
    upload: ProfileImageUpload,
) -> Result<String, RoostyError> {
    let extension = crate::media::supported_image_extension(&upload.content_type)
        .ok_or_else(|| RoostyError::InvalidInput("profile image type is invalid".to_owned()))?;
    image::load_from_memory(&upload.bytes)
        .map_err(|error| RoostyError::InvalidInput(format!("profile image is invalid: {error}")))?;

    let relative_path = format!(
        "accounts/{}/{}-{}.{}",
        account_id.0.simple(),
        kind,
        Uuid::now_v7().simple(),
        extension
    );
    let full_path = Path::new(&state.config.media_root).join(&relative_path);
    if let Some(parent) = full_path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    tokio::fs::write(full_path, upload.bytes).await?;

    Ok(relative_path)
}

/// Validate default status visibility values accepted by Mastodon clients.
fn validate_visibility(value: &str) -> Result<(), UpdateCredentialsError> {
    match value {
        "public" | "unlisted" | "private" | "direct" => Ok(()),
        _ => Err(UpdateCredentialsError::Visibility),
    }
}

/// Validate default quote policy values accepted by Mastodon clients.
fn validate_quote_policy(value: &str) -> Result<(), UpdateCredentialsError> {
    match value {
        "public" | "followers" | "nobody" => Ok(()),
        _ => Err(UpdateCredentialsError::QuotePolicy),
    }
}

/// Validate a short language tag for default posting language.
fn validate_language(value: &str) -> Result<(), UpdateCredentialsError> {
    let valid = value.len() <= 16
        && value
            .chars()
            .all(|character| character.is_ascii_alphabetic() || character == '-');
    if valid {
        Ok(())
    } else {
        Err(UpdateCredentialsError::Language)
    }
}

/// Parse an optional extracted boolean field.
fn optional_bool(value: Option<&Value>) -> Result<Option<bool>, UpdateCredentialsError> {
    value
        .map(|value| match value {
            Value::Bool(value) => Ok(*value),
            Value::Number(value) if value.as_u64() == Some(1) => Ok(true),
            Value::Number(value) if value.as_u64() == Some(0) => Ok(false),
            Value::String(value) => parse_bool(value),
            _ => Err(UpdateCredentialsError::Boolean),
        })
        .transpose()
}

/// Normalize an optional JSON language value, preserving explicit null.
fn json_language(value: Option<&Value>) -> Option<Option<String>> {
    value.map(|value| match value {
        Value::Null => None,
        Value::String(value) => normalize_language(value.to_owned()),
        _ => None,
    })
}

/// Convert empty language strings to null storage values.
fn normalize_language(value: String) -> Option<String> {
    let value = value.trim();
    if value.is_empty() {
        None
    } else {
        Some(value.to_owned())
    }
}

/// Parse boolean values commonly submitted by HTML forms and clients.
fn parse_bool(value: &str) -> Result<bool, UpdateCredentialsError> {
    match value {
        "1" | "true" | "TRUE" | "yes" | "YES" => Ok(true),
        "0" | "false" | "FALSE" | "no" | "NO" => Ok(false),
        _ => Err(UpdateCredentialsError::Boolean),
    }
}

/// Convert JSON `fields_attributes` into the stored profile field array.
fn json_profile_fields(value: Option<&Value>) -> Option<Value> {
    let value = value?;
    let values = match value {
        Value::Array(fields) => fields
            .iter()
            .filter_map(profile_field_from_value)
            .collect::<Vec<_>>(),
        Value::Object(fields) => {
            let mut keys = fields.keys().collect::<Vec<_>>();
            keys.sort();
            keys.into_iter()
                .filter_map(|key| profile_field_from_value(fields.get(key)?))
                .collect::<Vec<_>>()
        }
        _ => return None,
    };
    Some(Value::Array(values))
}

/// Convert one JSON profile field object into the stored field shape.
fn profile_field_from_value(value: &Value) -> Option<Value> {
    let object = value.as_object()?;
    profile_field(
        object.get("name")?.as_str()?.to_owned(),
        object.get("value")?.as_str()?.to_owned(),
    )
}

/// Build a stored profile field, omitting fully blank rows.
fn profile_field(name: String, value: String) -> Option<Value> {
    let name = name.trim();
    let value = value.trim();
    if name.is_empty() && value.is_empty() {
        return None;
    }
    Some(json!({
        "name": name,
        "value": value,
        "verified_at": null,
    }))
}

fn account_id_from_session(
    state: &AppState,
    headers: &HeaderMap,
) -> Result<Option<AccountId>, RoostyError> {
    let Some(cookie_header) = headers
        .get(header::COOKIE)
        .and_then(|value| value.to_str().ok())
    else {
        return Ok(None);
    };
    let Some(cookie_value) = cookie_header
        .split(';')
        .filter_map(|part| part.trim().split_once('='))
        .find_map(|(name, value)| (name == SESSION_COOKIE).then_some(value))
    else {
        return Ok(None);
    };

    parse_session_cookie(&state.config.session_secret, cookie_value)
}

fn session_cookie(state: &AppState, account_id: AccountId) -> Result<String, RoostyError> {
    let expires_at = OffsetDateTime::now_utc() + Duration::days(7);
    let payload = format!("{}.{}", account_id.0, expires_at.unix_timestamp());
    let signature = sign(&state.config.session_secret, &payload)?;
    Ok(format!(
        "{SESSION_COOKIE}={payload}.{signature}; Path=/; Max-Age=604800; HttpOnly; Secure; SameSite=Lax"
    ))
}

fn parse_session_cookie(secret: &str, value: &str) -> Result<Option<AccountId>, RoostyError> {
    let parts = value.split('.').collect::<Vec<_>>();
    if parts.len() != 3 {
        return Ok(None);
    }
    let payload = format!("{}.{}", parts[0], parts[1]);
    if sign(secret, &payload)? != parts[2] {
        return Ok(None);
    }
    let expires_at = match parts[1].parse::<i64>() {
        Ok(timestamp) => timestamp,
        Err(_) => return Ok(None),
    };
    if expires_at <= OffsetDateTime::now_utc().unix_timestamp() {
        return Ok(None);
    }
    let account_id = match parts[0].parse() {
        Ok(account_id) => account_id,
        Err(_) => return Ok(None),
    };

    Ok(Some(AccountId(account_id)))
}

fn sign(secret: &str, payload: &str) -> Result<String, RoostyError> {
    let mut mac = HmacSha256::new_from_slice(secret.as_bytes())
        .map_err(|error| RoostyError::InvalidInput(error.to_string()))?;
    mac.update(payload.as_bytes());
    Ok(URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes()))
}

fn bearer_token(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
}

fn optional_non_empty(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|value| !value.is_empty())
}

fn public_url(state: &AppState, path_and_query: &str) -> String {
    state
        .config
        .public_base_url
        .join(path_and_query.trim_start_matches('/'))
        .map(|url| url.to_string())
        .unwrap_or_else(|_| format!("{}{}", state.config.public_base_url, path_and_query))
}

fn redirect_uri_matches(registered: &str, requested: &str) -> bool {
    registered
        .lines()
        .map(str::trim)
        .any(|redirect_uri| redirect_uri == requested)
}

fn sanitize_next(next: Option<&str>) -> String {
    let next = next.unwrap_or("/");
    if next.starts_with('/') && !next.starts_with("//") {
        next.to_owned()
    } else {
        "/".to_owned()
    }
}

fn authorize_query_string(params: &AuthorizeParams) -> String {
    let mut pairs = vec![
        ("response_type", params.response_type.as_str()),
        ("client_id", params.client_id.as_str()),
        ("redirect_uri", params.redirect_uri.as_str()),
    ];
    if let Some(state) = params.state.as_deref() {
        pairs.push(("state", state));
    }
    if let Some(scope) = params.scope.as_deref() {
        pairs.push(("scope", scope));
    }
    if let Some(challenge) = params.code_challenge.as_deref() {
        pairs.push(("code_challenge", challenge));
    }
    if let Some(method) = params.code_challenge_method.as_deref() {
        pairs.push(("code_challenge_method", method));
    }

    pairs
        .into_iter()
        .map(|(key, value)| format!("{key}={}", url_encode(value)))
        .collect::<Vec<_>>()
        .join("&")
}

fn url_encode(value: &str) -> String {
    form_urlencoded::byte_serialize(value.as_bytes()).collect()
}

fn oauth_error(status: StatusCode, error: &str, description: &str) -> Response {
    Json(serde_json::json!({
        "error": error,
        "error_description": description,
    }))
    .into_response_with_status(status)
}

fn bad_request(description: &str) -> Response {
    oauth_error(StatusCode::BAD_REQUEST, "invalid_request", description)
}

fn server_error(error: RoostyError) -> Response {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(serde_json::json!({
            "error": "server_error",
            "error_description": error.to_string(),
        })),
    )
        .into_response()
}

trait JsonStatus {
    fn into_response_with_status(self, status: StatusCode) -> Response;
}

impl<T> JsonStatus for Json<T>
where
    T: Serialize,
{
    fn into_response_with_status(self, status: StatusCode) -> Response {
        (status, self).into_response()
    }
}

#[cfg(test)]
mod tests {
    use std::{
        io::Cursor,
        net::{IpAddr, Ipv4Addr, SocketAddr},
        time::{SystemTime, UNIX_EPOCH},
    };

    use axum::{
        Router,
        body::{Body, to_bytes},
        http::{
            Request, Response, StatusCode,
            header::{AUTHORIZATION, CONTENT_TYPE, COOKIE, LOCATION, SET_COOKIE},
        },
    };
    use image::{ImageBuffer, ImageFormat, Rgba};
    use postgresql_embedded::PostgreSQL;
    use roosty_migration::Migrator;
    use sea_orm_migration::MigratorTrait;
    use serde_json::{Value, json};
    use tempfile::TempDir;
    use test_context::{AsyncTestContext, test_context};
    use tower::ServiceExt;
    use url::{Url, form_urlencoded};

    use crate::{config::Config, http::AppState, password};

    const REDIRECT_URI: &str = "https://localhost:4001/oauth";
    const CODE_VERIFIER: &str = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
    const CODE_CHALLENGE: &str = "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM";

    #[test_context(EndpointContext)]
    #[tokio::test]
    async fn registers_oauth_app(context: &mut EndpointContext) {
        let response = context
            .form(
                "POST",
                "/api/v1/apps",
                &[
                    ("client_name", "Elk"),
                    ("redirect_uris", REDIRECT_URI),
                    ("scopes", "read write"),
                    ("website", "https://elk.zone"),
                ],
            )
            .await;

        assert_eq!(response.status(), StatusCode::OK);
        let body = json_body(response).await;
        assert_eq!(body["name"], "Elk");
        assert_eq!(body["redirect_uri"], REDIRECT_URI);
        assert!(
            body["client_id"]
                .as_str()
                .is_some_and(|value| !value.is_empty())
        );
        assert!(
            body["client_secret"]
                .as_str()
                .is_some_and(|value| !value.is_empty())
        );
    }

    #[test_context(EndpointContext)]
    #[tokio::test]
    async fn registers_elk_oauth_app_from_json(context: &mut EndpointContext) {
        let redirect_uri =
            "https://localhost:4001/api/roosty.localhost:4000/oauth/https%3A%2F%2Flocalhost%3A4001";
        let response = context
            .json(
                "POST",
                "/api/v1/apps",
                serde_json::json!({
                    "client_name": "Elk",
                    "redirect_uris": redirect_uri,
                    "scopes": "read write follow push",
                    "website": "https://localhost:4001",
                }),
            )
            .await;

        assert_eq!(response.status(), StatusCode::OK);
        let body = json_body(response).await;
        assert_eq!(body["name"], "Elk");
        assert_eq!(body["redirect_uri"], redirect_uri);
        assert!(
            body["client_id"]
                .as_str()
                .is_some_and(|value| !value.is_empty())
        );
        assert!(
            body["client_secret"]
                .as_str()
                .is_some_and(|value| !value.is_empty())
        );
    }

    #[test_context(EndpointContext)]
    #[tokio::test]
    async fn authorize_redirects_anonymous_users_to_login(context: &mut EndpointContext) {
        let app = context.register_app().await;
        let response = context
            .request(
                Request::builder()
                    .method("GET")
                    .uri(authorize_uri(&app.client_id))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await;

        assert_eq!(response.status(), StatusCode::SEE_OTHER);
        let location = header_value(&response, LOCATION);
        assert!(location.starts_with("https://localhost:4000/login?next="));
    }

    #[test_context(EndpointContext)]
    #[tokio::test]
    async fn login_sets_a_session_cookie(context: &mut EndpointContext) {
        let response = context
            .form(
                "POST",
                "/login",
                &[
                    ("login", "admin"),
                    ("password", "password"),
                    ("next", "/oauth/authorize"),
                ],
            )
            .await;

        assert_eq!(response.status(), StatusCode::SEE_OTHER);
        assert_eq!(
            header_value(&response, LOCATION),
            "https://localhost:4000/oauth/authorize"
        );
        assert!(session_cookie(&response).starts_with("roosty_session="));
    }

    #[test_context(EndpointContext)]
    #[tokio::test]
    async fn oauth_code_flow_verifies_credentials(context: &mut EndpointContext) {
        let app = context.register_app().await;
        let cookie = context.login().await;
        let code = context.authorize(&app.client_id, &cookie).await;
        let token = context.token(&app, &code).await;

        let response = context
            .request(
                Request::builder()
                    .method("GET")
                    .uri("/api/v1/accounts/verify_credentials")
                    .header(AUTHORIZATION, format!("Bearer {token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await;

        assert_eq!(response.status(), StatusCode::OK);
        let body = json_body(response).await;
        assert_eq!(body["username"], "admin");
        assert_eq!(body["role"]["name"], "Admin");
    }

    #[test_context(EndpointContext)]
    #[tokio::test]
    async fn elk_json_oauth_flow_without_pkce(context: &mut EndpointContext) {
        let response = context
            .json(
                "POST",
                "/api/v1/apps",
                serde_json::json!({
                    "client_name": "Elk",
                    "redirect_uris": REDIRECT_URI,
                    "scopes": "read write follow push",
                    "website": "https://localhost:4001",
                }),
            )
            .await;
        assert_eq!(response.status(), StatusCode::OK);
        let body = json_body(response).await;
        let app = RegisteredApp {
            client_id: body["client_id"].as_str().unwrap().to_owned(),
            client_secret: body["client_secret"].as_str().unwrap().to_owned(),
        };

        let cookie = context.login().await;
        let code = context
            .authorize_without_pkce(&app.client_id, &cookie)
            .await;
        let token = context.token_json_without_pkce(&app, &code).await;
        let response = context
            .request(
                Request::builder()
                    .method("GET")
                    .uri("/api/v1/accounts/verify_credentials")
                    .header(AUTHORIZATION, format!("Bearer {token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await;

        assert_eq!(response.status(), StatusCode::OK);
        let body = json_body(response).await;
        assert_eq!(body["username"], "admin");
    }

    #[test_context(EndpointContext)]
    #[tokio::test]
    async fn elk_authorize_url_can_omit_state(context: &mut EndpointContext) {
        let app = context.register_app().await;
        let cookie = context.login().await;
        let body = form_urlencoded::Serializer::new(String::new())
            .extend_pairs([
                ("response_type", "code"),
                ("client_id", app.client_id.as_str()),
                ("redirect_uri", REDIRECT_URI),
                ("scope", "read write follow push"),
            ])
            .finish();
        let response = context
            .request(
                Request::builder()
                    .method("POST")
                    .uri("/oauth/authorize")
                    .header(CONTENT_TYPE, "application/x-www-form-urlencoded")
                    .header(COOKIE, cookie)
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await;

        assert_eq!(response.status(), StatusCode::SEE_OTHER);
        let redirect = Url::parse(&header_value(&response, LOCATION)).unwrap();
        assert!(
            redirect
                .query_pairs()
                .any(|(name, value)| name == "code" && !value.is_empty())
        );
        assert!(!redirect.query_pairs().any(|(name, _)| name == "state"));
    }

    #[test_context(EndpointContext)]
    #[tokio::test]
    async fn authorize_treats_empty_pkce_fields_as_omitted(context: &mut EndpointContext) {
        let app = context.register_app().await;
        let cookie = context.login().await;
        let body = form_urlencoded::Serializer::new(String::new())
            .extend_pairs([
                ("response_type", "code"),
                ("client_id", app.client_id.as_str()),
                ("redirect_uri", REDIRECT_URI),
                ("scope", "read write follow push"),
                ("code_challenge", ""),
                ("code_challenge_method", ""),
            ])
            .finish();
        let response = context
            .request(
                Request::builder()
                    .method("POST")
                    .uri("/oauth/authorize")
                    .header(CONTENT_TYPE, "application/x-www-form-urlencoded")
                    .header(COOKIE, cookie)
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await;

        assert_eq!(response.status(), StatusCode::SEE_OTHER);
        let redirect = Url::parse(&header_value(&response, LOCATION)).unwrap();
        assert!(
            redirect
                .query_pairs()
                .any(|(name, value)| name == "code" && !value.is_empty())
        );
    }

    #[test_context(EndpointContext)]
    #[tokio::test]
    async fn authorization_codes_are_single_use(context: &mut EndpointContext) {
        let app = context.register_app().await;
        let cookie = context.login().await;
        let code = context.authorize(&app.client_id, &cookie).await;
        let _token = context.token(&app, &code).await;

        let response = context
            .form(
                "POST",
                "/oauth/token",
                &[
                    ("grant_type", "authorization_code"),
                    ("client_id", &app.client_id),
                    ("client_secret", &app.client_secret),
                    ("redirect_uri", REDIRECT_URI),
                    ("code", &code),
                    ("code_verifier", CODE_VERIFIER),
                ],
            )
            .await;

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let body = json_body(response).await;
        assert_eq!(body["error"], "invalid_grant");
    }

    #[test_context(EndpointContext)]
    #[tokio::test]
    async fn verify_credentials_rejects_missing_token(context: &mut EndpointContext) {
        let response = context
            .request(
                Request::builder()
                    .method("GET")
                    .uri("/api/v1/accounts/verify_credentials")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await;

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        let body = json_body(response).await;
        assert_eq!(body["error"], "invalid_token");
    }

    #[test_context(EndpointContext)]
    #[tokio::test]
    async fn preferences_require_a_valid_token(context: &mut EndpointContext) {
        // Preferences expose account-specific defaults, so anonymous and bogus
        // bearer requests must fail the same way as other user-token APIs.
        let missing = context
            .request(
                Request::builder()
                    .method("GET")
                    .uri("/api/v1/preferences")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await;
        assert_eq!(missing.status(), StatusCode::UNAUTHORIZED);

        let invalid = context
            .request(
                Request::builder()
                    .method("GET")
                    .uri("/api/v1/preferences")
                    .header(AUTHORIZATION, "Bearer not-a-real-token")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await;
        assert_eq!(invalid.status(), StatusCode::UNAUTHORIZED);
    }

    #[test_context(EndpointContext)]
    #[tokio::test]
    async fn preferences_return_persisted_posting_defaults(context: &mut EndpointContext) {
        // Elk reads these immediately after login to seed composer defaults.
        let token = context.authenticated_token().await;

        let response = context
            .request(
                Request::builder()
                    .method("GET")
                    .uri("/api/v1/preferences")
                    .header(AUTHORIZATION, format!("Bearer {token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await;

        assert_eq!(response.status(), StatusCode::OK);
        let body = json_body(response).await;
        assert_eq!(body["posting:default:visibility"], "public");
        assert_eq!(body["posting:default:sensitive"], false);
        assert_eq!(body["posting:default:language"], "en");
        assert_eq!(body["posting:default:quote_policy"], "followers");
        assert_eq!(body["reading:expand:media"], "default");
        assert_eq!(body["reading:expand:spoilers"], false);
    }

    #[test_context(EndpointContext)]
    #[tokio::test]
    /// Given URL-encoded profile and source settings, persists account metadata and preferences.
    async fn update_credentials_persists_profile_and_preferences(context: &mut EndpointContext) {
        // Mastodon clients update both profile settings and composer defaults
        // through update_credentials; preferences should reflect those writes.
        let token = context.authenticated_token().await;
        let body = form_urlencoded::Serializer::new(String::new())
            .extend_pairs([
                ("display_name", "Admin Person"),
                ("note", "A local administrator"),
                ("locked", "true"),
                ("bot", "false"),
                ("discoverable", "false"),
                ("source[privacy]", "unlisted"),
                ("source[sensitive]", "true"),
                ("source[language]", "de"),
                ("source[quote_policy]", "nobody"),
                ("fields_attributes[0][name]", "Website"),
                ("fields_attributes[0][value]", "https://example.com"),
            ])
            .finish();
        let update_response = context
            .request(
                Request::builder()
                    .method("PATCH")
                    .uri("/api/v1/accounts/update_credentials")
                    .header(AUTHORIZATION, format!("Bearer {token}"))
                    .header(CONTENT_TYPE, "application/x-www-form-urlencoded")
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await;

        assert_eq!(update_response.status(), StatusCode::OK);
        let body = json_body(update_response).await;
        assert_eq!(
            account_settings_snapshot(&body),
            json!({
                "display_name": "Admin Person",
                "note": "A local administrator",
                "locked": true,
                "bot": false,
                "discoverable": false,
                "source": {
                    "privacy": "unlisted",
                    "sensitive": true,
                    "language": "de",
                    "quote_policy": "nobody"
                },
                "fields": profile_fields_json(&[("Website", "https://example.com")])
            })
        );

        let credentials_response = context
            .request(
                Request::builder()
                    .method("GET")
                    .uri("/api/v1/accounts/verify_credentials")
                    .header(AUTHORIZATION, format!("Bearer {token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await;
        let body = json_body(credentials_response).await;
        assert_eq!(
            body["fields"],
            profile_fields_json(&[("Website", "https://example.com")])
        );

        let preferences_response = context
            .request(
                Request::builder()
                    .method("GET")
                    .uri("/api/v1/preferences")
                    .header(AUTHORIZATION, format!("Bearer {token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await;
        assert_eq!(preferences_response.status(), StatusCode::OK);
        let body = json_body(preferences_response).await;
        assert_eq!(
            preferences_snapshot(&body),
            json!({
                "posting:default:visibility": "unlisted",
                "posting:default:sensitive": true,
                "posting:default:language": "de",
                "posting:default:quote_policy": "nobody"
            })
        );
    }

    #[test_context(EndpointContext)]
    #[tokio::test]
    /// Given multipart profile metadata, stores fields for later credential reads.
    async fn update_credentials_persists_multipart_profile_fields(context: &mut EndpointContext) {
        let token = context.authenticated_token().await;
        let boundary = "geckoformboundarytest";
        let body = [
            format!(
                "--{boundary}\r\nContent-Disposition: form-data; name=\"display_name\"\r\n\r\nJust me\r\n"
            ),
            format!(
                "--{boundary}\r\nContent-Disposition: form-data; name=\"fields_attributes[][name]\"\r\n\r\nWebsite\r\n"
            ),
            format!(
                "--{boundary}\r\nContent-Disposition: form-data; name=\"fields_attributes[][value]\"\r\n\r\nhttps://example.com\r\n"
            ),
            format!("--{boundary}--\r\n"),
        ]
        .concat();

        let update_response = context
            .request(
                Request::builder()
                    .method("PATCH")
                    .uri("/api/v1/accounts/update_credentials")
                    .header(AUTHORIZATION, format!("Bearer {token}"))
                    .header(
                        CONTENT_TYPE,
                        format!("multipart/form-data; boundary={boundary}"),
                    )
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await;

        assert_eq!(update_response.status(), StatusCode::OK);
        let update_body = json_body(update_response).await;
        assert_eq!(
            profile_snapshot(&update_body),
            json!({
                "display_name": "Just me",
                "fields": profile_fields_json(&[("Website", "https://example.com")])
            })
        );

        let credentials_response = context
            .request(
                Request::builder()
                    .method("GET")
                    .uri("/api/v1/accounts/verify_credentials")
                    .header(AUTHORIZATION, format!("Bearer {token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await;
        let credentials_body = json_body(credentials_response).await;
        assert_eq!(
            profile_snapshot(&credentials_body),
            json!({
                "display_name": "Just me",
                "fields": profile_fields_json(&[("Website", "https://example.com")])
            })
        );
    }

    #[test_context(EndpointContext)]
    #[tokio::test]
    /// Given multipart profile images, stores avatar and header paths for account responses.
    async fn update_credentials_persists_profile_images(context: &mut EndpointContext) {
        let token = context.authenticated_token().await;
        let boundary = "geckoformboundaryimages";
        let avatar = encoded_test_image();
        let header = encoded_test_image();
        let mut body = Vec::new();
        append_multipart_file(
            &mut body,
            boundary,
            "avatar",
            "avatar.png",
            "image/png",
            &avatar,
        );
        append_multipart_file(
            &mut body,
            boundary,
            "header",
            "header.png",
            "image/png",
            &header,
        );
        body.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());

        let update_response = context
            .request(
                Request::builder()
                    .method("PATCH")
                    .uri("/api/v1/accounts/update_credentials")
                    .header(AUTHORIZATION, format!("Bearer {token}"))
                    .header(
                        CONTENT_TYPE,
                        format!("multipart/form-data; boundary={boundary}"),
                    )
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await;

        assert_eq!(update_response.status(), StatusCode::OK);
        let update_body = json_body(update_response).await;
        let avatar_url = update_body["avatar"].as_str().unwrap();
        let header_url = update_body["header"].as_str().unwrap();
        assert!(avatar_url.contains("/media_attachments/files/accounts/"));
        assert!(header_url.contains("/media_attachments/files/accounts/"));
        assert_eq!(update_body["avatar_static"], avatar_url);
        assert_eq!(update_body["header_static"], header_url);

        let credentials_response = context
            .request(
                Request::builder()
                    .method("GET")
                    .uri("/api/v1/accounts/verify_credentials")
                    .header(AUTHORIZATION, format!("Bearer {token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await;
        let credentials_body = json_body(credentials_response).await;
        assert_eq!(credentials_body["avatar"], avatar_url);
        assert_eq!(credentials_body["header"], header_url);

        let avatar_path = avatar_url.strip_prefix("https://localhost:4000").unwrap();
        let served = context
            .request(
                Request::builder()
                    .method("GET")
                    .uri(avatar_path)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await;
        assert_eq!(served.status(), StatusCode::OK);
    }

    #[test_context(EndpointContext)]
    #[tokio::test]
    /// Given JSON profile metadata arrays, stores fields for later credential reads.
    async fn update_credentials_persists_json_profile_fields(context: &mut EndpointContext) {
        let token = context.authenticated_token().await;
        let update_response = context
            .request(
                Request::builder()
                    .method("PATCH")
                    .uri("/api/v1/accounts/update_credentials")
                    .header(AUTHORIZATION, format!("Bearer {token}"))
                    .header(CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        serde_json::json!({
                            "fields_attributes": [
                                { "name": "Website", "value": "https://example.com" },
                                { "name": "Location", "value": "Berlin" }
                            ]
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await;

        assert_eq!(update_response.status(), StatusCode::OK);
        let update_body = json_body(update_response).await;
        let fields =
            profile_fields_json(&[("Website", "https://example.com"), ("Location", "Berlin")]);
        assert_eq!(update_body["fields"], fields);

        let credentials_response = context
            .request(
                Request::builder()
                    .method("GET")
                    .uri("/api/v1/accounts/verify_credentials")
                    .header(AUTHORIZATION, format!("Bearer {token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await;
        let credentials_body = json_body(credentials_response).await;
        assert_eq!(credentials_body["fields"], fields);
    }

    #[test_context(EndpointContext)]
    #[tokio::test]
    async fn update_credentials_rejects_invalid_preferences(context: &mut EndpointContext) {
        // Reject invalid enum-like settings before they can be persisted.
        let token = context.authenticated_token().await;
        let body = form_urlencoded::Serializer::new(String::new())
            .extend_pairs([("source[privacy]", "everyone")])
            .finish();

        let response = context
            .request(
                Request::builder()
                    .method("PATCH")
                    .uri("/api/v1/accounts/update_credentials")
                    .header(AUTHORIZATION, format!("Bearer {token}"))
                    .header(CONTENT_TYPE, "application/x-www-form-urlencoded")
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await;

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let body = json_body(response).await;
        assert_eq!(body["error"], "invalid_request");
    }

    /// Select profile fields that should round-trip through account responses.
    fn profile_snapshot(body: &Value) -> Value {
        json!({
            "display_name": body["display_name"],
            "fields": body["fields"]
        })
    }

    /// Select profile and preference fields persisted by update credentials.
    fn account_settings_snapshot(body: &Value) -> Value {
        json!({
            "display_name": body["display_name"],
            "note": body["note"],
            "locked": body["locked"],
            "bot": body["bot"],
            "discoverable": body["discoverable"],
            "source": {
                "privacy": body["source"]["privacy"],
                "sensitive": body["source"]["sensitive"],
                "language": body["source"]["language"],
                "quote_policy": body["source"]["quote_policy"]
            },
            "fields": body["fields"]
        })
    }

    /// Select Mastodon preference keys that mirror update credentials settings.
    fn preferences_snapshot(body: &Value) -> Value {
        json!({
            "posting:default:visibility": body["posting:default:visibility"],
            "posting:default:sensitive": body["posting:default:sensitive"],
            "posting:default:language": body["posting:default:language"],
            "posting:default:quote_policy": body["posting:default:quote_policy"]
        })
    }

    /// Build expected Mastodon profile metadata field arrays.
    fn profile_fields_json(fields: &[(&str, &str)]) -> Value {
        Value::Array(
            fields
                .iter()
                .map(|(name, value)| json!({ "name": name, "value": value, "verified_at": null }))
                .collect(),
        )
    }

    /// Build a tiny valid PNG fixture for profile image uploads.
    fn encoded_test_image() -> Vec<u8> {
        let image = ImageBuffer::from_fn(2, 2, |x, y| {
            if (x + y) % 2 == 0 {
                Rgba([220_u8, 20, 60, 255])
            } else {
                Rgba([20_u8, 80, 220, 255])
            }
        });
        let mut bytes = Cursor::new(Vec::new());
        image.write_to(&mut bytes, ImageFormat::Png).unwrap();
        bytes.into_inner()
    }

    /// Append one multipart file part to a test request body.
    fn append_multipart_file(
        body: &mut Vec<u8>,
        boundary: &str,
        name: &str,
        filename: &str,
        content_type: &str,
        bytes: &[u8],
    ) {
        body.extend_from_slice(
            format!(
                "--{boundary}\r\nContent-Disposition: form-data; name=\"{name}\"; filename=\"{filename}\"\r\nContent-Type: {content_type}\r\n\r\n"
            )
            .as_bytes(),
        );
        body.extend_from_slice(bytes);
        body.extend_from_slice(b"\r\n");
    }

    struct EndpointContext {
        postgresql: PostgreSQL,
        db: roosty_db::DbConnection,
        database_name: String,
        config: Config,
        _temp_dir: TempDir,
    }

    impl AsyncTestContext for EndpointContext {
        async fn setup() -> Self {
            let temp_dir = tempfile::Builder::new()
                .prefix("roosty-server-")
                .tempdir()
                .unwrap();
            let database_name = unique_name();
            let data_dir = temp_dir.path().join("data").join(&database_name);
            let password_file = temp_dir
                .path()
                .join("passwords")
                .join(format!("{database_name}.pgpass"));

            if let Some(parent) = password_file.parent() {
                std::fs::create_dir_all(parent).unwrap();
            }

            let settings = crate::test_postgres::settings(&data_dir, password_file);
            let mut postgresql = PostgreSQL::new(settings);

            postgresql.setup().await.unwrap();
            postgresql.start().await.unwrap();
            postgresql.create_database(&database_name).await.unwrap();

            let database_url = postgresql.settings().url(&database_name);
            let db = roosty_db::connect(&database_url).await.unwrap();
            Migrator::up(&db, None).await.unwrap();

            let password_hash = password::hash_password("password").unwrap();
            roosty_db::create_bootstrap_admin(&db, "admin", "admin@example.com", &password_hash)
                .await
                .unwrap();

            let config = Config {
                database_url,
                public_base_url: "https://localhost:4000".parse().unwrap(),
                listen_addr: SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 4000),
                infra_listen_addr: None,
                session_secret: "test-session-secret-change-me-000".to_owned(),
                token_pepper: "test-token-pepper-change-me-0000".to_owned(),
                object_storage_backend: "local".to_owned(),
                media_root: temp_dir.path().join("media").to_string_lossy().to_string(),
                registration_mode: "closed".to_owned(),
                federation_enabled: false,
                federation_key_encryption_secret: None,
                federation_allowed_domains: Vec::new(),
                federation_blocked_domains: Vec::new(),
                federation_delivery_max_age: time::Duration::days(7),
                instance_name: "Roosty Test".to_owned(),
                instance_description: Some("Endpoint test instance".to_owned()),
            };

            Self {
                postgresql,
                db,
                database_name,
                config,
                _temp_dir: temp_dir,
            }
        }

        async fn teardown(self) {
            self.db.close().await.unwrap();
            self.postgresql
                .drop_database(&self.database_name)
                .await
                .unwrap();
            self.postgresql.stop().await.unwrap();
        }
    }

    impl EndpointContext {
        fn app(&self) -> Router {
            crate::http::app_router(AppState::new(self.config.clone(), self.db.clone()), false)
        }

        async fn request(&self, request: Request<Body>) -> Response<Body> {
            self.app().oneshot(request).await.unwrap()
        }

        async fn form(&self, method: &str, uri: &str, fields: &[(&str, &str)]) -> Response<Body> {
            let body = form_urlencoded::Serializer::new(String::new())
                .extend_pairs(fields.iter().copied())
                .finish();

            self.request(
                Request::builder()
                    .method(method)
                    .uri(uri)
                    .header(CONTENT_TYPE, "application/x-www-form-urlencoded")
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
        }

        async fn json(&self, method: &str, uri: &str, body: Value) -> Response<Body> {
            self.request(
                Request::builder()
                    .method(method)
                    .uri(uri)
                    .header(CONTENT_TYPE, "application/json")
                    .body(Body::from(serde_json::to_vec(&body).unwrap()))
                    .unwrap(),
            )
            .await
        }

        async fn register_app(&self) -> RegisteredApp {
            let response = self
                .form(
                    "POST",
                    "/api/v1/apps",
                    &[
                        ("client_name", "Elk"),
                        ("redirect_uris", REDIRECT_URI),
                        ("scopes", "read write"),
                    ],
                )
                .await;

            assert_eq!(response.status(), StatusCode::OK);
            let body = json_body(response).await;

            RegisteredApp {
                client_id: body["client_id"].as_str().unwrap().to_owned(),
                client_secret: body["client_secret"].as_str().unwrap().to_owned(),
            }
        }

        async fn login(&self) -> String {
            let response = self
                .form(
                    "POST",
                    "/login",
                    &[("login", "admin"), ("password", "password"), ("next", "/")],
                )
                .await;

            assert_eq!(response.status(), StatusCode::SEE_OTHER);
            session_cookie(&response)
        }

        async fn authorize(&self, client_id: &str, cookie: &str) -> String {
            let body = form_urlencoded::Serializer::new(String::new())
                .extend_pairs([
                    ("response_type", "code"),
                    ("client_id", client_id),
                    ("redirect_uri", REDIRECT_URI),
                    ("scope", "read write"),
                    ("state", "test-state"),
                    ("code_challenge", CODE_CHALLENGE),
                    ("code_challenge_method", "S256"),
                ])
                .finish();
            let response = self
                .request(
                    Request::builder()
                        .method("POST")
                        .uri("/oauth/authorize")
                        .header(CONTENT_TYPE, "application/x-www-form-urlencoded")
                        .header(COOKIE, cookie)
                        .body(Body::from(body))
                        .unwrap(),
                )
                .await;

            assert_eq!(response.status(), StatusCode::SEE_OTHER);
            let redirect = Url::parse(&header_value(&response, LOCATION)).unwrap();
            assert_eq!(redirect.as_str().split('?').next().unwrap(), REDIRECT_URI);
            assert_eq!(
                redirect.query_pairs().find(|(name, _)| name == "state"),
                Some(("state".into(), "test-state".into()))
            );

            redirect
                .query_pairs()
                .find_map(|(name, value)| (name == "code").then(|| value.into_owned()))
                .expect("authorization redirect should include a code")
        }

        async fn authorize_without_pkce(&self, client_id: &str, cookie: &str) -> String {
            let body = form_urlencoded::Serializer::new(String::new())
                .extend_pairs([
                    ("response_type", "code"),
                    ("client_id", client_id),
                    ("redirect_uri", REDIRECT_URI),
                    ("scope", "read write follow push"),
                    ("state", "test-state"),
                ])
                .finish();
            let response = self
                .request(
                    Request::builder()
                        .method("POST")
                        .uri("/oauth/authorize")
                        .header(CONTENT_TYPE, "application/x-www-form-urlencoded")
                        .header(COOKIE, cookie)
                        .body(Body::from(body))
                        .unwrap(),
                )
                .await;

            assert_eq!(response.status(), StatusCode::SEE_OTHER);
            let redirect = Url::parse(&header_value(&response, LOCATION)).unwrap();
            redirect
                .query_pairs()
                .find_map(|(name, value)| (name == "code").then(|| value.into_owned()))
                .expect("authorization redirect should include a code")
        }

        async fn token(&self, app: &RegisteredApp, code: &str) -> String {
            let response = self
                .form(
                    "POST",
                    "/oauth/token",
                    &[
                        ("grant_type", "authorization_code"),
                        ("client_id", &app.client_id),
                        ("client_secret", &app.client_secret),
                        ("redirect_uri", REDIRECT_URI),
                        ("code", code),
                        ("code_verifier", CODE_VERIFIER),
                    ],
                )
                .await;

            assert_eq!(response.status(), StatusCode::OK);
            let body = json_body(response).await;
            assert_eq!(body["token_type"], "Bearer");
            body["access_token"].as_str().unwrap().to_owned()
        }

        async fn token_json_without_pkce(&self, app: &RegisteredApp, code: &str) -> String {
            let response = self
                .json(
                    "POST",
                    "/oauth/token",
                    serde_json::json!({
                        "grant_type": "authorization_code",
                        "client_id": app.client_id,
                        "client_secret": app.client_secret,
                        "redirect_uri": REDIRECT_URI,
                        "code": code,
                        "scope": "read write follow push",
                    }),
                )
                .await;

            assert_eq!(response.status(), StatusCode::OK);
            let body = json_body(response).await;
            assert_eq!(body["token_type"], "Bearer");
            body["access_token"].as_str().unwrap().to_owned()
        }

        async fn authenticated_token(&self) -> String {
            let app = self.register_app().await;
            let cookie = self.login().await;
            let code = self.authorize(&app.client_id, &cookie).await;
            self.token(&app, &code).await
        }
    }

    struct RegisteredApp {
        client_id: String,
        client_secret: String,
    }

    fn authorize_uri(client_id: &str) -> String {
        let query = form_urlencoded::Serializer::new(String::new())
            .extend_pairs([
                ("response_type", "code"),
                ("client_id", client_id),
                ("redirect_uri", REDIRECT_URI),
                ("scope", "read write"),
                ("state", "test-state"),
                ("code_challenge", CODE_CHALLENGE),
                ("code_challenge_method", "S256"),
            ])
            .finish();

        format!("/oauth/authorize?{query}")
    }

    fn header_value(response: &Response<Body>, name: axum::http::header::HeaderName) -> String {
        response
            .headers()
            .get(name)
            .unwrap()
            .to_str()
            .unwrap()
            .to_owned()
    }

    fn session_cookie(response: &Response<Body>) -> String {
        header_value(response, SET_COOKIE)
            .split(';')
            .next()
            .unwrap()
            .to_owned()
    }

    async fn json_body(response: Response<Body>) -> Value {
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        serde_json::from_slice(&body).unwrap()
    }

    fn unique_name() -> String {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time is before the Unix epoch")
            .as_nanos();

        format!("roosty_server_{}_{}", std::process::id(), timestamp)
    }
}

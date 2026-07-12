use askama::Template;
use axum::{
    Form, Json, Router,
    body::to_bytes,
    extract::{FromRequest, Query, Request, State},
    http::{HeaderMap, StatusCode, header},
    response::{Html, IntoResponse, Redirect, Response},
    routing::{get, post},
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use hmac::{Hmac, Mac};
use roost_core::{AccountId, RoostError};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use sha2::Sha256;
use time::{Duration, OffsetDateTime};
use url::form_urlencoded;

use crate::{http::AppState, password};

type HmacSha256 = Hmac<Sha256>;

const SESSION_COOKIE: &str = "roost_session";

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
    let account = match roost_db::find_local_account_by_login(&state.db, &form.login).await {
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
        Err(error) => server_error(RoostError::InvalidInput(error.to_string())),
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
    match roost_db::create_oauth_application(
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
<p>This application is requesting access to your Roost account.</p>
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
        match roost_db::find_oauth_application_by_client_id(&state.db, &params.client_id).await {
            Ok(Some(app)) => app,
            Ok(None) => {
                return oauth_error(StatusCode::BAD_REQUEST, "invalid_client", "unknown client");
            }
            Err(error) => return server_error(error),
        };

    if roost_db::find_local_account_by_id(&state.db, account_id)
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
        Err(error) => server_error(RoostError::InvalidInput(error.to_string())),
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
    let code = match roost_db::create_authorization_code(
        &state.db,
        &state.config.token_pepper,
        roost_db::NewAuthorizationCode {
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
) -> Result<roost_db::OAuthApplication, Response> {
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

    let app = roost_db::find_oauth_application_by_client_id(&state.db, &params.client_id)
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

    let app = match roost_db::find_oauth_application_by_client_id(&state.db, &form.client_id).await
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
        let supplied_hash = match roost_db::secret_hash(&state.config.token_pepper, secret) {
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

    let Some((account_id, scopes, challenge, method)) = (match roost_db::consume_authorization_code(
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
    }) else {
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
                .is_none_or(|verifier| roost_db::pkce_s256_challenge(verifier) != challenge))
    {
        return oauth_error(
            StatusCode::BAD_REQUEST,
            "invalid_grant",
            "invalid PKCE verifier",
        );
    }

    match roost_db::create_access_token(
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
    match roost_db::revoke_access_token(&state.db, &state.config.token_pepper, &form.token).await {
        Ok(()) => StatusCode::OK.into_response(),
        Err(error) => server_error(error),
    }
}

#[derive(Serialize)]
struct AccountResponse {
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
    followers_count: u64,
    following_count: u64,
    statuses_count: u64,
    last_status_at: Option<String>,
    source: AccountSource,
    role: AccountRole,
}

#[derive(Serialize)]
struct AccountSource {
    note: String,
    fields: Vec<serde_json::Value>,
    privacy: String,
    sensitive: bool,
    language: String,
    follow_requests_count: u64,
}

#[derive(Serialize)]
struct AccountRole {
    id: String,
    name: String,
    color: String,
    permissions: String,
    highlighted: bool,
}

async fn verify_credentials(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let bearer = match bearer_token(&headers) {
        Some(token) => token,
        None => {
            return oauth_error(
                StatusCode::UNAUTHORIZED,
                "invalid_token",
                "missing bearer token",
            );
        }
    };

    let account =
        match roost_db::find_account_by_access_token(&state.db, &state.config.token_pepper, bearer)
            .await
        {
            Ok(Some((account, _scopes))) => account,
            Ok(None) => {
                return oauth_error(StatusCode::UNAUTHORIZED, "invalid_token", "invalid token");
            }
            Err(error) => return server_error(error),
        };

    let account_url = match state
        .config
        .public_base_url
        .join(&format!("@{}", account.username))
    {
        Ok(url) => url.to_string(),
        Err(_) => format!("{}/@{}", state.config.public_base_url, account.username),
    };

    Json(AccountResponse {
        id: account.id.0.to_string(),
        username: account.username.clone(),
        acct: account.username.clone(),
        display_name: account.username,
        locked: false,
        bot: false,
        discoverable: true,
        group: false,
        created_at: "1970-01-01T00:00:00.000Z".to_owned(),
        note: String::new(),
        url: account_url,
        avatar: String::new(),
        avatar_static: String::new(),
        header: String::new(),
        header_static: String::new(),
        followers_count: 0,
        following_count: 0,
        statuses_count: 0,
        last_status_at: None,
        source: AccountSource {
            note: String::new(),
            fields: Vec::new(),
            privacy: "public".to_owned(),
            sensitive: false,
            language: "en".to_owned(),
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
    .into_response()
}

fn account_id_from_session(
    state: &AppState,
    headers: &HeaderMap,
) -> Result<Option<AccountId>, RoostError> {
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

fn session_cookie(state: &AppState, account_id: AccountId) -> Result<String, RoostError> {
    let expires_at = OffsetDateTime::now_utc() + Duration::days(7);
    let payload = format!("{}.{}", account_id.0, expires_at.unix_timestamp());
    let signature = sign(&state.config.session_secret, &payload)?;
    Ok(format!(
        "{SESSION_COOKIE}={payload}.{signature}; Path=/; Max-Age=604800; HttpOnly; Secure; SameSite=Lax"
    ))
}

fn parse_session_cookie(secret: &str, value: &str) -> Result<Option<AccountId>, RoostError> {
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

fn sign(secret: &str, payload: &str) -> Result<String, RoostError> {
    let mut mac = HmacSha256::new_from_slice(secret.as_bytes())
        .map_err(|error| RoostError::InvalidInput(error.to_string()))?;
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

fn server_error(error: RoostError) -> Response {
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
        net::{IpAddr, Ipv4Addr, SocketAddr},
        time::{Duration as StdDuration, SystemTime, UNIX_EPOCH},
    };

    use axum::{
        Router,
        body::{Body, to_bytes},
        http::{
            Request, Response, StatusCode,
            header::{AUTHORIZATION, CONTENT_TYPE, COOKIE, LOCATION, SET_COOKIE},
        },
    };
    use postgresql_embedded::{PostgreSQL, SettingsBuilder, V18};
    use roost_migration::Migrator;
    use sea_orm_migration::MigratorTrait;
    use serde_json::Value;
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
            "https://localhost:4001/api/roost.localhost:4000/oauth/https%3A%2F%2Flocalhost%3A4001";
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
        assert!(session_cookie(&response).starts_with("roost_session="));
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

    struct EndpointContext {
        postgresql: PostgreSQL,
        db: roost_db::DbConnection,
        database_name: String,
        config: Config,
        _temp_dir: TempDir,
    }

    impl AsyncTestContext for EndpointContext {
        async fn setup() -> Self {
            let temp_dir = tempfile::Builder::new()
                .prefix("roost-server-")
                .tempdir()
                .unwrap();
            let install_cache_root = std::env::var_os("CARGO_TARGET_TMPDIR")
                .map(std::path::PathBuf::from)
                .unwrap_or_else(|| std::env::temp_dir().join("roost-target-tmp"));
            let install_cache = install_cache_root.join("embedded-postgres").join("install");
            let database_name = unique_name();
            let data_dir = temp_dir.path().join("data").join(&database_name);
            let password_file = temp_dir
                .path()
                .join("passwords")
                .join(format!("{database_name}.pgpass"));

            if let Some(parent) = password_file.parent() {
                std::fs::create_dir_all(parent).unwrap();
            }

            let settings = SettingsBuilder::new()
                .version((*V18).clone())
                .installation_dir(install_cache)
                .data_dir(&data_dir)
                .password_file(password_file)
                .timeout(Some(StdDuration::from_secs(30)))
                .build();
            let mut postgresql = PostgreSQL::new(settings);

            postgresql.setup().await.unwrap();
            postgresql.start().await.unwrap();
            postgresql.create_database(&database_name).await.unwrap();

            let database_url = postgresql.settings().url(&database_name);
            let db = roost_db::connect(&database_url).await.unwrap();
            Migrator::up(&db, None).await.unwrap();

            let password_hash = password::hash_password("password").unwrap();
            roost_db::create_bootstrap_admin(&db, "admin", "admin@example.com", &password_hash)
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
                media_root: "./media".to_owned(),
                registration_mode: "closed".to_owned(),
                federation_enabled: false,
                instance_name: "Roost Test".to_owned(),
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

        format!("roost_server_{}_{}", std::process::id(), timestamp)
    }
}

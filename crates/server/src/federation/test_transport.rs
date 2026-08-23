//! In-process ActivityPub transport used only by federation integration tests.

use std::{
    collections::{HashMap, HashSet},
    sync::{LazyLock, Mutex},
};

use axum::{
    body::{Body, to_bytes},
    http::{HeaderMap, Request, StatusCode},
};
use roosty_core::RoostyError;
use serde_json::Value;
use tower::ServiceExt;
use url::Url;

use super::signature::SignatureFormat;
use crate::http::{AppState, DatabaseContext, app_router};

#[derive(Clone)]
struct RegisteredInbox {
    state: AppState,
    database: DatabaseContext,
}

static INBOXES: LazyLock<Mutex<HashMap<String, RegisteredInbox>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
static REJECT_LEGACY_ONCE: LazyLock<Mutex<HashSet<String>>> =
    LazyLock::new(|| Mutex::new(HashSet::new()));
static REQUESTS: LazyLock<Mutex<Vec<RecordedRequest>>> = LazyLock::new(|| Mutex::new(Vec::new()));

/// One outbound request observed before optional in-process inbox processing.
#[derive(Clone, Debug)]
pub(super) struct RecordedRequest {
    pub(super) format: SignatureFormat,
    pub(super) headers: HeaderMap,
    pub(super) target: Url,
    pub(super) body: Vec<u8>,
}

/// Register an isolated test instance to receive signed requests for one host.
pub(super) fn register_inbox(host: &str, state: AppState, database: DatabaseContext) {
    let mut inboxes = INBOXES
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    inboxes.insert(host.to_owned(), RegisteredInbox { state, database });
}

/// Clear registered recipients after a test to prevent cross-test delivery.
pub(super) fn clear_inboxes() {
    let mut inboxes = INBOXES
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    inboxes.clear();
    REJECT_LEGACY_ONCE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clear();
    REQUESTS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clear();
}

/// Reject the next legacy request for a host before it can reach inbox processing.
pub(super) fn reject_legacy_once(host: &str) {
    REJECT_LEGACY_ONCE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .insert(host.to_owned());
}

/// Return all requests recorded by the in-process transport.
pub(super) fn recorded_requests() -> Vec<RecordedRequest> {
    REQUESTS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone()
}

/// Serve one in-process ActivityPub GET for federation discovery tests.
pub(super) async fn fetch_if_registered(url: &Url) -> Option<Result<Value, RoostyError>> {
    let inbox = {
        let inboxes = INBOXES
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        inboxes.get(url.host_str()?).cloned()
    }?;
    let path = match url.query() {
        Some(query) => format!("{}?{query}", url.path()),
        None => url.path().to_owned(),
    };
    let request = match Request::builder()
        .method("GET")
        .uri(path)
        .header("accept", "application/activity+json")
        .body(Body::empty())
    {
        Ok(request) => request,
        Err(error) => return Some(Err(RoostyError::InvalidInput(error.to_string()))),
    };
    let response = match app_router(inbox.state, inbox.database, false)
        .oneshot(request)
        .await
    {
        Ok(response) => response,
        Err(error) => match error {},
    };
    if !response.status().is_success() {
        return Some(Err(RoostyError::InvalidInput(format!(
            "test federation GET returned {}",
            response.status()
        ))));
    }
    let body = match to_bytes(response.into_body(), 1_048_576).await {
        Ok(body) => body,
        Err(error) => return Some(Err(RoostyError::InvalidInput(error.to_string()))),
    };
    Some(
        serde_json::from_slice(&body).map_err(|error| RoostyError::InvalidInput(error.to_string())),
    )
}

/// Forward one already signed request to an in-process recipient when its host is registered.
pub(super) async fn deliver_if_registered(
    url: &Url,
    host: &str,
    format: SignatureFormat,
    signed_headers: &HeaderMap,
    body: Vec<u8>,
) -> Option<Result<StatusCode, RoostyError>> {
    let inbox = {
        let inboxes = INBOXES
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        inboxes.get(host).cloned()
    }?;
    REQUESTS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .push(RecordedRequest {
            format,
            headers: signed_headers.clone(),
            target: url.clone(),
            body: body.clone(),
        });
    if format == SignatureFormat::Legacy
        && REJECT_LEGACY_ONCE
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(host)
    {
        return Some(Ok(StatusCode::BAD_REQUEST));
    }
    let path = match url.query() {
        Some(query) => format!("{}?{query}", url.path()),
        None => url.path().to_owned(),
    };
    let mut request = match Request::builder()
        .method("POST")
        .uri(path)
        .header("host", host)
        .header("content-type", "application/activity+json")
        .body(Body::from(body))
    {
        Ok(request) => request,
        Err(error) => return Some(Err(RoostyError::InvalidInput(error.to_string()))),
    };
    request.headers_mut().extend(signed_headers.clone());
    let response = match app_router(inbox.state, inbox.database, false)
        .oneshot(request)
        .await
    {
        Ok(response) => response,
        Err(error) => match error {},
    };
    Some(Ok(response.status()))
}

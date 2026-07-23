//! In-process ActivityPub transport used only by federation integration tests.

use std::{
    collections::HashMap,
    sync::{LazyLock, Mutex},
};

use axum::{
    body::{Body, to_bytes},
    http::Request,
};
use roosty_core::RoostyError;
use serde_json::Value;
use tower::ServiceExt;
use url::Url;

use crate::http::AppState;

use super::process_inbox;

static INBOXES: LazyLock<Mutex<HashMap<String, AppState>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Register an isolated test instance to receive signed requests for one host.
pub(super) fn register_inbox(host: &str, state: AppState) {
    let mut inboxes = INBOXES
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    inboxes.insert(host.to_owned(), state);
}

/// Clear registered recipients after a test to prevent cross-test delivery.
pub(super) fn clear_inboxes() {
    let mut inboxes = INBOXES
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    inboxes.clear();
}

/// Serve one in-process ActivityPub GET for federation discovery tests.
pub(super) async fn fetch_if_registered(url: &Url) -> Option<Result<Value, RoostyError>> {
    let state = {
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
    let response = match super::router().with_state(state).oneshot(request).await {
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
    date: &str,
    digest: &str,
    signature: &str,
    body: Vec<u8>,
) -> Option<Result<(), RoostyError>> {
    let state = {
        let inboxes = INBOXES
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        inboxes.get(host).cloned()
    }?;
    let path = match url.query() {
        Some(query) => format!("{}?{query}", url.path()),
        None => url.path().to_owned(),
    };
    let request = match Request::builder()
        .method("POST")
        .uri(path)
        .header("host", host)
        .header("date", date)
        .header("digest", digest)
        .header("signature", signature)
        .header("content-type", "application/activity+json")
        .body(Body::from(body))
    {
        Ok(request) => request,
        Err(error) => return Some(Err(RoostyError::InvalidInput(error.to_string()))),
    };
    let response = process_inbox(&state, request).await;
    if response.status().is_success() {
        Some(Ok(()))
    } else {
        Some(Err(RoostyError::InvalidInput(format!(
            "test inbox returned {}",
            response.status()
        ))))
    }
}

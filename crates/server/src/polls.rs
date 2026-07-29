//! Mastodon-compatible poll lookup and voting endpoints.

use axum::{
    Json, Router,
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
};
use roosty_core::{AccountId, RoostyError};
use roosty_db::{
    PollStatus, PollViewerState, PollVoteError, StatusPoll, enqueue_job_in_transaction,
    enqueue_poll_update, find_local_status_by_id, find_poll_by_id, find_remote_status_by_id,
    poll_viewer_state, remote_status_visible_to_account, vote_in_poll,
};
use sea_orm::{AccessMode, ConnectionTrait, TransactionTrait};
use serde::{Deserialize, Serialize};
use std::borrow::Cow;
use thiserror::Error;
use time::{Duration, OffsetDateTime};
use uuid::Uuid;

use crate::{
    auth::{AuthenticatedAccessToken, OptionalAuthenticatedAccount},
    federation::{StatusActivityKind, enqueue_status_activity_in_transaction, prepare_poll_vote},
    http::AppState,
    notifications::publish_committed_notification,
    statuses::{parse_request_body, status_visible_to_viewer_on},
};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/v1/polls/{poll_id}", get(show_poll))
        .route("/api/v1/polls/{poll_id}/votes", post(vote_poll))
}

#[derive(Deserialize)]
struct PollPath {
    poll_id: Uuid,
}

#[derive(Deserialize)]
struct VoteInput {
    choices: Vec<u32>,
}

#[derive(Deserialize)]
struct PollJobPayload {
    poll_id: Uuid,
}

#[derive(Clone, Serialize)]
pub(crate) struct PollResponse {
    id: String,
    #[serde(with = "time::serde::rfc3339::option")]
    expires_at: Option<OffsetDateTime>,
    expired: bool,
    multiple: bool,
    votes_count: u64,
    voters_count: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    voted: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    own_votes: Option<Vec<u32>>,
    options: Vec<PollOptionResponse>,
    emojis: Vec<serde_json::Value>,
}

#[derive(Clone, Serialize)]
struct PollOptionResponse {
    title: String,
    votes_count: Option<u64>,
}

#[derive(Debug, Error)]
enum PollApiError {
    #[error("{0}")]
    InvalidInput(Cow<'static, str>),
    #[error("{0}")]
    Forbidden(Cow<'static, str>),
    #[error("Record not found")]
    NotFound,
    #[error(transparent)]
    Database(#[from] RoostyError),
    #[error(transparent)]
    SeaOrm(#[from] sea_orm::DbErr),
}

impl IntoResponse for PollApiError {
    fn into_response(self) -> Response {
        let status = match self {
            Self::InvalidInput(_) => StatusCode::UNPROCESSABLE_ENTITY,
            Self::Forbidden(_) => StatusCode::FORBIDDEN,
            Self::NotFound => StatusCode::NOT_FOUND,
            Self::Database(_) | Self::SeaOrm(_) => StatusCode::INTERNAL_SERVER_ERROR,
        };
        if matches!(self, Self::Database(_) | Self::SeaOrm(_)) {
            tracing::error!(error = %self, "poll API database operation failed");
        }
        (
            status,
            Json(serde_json::json!({
                "error": self.to_string(),
                "error_description": self.to_string(),
            })),
        )
            .into_response()
    }
}

async fn show_poll(
    State(state): State<AppState>,
    OptionalAuthenticatedAccount(viewer): OptionalAuthenticatedAccount,
    Path(path): Path<PollPath>,
) -> Result<Json<PollResponse>, PollApiError> {
    let txn = state
        .db
        .begin_with_config(None, Some(AccessMode::ReadOnly))
        .await?;
    let poll = find_poll_by_id(&txn, path.poll_id)
        .await?
        .ok_or(PollApiError::NotFound)?;
    let viewer = viewer.as_ref().map(|account| account.id);
    ensure_visible(&txn, &poll, viewer).await?;
    let response = poll_response(&txn, poll, viewer).await?;
    txn.commit().await?;
    Ok(Json(response))
}

async fn vote_poll(
    State(state): State<AppState>,
    token: AuthenticatedAccessToken,
    Path(path): Path<PollPath>,
    request: axum::extract::Request,
) -> Result<Json<PollResponse>, PollApiError> {
    if !has_write_status_scope(&token.grant.scopes) {
        return Err(PollApiError::Forbidden(
            "This action is outside the authorized scopes".into(),
        ));
    }
    let input: VoteInput = parse_request_body(request)
        .await
        .map_err(|error| PollApiError::InvalidInput(error.to_string().into()))?;
    let txn = state.db.begin().await?;
    let poll = find_poll_by_id(&txn, path.poll_id)
        .await?
        .ok_or(PollApiError::NotFound)?;
    ensure_visible(&txn, &poll, Some(token.grant.account.id)).await?;
    let poll = vote_in_poll(&txn, poll.id, token.grant.account.id, &input.choices)
        .await
        .map_err(map_vote_error)?;
    let delivery = match poll.status {
        PollStatus::Remote(_) => Some(
            prepare_poll_vote(&state, &txn, token.grant.account.id, &poll, &input.choices)
                .await
                .map_err(PollApiError::Database)?,
        ),
        PollStatus::Local(_) => None,
    };
    if let Some(job) = delivery {
        enqueue_job_in_transaction(&txn, job).await?;
    } else if !poll.hide_totals {
        enqueue_poll_update(
            &txn,
            poll.id,
            OffsetDateTime::now_utc() + Duration::minutes(3),
        )
        .await?;
    }
    let response = poll_response(&txn, poll, Some(token.grant.account.id)).await?;
    txn.commit().await?;
    Ok(Json(response))
}

pub(crate) async fn poll_response(
    db: &impl ConnectionTrait,
    poll: StatusPoll,
    viewer: Option<AccountId>,
) -> Result<PollResponse, RoostyError> {
    let viewer_state = match viewer {
        Some(account_id) => Some(poll_viewer_state(db, poll.id, account_id).await?),
        None => None,
    };
    Ok(poll_response_with_viewer(poll, viewer_state))
}

fn poll_response_with_viewer(poll: StatusPoll, viewer: Option<PollViewerState>) -> PollResponse {
    let expired = poll.expired(OffsetDateTime::now_utc());
    let show_totals = expired || !poll.hide_totals;
    PollResponse {
        id: poll.id.to_string(),
        expires_at: poll.expires_at,
        expired,
        multiple: poll.multiple,
        votes_count: poll.votes_count(),
        voters_count: poll.voters_count,
        voted: viewer.as_ref().map(|viewer| viewer.voted),
        own_votes: viewer.map(|viewer| viewer.own_votes),
        options: poll
            .options
            .into_iter()
            .map(|option| PollOptionResponse {
                title: option.title,
                votes_count: show_totals.then_some(option.votes_count),
            })
            .collect(),
        emojis: Vec::new(),
    }
}

async fn ensure_visible(
    db: &impl ConnectionTrait,
    poll: &StatusPoll,
    viewer: Option<AccountId>,
) -> Result<(), PollApiError> {
    let visible = match poll.status {
        PollStatus::Local(status_id) => {
            let status = find_local_status_by_id(db, status_id)
                .await?
                .ok_or(PollApiError::NotFound)?;
            status_visible_to_viewer_on(db, &status, viewer).await?
        }
        PollStatus::Remote(status_id) => {
            let status = find_remote_status_by_id(db, status_id)
                .await?
                .ok_or(PollApiError::NotFound)?;
            match viewer {
                Some(account_id) => {
                    remote_status_visible_to_account(db, &status, account_id).await?
                }
                None => matches!(
                    status.visibility,
                    roosty_db::StatusVisibility::Public | roosty_db::StatusVisibility::Unlisted
                ),
            }
        }
    };
    if visible {
        Ok(())
    } else {
        Err(PollApiError::NotFound)
    }
}

fn has_write_status_scope(scopes: &str) -> bool {
    scopes
        .split_ascii_whitespace()
        .any(|scope| matches!(scope, "write" | "write:statuses"))
}

fn map_vote_error(error: PollVoteError) -> PollApiError {
    match error {
        PollVoteError::NotFound => PollApiError::NotFound,
        PollVoteError::Expired
        | PollVoteError::AlreadyVoted
        | PollVoteError::InvalidChoice
        | PollVoteError::MultipleChoices => PollApiError::InvalidInput(error.to_string().into()),
        PollVoteError::Database(error) => PollApiError::Database(error),
        PollVoteError::SeaOrm(error) => PollApiError::Database(error.into()),
    }
}

/// Expire one poll exactly once and publish its committed notifications.
pub(crate) async fn expire_poll_job(
    state: &AppState,
    payload: serde_json::Value,
) -> Result<(), RoostyError> {
    let payload: PollJobPayload = serde_json::from_value(payload)
        .map_err(|_| RoostyError::InvalidInput("invalid poll expiration payload".to_owned()))?;
    let txn = state.db.begin().await?;
    let expiration = roosty_db::expire_poll(&txn, payload.poll_id).await?;
    if let Some(expiration) = &expiration
        && expiration.newly_expired
        && matches!(expiration.poll.status, PollStatus::Local(_))
    {
        enqueue_poll_update(&txn, expiration.poll.id, OffsetDateTime::now_utc()).await?;
    }
    txn.commit().await?;
    if let Some(expiration) = expiration {
        for notification in expiration.notifications {
            publish_committed_notification(state, notification.account_id, notification).await?;
        }
    }
    Ok(())
}

/// Fan out one coalesced local Question tally update, including remote voters.
pub(crate) async fn publish_poll_update_job(
    state: &AppState,
    payload: serde_json::Value,
) -> Result<(), RoostyError> {
    let payload: PollJobPayload = serde_json::from_value(payload)
        .map_err(|_| RoostyError::InvalidInput("invalid poll update payload".to_owned()))?;
    let txn = state.db.begin().await?;
    let Some(poll) = find_poll_by_id(&txn, payload.poll_id).await? else {
        txn.commit().await?;
        return Ok(());
    };
    let PollStatus::Local(status_id) = poll.status else {
        txn.commit().await?;
        return Ok(());
    };
    let Some(status) = find_local_status_by_id(&txn, status_id).await? else {
        txn.commit().await?;
        return Ok(());
    };
    let voter_ids = roosty_db::local_poll_remote_voter_ids(&txn, poll.id).await?;
    let voters = roosty_db::remote_actors_by_id(&txn, voter_ids).await?;
    enqueue_status_activity_in_transaction(
        state,
        &txn,
        &status,
        StatusActivityKind::Update,
        &voters,
    )
    .await?;
    txn.commit().await?;
    Ok(())
}

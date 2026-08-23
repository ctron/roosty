//! Typed persistence and concurrency control for local and federated status polls.

use std::result;

use crate::{
    JobKind, LocalNotification, LocalNotificationType, NewJob, enqueue_job_in_transaction,
    entity::{
        local_notification, local_status, remote_status, scheduled_status_poll,
        scheduled_status_poll_option, status_poll, status_poll_option, status_poll_vote,
    },
    local_notification_from_model,
};
use roosty_core::{AccountId, Result, RoostyError, StatusId};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, DatabaseBackend, DatabaseTransaction,
    EntityTrait, IntoActiveModel, ModelTrait, QueryFilter, QueryOrder, QuerySelect, Set, Statement,
};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use time::{Duration, OffsetDateTime};
use unicode_segmentation::UnicodeSegmentation;
use uuid::Uuid;

pub const MIN_POLL_EXPIRATION_SECONDS: i64 = 300;
pub const MAX_POLL_EXPIRATION_SECONDS: i64 = 2_629_746;
pub const MAX_POLL_OPTIONS: usize = 4;
pub const MAX_POLL_OPTION_GRAPHEMES: usize = 50;

/// Local or cached-remote status carrying a poll.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PollStatus {
    Local(StatusId),
    Remote(StatusId),
}

/// One poll option in stable client order.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PollOption {
    pub position: u32,
    pub title: String,
    pub votes_count: u64,
}

/// A poll and viewer-independent authoritative state.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StatusPoll {
    pub id: Uuid,
    pub status: PollStatus,
    pub multiple: bool,
    pub hide_totals: bool,
    pub expires_at: Option<OffsetDateTime>,
    pub closed_at: Option<OffsetDateTime>,
    pub voters_count: Option<u64>,
    pub options: Vec<PollOption>,
    pub updated_at: OffsetDateTime,
}

impl StatusPoll {
    pub fn expired(&self, now: OffsetDateTime) -> bool {
        self.closed_at.is_some() || self.expires_at.is_some_and(|expires_at| expires_at <= now)
    }

    pub fn votes_count(&self) -> u64 {
        self.options.iter().map(|option| option.votes_count).sum()
    }
}

/// Validated local poll input shared by immediate and scheduled publication.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct NewStatusPoll {
    pub options: Vec<String>,
    pub expires_in: i64,
    #[serde(default)]
    pub multiple: bool,
    #[serde(default)]
    pub hide_totals: bool,
}

/// Remote Question state accepted after ActivityPub ownership and shape validation.
#[derive(Clone, Debug)]
pub struct RemoteStatusPoll {
    pub options: Vec<(String, u64)>,
    pub multiple: bool,
    pub expires_at: Option<OffsetDateTime>,
    pub closed_at: Option<OffsetDateTime>,
    pub voters_count: Option<u64>,
}

/// Viewer-specific vote state projected into Mastodon Poll entities.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct PollViewerState {
    pub voted: bool,
    pub own_votes: Vec<u32>,
}

/// Stable vote rejection categories used by the HTTP and federation boundaries.
#[derive(Debug, Error)]
pub enum PollVoteError {
    #[error("poll does not exist")]
    NotFound,
    #[error("poll has already ended")]
    Expired,
    #[error("account has already voted in this poll")]
    AlreadyVoted,
    #[error("poll choice is invalid")]
    InvalidChoice,
    #[error("a single-choice poll accepts exactly one choice")]
    MultipleChoices,
    #[error(transparent)]
    Database(#[from] RoostyError),
    #[error(transparent)]
    SeaOrm(#[from] sea_orm::DbErr),
}

/// Result of one expiration job transaction.
pub struct PollExpiration {
    pub poll: StatusPoll,
    pub notifications: Vec<LocalNotification>,
    pub newly_expired: bool,
    pub next_attempt_at: Option<OffsetDateTime>,
}

pub fn validate_poll(input: &NewStatusPoll) -> Result<()> {
    if !(2..=MAX_POLL_OPTIONS).contains(&input.options.len()) {
        return Err(RoostyError::InvalidInput(
            "polls require between 2 and 4 options".to_owned(),
        ));
    }
    if !(MIN_POLL_EXPIRATION_SECONDS..=MAX_POLL_EXPIRATION_SECONDS).contains(&input.expires_in) {
        return Err(RoostyError::InvalidInput(
            "poll expiration must be between 300 and 2629746 seconds".to_owned(),
        ));
    }
    let normalized = input
        .options
        .iter()
        .map(|option| option.trim())
        .collect::<Vec<_>>();
    if normalized.iter().any(|option| {
        option.is_empty() || option.graphemes(true).count() > MAX_POLL_OPTION_GRAPHEMES
    }) {
        return Err(RoostyError::InvalidInput(
            "poll options must contain between 1 and 50 characters".to_owned(),
        ));
    }
    let mut unique = normalized.clone();
    unique.sort_unstable();
    unique.dedup();
    if unique.len() != normalized.len() {
        return Err(RoostyError::InvalidInput(
            "poll options must be unique".to_owned(),
        ));
    }
    Ok(())
}

/// Attach a new local poll and its durable expiration job to an inserted status.
pub async fn create_local_poll(
    txn: &DatabaseTransaction,
    status_id: StatusId,
    input: &NewStatusPoll,
) -> Result<StatusPoll> {
    validate_poll(input)?;
    let now = OffsetDateTime::now_utc();
    let expires_at = now + Duration::seconds(input.expires_in);
    let poll_id = Uuid::now_v7();
    status_poll::ActiveModel {
        id: Set(poll_id),
        local_status_id: Set(Some(status_id.0)),
        remote_status_id: Set(None),
        multiple: Set(input.multiple),
        hide_totals: Set(input.hide_totals),
        expires_at: Set(Some(expires_at)),
        closed_at: Set(None),
        notifications_sent_at: Set(None),
        voters_count: Set(Some(0)),
        created_at: Set(now),
        updated_at: Set(now),
    }
    .insert(txn)
    .await?;
    replace_options(
        txn,
        poll_id,
        input.options.iter().map(|title| (title.as_str(), 0)),
    )
    .await?;
    enqueue_poll_expiration(txn, poll_id, expires_at).await?;
    find_poll_by_id(txn, poll_id)
        .await?
        .ok_or_else(|| RoostyError::InvalidInput("created poll was not found".to_owned()))
}

/// Store a poll with a scheduled status for later atomic publication.
pub async fn create_scheduled_poll(
    txn: &impl ConnectionTrait,
    scheduled_status_id: Uuid,
    input: &NewStatusPoll,
) -> Result<()> {
    validate_poll(input)?;
    scheduled_status_poll::ActiveModel {
        scheduled_status_id: Set(scheduled_status_id),
        multiple: Set(input.multiple),
        hide_totals: Set(input.hide_totals),
        expires_in: Set(input.expires_in),
    }
    .insert(txn)
    .await?;
    for (position, title) in input.options.iter().enumerate() {
        scheduled_status_poll_option::ActiveModel {
            scheduled_status_id: Set(scheduled_status_id),
            position: Set(position as i32),
            title: Set(title.trim().to_owned()),
        }
        .insert(txn)
        .await?;
    }
    Ok(())
}

/// Load the typed poll parameters retained with a scheduled status.
pub async fn scheduled_poll(
    db: &impl ConnectionTrait,
    scheduled_status_id: Uuid,
) -> Result<Option<NewStatusPoll>> {
    let Some(poll) = scheduled_status_poll::Entity::find_by_id(scheduled_status_id)
        .one(db)
        .await?
    else {
        return Ok(None);
    };
    let options = scheduled_status_poll_option::Entity::find()
        .filter(scheduled_status_poll_option::Column::ScheduledStatusId.eq(scheduled_status_id))
        .order_by_asc(scheduled_status_poll_option::Column::Position)
        .all(db)
        .await?
        .into_iter()
        .map(|option| option.title)
        .collect();
    Ok(Some(NewStatusPoll {
        options,
        expires_in: poll.expires_in,
        multiple: poll.multiple,
        hide_totals: poll.hide_totals,
    }))
}

pub async fn find_poll_by_id(
    db: &impl ConnectionTrait,
    poll_id: Uuid,
) -> Result<Option<StatusPoll>> {
    let Some(model) = status_poll::Entity::find_by_id(poll_id).one(db).await? else {
        return Ok(None);
    };
    hydrate_poll(db, model).await.map(Some)
}

pub async fn find_poll_for_status(
    db: &impl ConnectionTrait,
    status: PollStatus,
) -> Result<Option<StatusPoll>> {
    let query = status_poll::Entity::find();
    let query = match status {
        PollStatus::Local(id) => query.filter(status_poll::Column::LocalStatusId.eq(id.0)),
        PollStatus::Remote(id) => query.filter(status_poll::Column::RemoteStatusId.eq(id.0)),
    };
    let Some(model) = query.one(db).await? else {
        return Ok(None);
    };
    hydrate_poll(db, model).await.map(Some)
}

/// Return local viewer choices without exposing any other voter's identity.
pub async fn poll_viewer_state(
    db: &impl ConnectionTrait,
    poll_id: Uuid,
    account_id: AccountId,
) -> Result<PollViewerState> {
    let mut own_votes = status_poll_vote::Entity::find()
        .filter(status_poll_vote::Column::PollId.eq(poll_id))
        .filter(status_poll_vote::Column::LocalAccountId.eq(account_id.0))
        .order_by_asc(status_poll_vote::Column::ChoicePosition)
        .all(db)
        .await?
        .into_iter()
        .map(|vote| vote.choice_position as u32)
        .collect::<Vec<_>>();
    own_votes.dedup();
    let author = poll_author(db, poll_id).await?;
    Ok(PollViewerState {
        voted: author == Some(account_id) || !own_votes.is_empty(),
        own_votes,
    })
}

/// Cast a local account's choices under an exclusive poll lock.
pub async fn vote_in_poll(
    txn: &DatabaseTransaction,
    poll_id: Uuid,
    account_id: AccountId,
    choices: &[u32],
) -> result::Result<StatusPoll, PollVoteError> {
    vote(txn, poll_id, Some(account_id), None, choices, None).await
}

/// Record a verified remote vote on a locally authored poll.
pub async fn record_remote_poll_vote(
    txn: &DatabaseTransaction,
    poll_id: Uuid,
    actor_id: AccountId,
    choice: u32,
    activitypub_id: &str,
) -> result::Result<StatusPoll, PollVoteError> {
    vote(
        txn,
        poll_id,
        None,
        Some(actor_id),
        &[choice],
        Some(activitypub_id),
    )
    .await
}

async fn vote(
    txn: &DatabaseTransaction,
    poll_id: Uuid,
    local_account_id: Option<AccountId>,
    remote_actor_id: Option<AccountId>,
    choices: &[u32],
    activitypub_id: Option<&str>,
) -> result::Result<StatusPoll, PollVoteError> {
    let Some(model) = status_poll::Entity::find_by_id(poll_id)
        .lock_exclusive()
        .one(txn)
        .await?
    else {
        return Err(PollVoteError::NotFound);
    };
    if model.closed_at.is_some()
        || model
            .expires_at
            .is_some_and(|expires_at| expires_at <= OffsetDateTime::now_utc())
    {
        return Err(PollVoteError::Expired);
    }
    let mut choices = choices.to_vec();
    choices.sort_unstable();
    choices.dedup();
    if choices.is_empty() {
        return Err(PollVoteError::InvalidChoice);
    }
    if !model.multiple && choices.len() != 1 {
        return Err(PollVoteError::MultipleChoices);
    }
    let existing = match (local_account_id, remote_actor_id) {
        (Some(account), None) => {
            status_poll_vote::Entity::find()
                .filter(status_poll_vote::Column::PollId.eq(poll_id))
                .filter(status_poll_vote::Column::LocalAccountId.eq(account.0))
                .all(txn)
                .await?
        }
        (None, Some(actor)) => {
            status_poll_vote::Entity::find()
                .filter(status_poll_vote::Column::PollId.eq(poll_id))
                .filter(status_poll_vote::Column::RemoteActorId.eq(actor.0))
                .all(txn)
                .await?
        }
        _ => {
            return Err(PollVoteError::Database(RoostyError::InvalidInput(
                "poll voter identity is invalid".to_owned(),
            )));
        }
    };
    if !existing.is_empty() && (local_account_id.is_some() || !model.multiple) {
        return Err(PollVoteError::AlreadyVoted);
    }
    let options = status_poll_option::Entity::find()
        .filter(status_poll_option::Column::PollId.eq(poll_id))
        .all(txn)
        .await?;
    if choices.iter().any(|choice| {
        !options
            .iter()
            .any(|option| option.position == *choice as i32)
    }) {
        return Err(PollVoteError::InvalidChoice);
    }
    if choices.iter().any(|choice| {
        existing
            .iter()
            .any(|vote| vote.choice_position == *choice as i32)
    }) {
        return Err(PollVoteError::AlreadyVoted);
    }
    for choice in choices {
        status_poll_vote::ActiveModel {
            id: Set(Uuid::now_v7()),
            poll_id: Set(poll_id),
            choice_position: Set(choice as i32),
            local_account_id: Set(local_account_id.map(|id| id.0)),
            remote_actor_id: Set(remote_actor_id.map(|id| id.0)),
            activitypub_id: Set(activitypub_id.map(str::to_owned)),
            created_at: Set(OffsetDateTime::now_utc()),
        }
        .insert(txn)
        .await?;
        let option = status_poll_option::Entity::find_by_id((poll_id, choice as i32))
            .lock_exclusive()
            .one(txn)
            .await?
            .ok_or(PollVoteError::InvalidChoice)?;
        let mut active = option.into_active_model();
        active.votes_count = Set(active.votes_count.unwrap() + 1);
        active.update(txn).await?;
    }
    let mut active = model.into_active_model();
    if existing.is_empty() {
        active.voters_count = Set(active.voters_count.unwrap().map(|count| count + 1));
    }
    active.updated_at = Set(OffsetDateTime::now_utc());
    active.update(txn).await?;
    find_poll_by_id(txn, poll_id)
        .await?
        .ok_or(PollVoteError::NotFound)
}

/// Replace or remove a local status poll as part of a status edit.
pub async fn replace_local_poll(
    txn: &DatabaseTransaction,
    status_id: StatusId,
    input: Option<&NewStatusPoll>,
) -> Result<Option<StatusPoll>> {
    let existing = status_poll::Entity::find()
        .filter(status_poll::Column::LocalStatusId.eq(status_id.0))
        .lock_exclusive()
        .one(txn)
        .await?;
    let Some(input) = input else {
        if let Some(existing) = existing {
            existing.delete(txn).await?;
        }
        return Ok(None);
    };
    validate_poll(input)?;
    let now = OffsetDateTime::now_utc();
    let expires_at = now + Duration::seconds(input.expires_in);
    let poll_id = if let Some(existing) = existing {
        let current_options = status_poll_option::Entity::find()
            .filter(status_poll_option::Column::PollId.eq(existing.id))
            .order_by_asc(status_poll_option::Column::Position)
            .all(txn)
            .await?;
        let changed = existing.multiple != input.multiple
            || current_options
                .iter()
                .map(|option| option.title.as_str())
                .ne(input.options.iter().map(|option| option.trim()));
        if changed {
            status_poll_vote::Entity::delete_many()
                .filter(status_poll_vote::Column::PollId.eq(existing.id))
                .exec(txn)
                .await?;
            replace_options(
                txn,
                existing.id,
                input.options.iter().map(|title| (title.as_str(), 0)),
            )
            .await?;
        }
        let id = existing.id;
        let mut active = existing.into_active_model();
        active.multiple = Set(input.multiple);
        active.hide_totals = Set(input.hide_totals);
        active.expires_at = Set(Some(expires_at));
        active.closed_at = Set(None);
        active.notifications_sent_at = Set(None);
        if changed {
            active.voters_count = Set(Some(0));
        }
        active.updated_at = Set(now);
        active.update(txn).await?;
        id
    } else {
        return create_local_poll(txn, status_id, input).await.map(Some);
    };
    enqueue_poll_expiration(txn, poll_id, expires_at).await?;
    find_poll_by_id(txn, poll_id).await
}

/// Insert or refresh a verified remote Question, resetting local votes if its choices changed.
pub async fn upsert_remote_poll(
    txn: &DatabaseTransaction,
    status_id: StatusId,
    input: RemoteStatusPoll,
) -> Result<StatusPoll> {
    if input.options.len() < 2 || input.options.len() > 500 {
        return Err(RoostyError::InvalidInput(
            "remote poll option count is invalid".to_owned(),
        ));
    }
    let now = OffsetDateTime::now_utc();
    let existing = status_poll::Entity::find()
        .filter(status_poll::Column::RemoteStatusId.eq(status_id.0))
        .lock_exclusive()
        .one(txn)
        .await?;
    let poll_id = if let Some(existing) = existing {
        let options = status_poll_option::Entity::find()
            .filter(status_poll_option::Column::PollId.eq(existing.id))
            .order_by_asc(status_poll_option::Column::Position)
            .all(txn)
            .await?;
        let changed = existing.multiple != input.multiple
            || options
                .iter()
                .map(|option| option.title.as_str())
                .ne(input.options.iter().map(|(title, _)| title.as_str()));
        if changed {
            status_poll_vote::Entity::delete_many()
                .filter(status_poll_vote::Column::PollId.eq(existing.id))
                .exec(txn)
                .await?;
        }
        replace_options(
            txn,
            existing.id,
            input
                .options
                .iter()
                .map(|(title, votes)| (title.as_str(), *votes)),
        )
        .await?;
        let id = existing.id;
        let mut active = existing.into_active_model();
        active.multiple = Set(input.multiple);
        active.expires_at = Set(input.expires_at);
        active.closed_at = Set(input.closed_at);
        active.voters_count = Set(input.voters_count.map(|count| count as i64));
        active.updated_at = Set(now);
        active.update(txn).await?;
        id
    } else {
        let id = Uuid::now_v7();
        status_poll::ActiveModel {
            id: Set(id),
            local_status_id: Set(None),
            remote_status_id: Set(Some(status_id.0)),
            multiple: Set(input.multiple),
            hide_totals: Set(false),
            expires_at: Set(input.expires_at),
            closed_at: Set(input.closed_at),
            notifications_sent_at: Set(None),
            voters_count: Set(input.voters_count.map(|count| count as i64)),
            created_at: Set(now),
            updated_at: Set(now),
        }
        .insert(txn)
        .await?;
        replace_options(
            txn,
            id,
            input
                .options
                .iter()
                .map(|(title, votes)| (title.as_str(), *votes)),
        )
        .await?;
        id
    };
    if let Some(expires_at) = input.closed_at.or(input.expires_at) {
        enqueue_poll_expiration(txn, poll_id, expires_at + Duration::minutes(5)).await?;
    }
    find_poll_by_id(txn, poll_id)
        .await?
        .ok_or_else(|| RoostyError::InvalidInput("remote poll was not found".to_owned()))
}

/// Mark a due poll notified and create all local notifications atomically.
pub async fn expire_poll(
    txn: &DatabaseTransaction,
    poll_id: Uuid,
) -> Result<Option<PollExpiration>> {
    let Some(model) = status_poll::Entity::find_by_id(poll_id)
        .lock_exclusive()
        .one(txn)
        .await?
    else {
        return Ok(None);
    };
    let now = OffsetDateTime::now_utc();
    let due_at = model.closed_at.or(model.expires_at);
    if due_at.is_some_and(|due_at| due_at > now) {
        let poll = hydrate_poll(txn, model).await?;
        return Ok(Some(PollExpiration {
            poll,
            notifications: Vec::new(),
            newly_expired: false,
            next_attempt_at: due_at,
        }));
    }
    if model.notifications_sent_at.is_some() {
        let poll = hydrate_poll(txn, model).await?;
        return Ok(Some(PollExpiration {
            poll,
            notifications: Vec::new(),
            newly_expired: false,
            next_attempt_at: None,
        }));
    }
    let mut recipients = status_poll_vote::Entity::find()
        .filter(status_poll_vote::Column::PollId.eq(poll_id))
        .filter(status_poll_vote::Column::LocalAccountId.is_not_null())
        .all(txn)
        .await?
        .into_iter()
        .filter_map(|vote| vote.local_account_id)
        .collect::<Vec<_>>();
    let (actor_account_id, remote_actor_id, status_id, remote_status_id) =
        match (model.local_status_id, model.remote_status_id) {
            (Some(status_id), None) => {
                let status = local_status::Entity::find_by_id(status_id)
                    .one(txn)
                    .await?
                    .ok_or_else(|| {
                        RoostyError::InvalidInput("poll status is missing".to_owned())
                    })?;
                recipients.push(status.account_id);
                (Some(status.account_id), None, Some(status_id), None)
            }
            (None, Some(status_id)) => {
                let status = remote_status::Entity::find_by_id(status_id)
                    .one(txn)
                    .await?
                    .ok_or_else(|| {
                        RoostyError::InvalidInput("poll status is missing".to_owned())
                    })?;
                (None, Some(status.remote_actor_id), None, Some(status_id))
            }
            _ => {
                return Err(RoostyError::InvalidInput(
                    "stored poll status reference is invalid".to_owned(),
                ));
            }
        };
    recipients.sort_unstable();
    recipients.dedup();
    let mut notifications = Vec::with_capacity(recipients.len());
    for account_id in recipients {
        let notification = local_notification::ActiveModel {
            id: Set(Uuid::now_v7()),
            account_id: Set(account_id),
            notification_type: Set(LocalNotificationType::Poll),
            actor_account_id: Set(actor_account_id),
            remote_actor_id: Set(remote_actor_id),
            status_id: Set(status_id),
            remote_status_id: Set(remote_status_id),
            group_id: Set(None),
            filtered: Set(false),
            notification_request_id: Set(None),
            report_id: Set(None),
            created_at: Set(now),
            dismissed_at: Set(None),
        }
        .insert(txn)
        .await?;
        notifications.push(local_notification_from_model(notification));
    }
    let poll_id = model.id;
    let mut active = model.into_active_model();
    active.closed_at = Set(active.closed_at.unwrap().or(due_at).or(Some(now)));
    active.notifications_sent_at = Set(Some(now));
    active.updated_at = Set(now);
    active.update(txn).await?;
    let poll = find_poll_by_id(txn, poll_id)
        .await?
        .ok_or_else(|| RoostyError::InvalidInput("expired poll was not found".to_owned()))?;
    Ok(Some(PollExpiration {
        poll,
        notifications,
        newly_expired: true,
        next_attempt_at: None,
    }))
}

pub async fn local_poll_remote_voter_ids(
    db: &impl ConnectionTrait,
    poll_id: Uuid,
) -> Result<Vec<AccountId>> {
    let mut ids = status_poll_vote::Entity::find()
        .filter(status_poll_vote::Column::PollId.eq(poll_id))
        .filter(status_poll_vote::Column::RemoteActorId.is_not_null())
        .all(db)
        .await?
        .into_iter()
        .filter_map(|vote| vote.remote_actor_id.map(AccountId))
        .collect::<Vec<_>>();
    ids.sort_by_key(|id| id.0);
    ids.dedup();
    Ok(ids)
}

/// Coalesce one locally authored tally update for durable ActivityPub distribution.
pub async fn enqueue_poll_update(
    txn: &DatabaseTransaction,
    poll_id: Uuid,
    run_after: OffsetDateTime,
) -> Result<()> {
    enqueue_job_in_transaction(
        txn,
        NewJob {
            kind: JobKind::PollUpdate,
            payload: serde_json::json!({"poll_id": poll_id}),
            deduplication_key: Some(poll_id.to_string()),
            run_after,
        },
    )
    .await?;
    txn.execute(Statement::from_sql_and_values(
        DatabaseBackend::Postgres,
        "UPDATE job SET run_after = LEAST(run_after, $1) WHERE kind = $2 AND deduplication_key = $3 AND completed_at IS NULL",
        vec![
            run_after.into(),
            JobKind::PollUpdate.as_str().to_owned().into(),
            poll_id.to_string().into(),
        ],
    ))
    .await?;
    Ok(())
}

async fn enqueue_poll_expiration(
    txn: &DatabaseTransaction,
    poll_id: Uuid,
    run_after: OffsetDateTime,
) -> Result<()> {
    enqueue_job_in_transaction(
        txn,
        NewJob {
            kind: JobKind::PollExpiration,
            payload: serde_json::json!({"poll_id": poll_id}),
            // Expiration edits leave the earlier job as a harmless due-time check
            // and create a distinct authoritative job for the new deadline.
            deduplication_key: Some(format!("{}:{}", poll_id, run_after.unix_timestamp_nanos())),
            run_after,
        },
    )
    .await?;
    Ok(())
}

async fn replace_options<'a>(
    txn: &DatabaseTransaction,
    poll_id: Uuid,
    options: impl Iterator<Item = (&'a str, u64)>,
) -> Result<()> {
    status_poll_option::Entity::delete_many()
        .filter(status_poll_option::Column::PollId.eq(poll_id))
        .exec(txn)
        .await?;
    for (position, (title, votes_count)) in options.enumerate() {
        status_poll_option::ActiveModel {
            poll_id: Set(poll_id),
            position: Set(position as i32),
            title: Set(title.trim().to_owned()),
            votes_count: Set(votes_count as i64),
        }
        .insert(txn)
        .await?;
    }
    Ok(())
}

async fn hydrate_poll(db: &impl ConnectionTrait, model: status_poll::Model) -> Result<StatusPoll> {
    let options = status_poll_option::Entity::find()
        .filter(status_poll_option::Column::PollId.eq(model.id))
        .order_by_asc(status_poll_option::Column::Position)
        .all(db)
        .await?
        .into_iter()
        .map(|option| PollOption {
            position: option.position as u32,
            title: option.title,
            votes_count: option.votes_count as u64,
        })
        .collect();
    let status = match (model.local_status_id, model.remote_status_id) {
        (Some(id), None) => PollStatus::Local(StatusId(id)),
        (None, Some(id)) => PollStatus::Remote(StatusId(id)),
        _ => {
            return Err(RoostyError::InvalidInput(
                "stored poll status reference is invalid".to_owned(),
            ));
        }
    };
    Ok(StatusPoll {
        id: model.id,
        status,
        multiple: model.multiple,
        hide_totals: model.hide_totals,
        expires_at: model.expires_at,
        closed_at: model.closed_at,
        voters_count: model.voters_count.map(|count| count as u64),
        options,
        updated_at: model.updated_at,
    })
}

async fn poll_author(db: &impl ConnectionTrait, poll_id: Uuid) -> Result<Option<AccountId>> {
    let Some(poll) = status_poll::Entity::find_by_id(poll_id).one(db).await? else {
        return Ok(None);
    };
    let Some(status_id) = poll.local_status_id else {
        return Ok(None);
    };
    Ok(local_status::Entity::find_by_id(status_id)
        .one(db)
        .await?
        .map(|status| AccountId(status.account_id)))
}

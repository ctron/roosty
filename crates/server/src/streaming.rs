use std::{
    sync::{
        Arc, OnceLock,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Duration,
};

use roosty_core::{Result as RoostyResult, RoostyError};
use roosty_db::{NewStreamingEvent, StatusVisibility, StreamingEventKind, StreamingStatusOrigin};
use serde::Serialize;
use sqlx::postgres::PgListener;
use strum::EnumString;
use tokio::sync::{broadcast, mpsc, watch};
use tracing::{debug, info, warn};
use uuid::Uuid;

use roosty_core::AccountId;

const STREAMING_CHANNEL: &str = "roosty_streaming_events";

/// Process-local metrics for streaming fan-out and socket lifecycle behavior.
#[derive(Debug, Default)]
pub struct StreamingMetrics {
    active_connections: AtomicU64,
    rejected_connections: AtomicU64,
    send_timeouts: AtomicU64,
    idle_disconnects: AtomicU64,
    lagged_receivers: AtomicU64,
    listener_reconnects: AtomicU64,
    publication_failures: AtomicU64,
}

impl StreamingMetrics {
    pub fn connection_opened(&self) {
        self.active_connections.fetch_add(1, Ordering::Relaxed);
    }

    pub fn connection_closed(&self) {
        self.active_connections.fetch_sub(1, Ordering::Relaxed);
    }

    pub fn connection_rejected(&self) {
        self.rejected_connections.fetch_add(1, Ordering::Relaxed);
    }

    pub fn send_timed_out(&self) {
        self.send_timeouts.fetch_add(1, Ordering::Relaxed);
    }

    pub fn idle_disconnected(&self) {
        self.idle_disconnects.fetch_add(1, Ordering::Relaxed);
    }

    pub fn receiver_lagged(&self) {
        self.lagged_receivers.fetch_add(1, Ordering::Relaxed);
    }

    /// Render the streaming metrics in Prometheus's text exposition format.
    pub fn text(&self) -> String {
        format!(
            concat!(
                "# HELP roosty_streaming_active_connections Active streaming WebSocket connections.\n",
                "# TYPE roosty_streaming_active_connections gauge\n",
                "roosty_streaming_active_connections {}\n",
                "# HELP roosty_streaming_rejected_connections_total Connections rejected by the per-process limit.\n",
                "# TYPE roosty_streaming_rejected_connections_total counter\n",
                "roosty_streaming_rejected_connections_total {}\n",
                "# HELP roosty_streaming_send_timeouts_total Streaming socket sends that exceeded their deadline.\n",
                "# TYPE roosty_streaming_send_timeouts_total counter\n",
                "roosty_streaming_send_timeouts_total {}\n",
                "# HELP roosty_streaming_idle_disconnects_total Streaming sockets closed after no inbound frames.\n",
                "# TYPE roosty_streaming_idle_disconnects_total counter\n",
                "roosty_streaming_idle_disconnects_total {}\n",
                "# HELP roosty_streaming_lagged_receivers_total Events skipped by lagged local broadcast receivers.\n",
                "# TYPE roosty_streaming_lagged_receivers_total counter\n",
                "roosty_streaming_lagged_receivers_total {}\n",
                "# HELP roosty_streaming_listener_reconnects_total PostgreSQL listener reconnections.\n",
                "# TYPE roosty_streaming_listener_reconnects_total counter\n",
                "roosty_streaming_listener_reconnects_total {}\n",
                "# HELP roosty_streaming_publication_failures_total Failed cross-process event publications.\n",
                "# TYPE roosty_streaming_publication_failures_total counter\n",
                "roosty_streaming_publication_failures_total {}\n",
            ),
            self.active_connections.load(Ordering::Relaxed),
            self.rejected_connections.load(Ordering::Relaxed),
            self.send_timeouts.load(Ordering::Relaxed),
            self.idle_disconnects.load(Ordering::Relaxed),
            self.lagged_receivers.load(Ordering::Relaxed),
            self.listener_reconnects.load(Ordering::Relaxed),
            self.publication_failures.load(Ordering::Relaxed),
        )
    }
}

struct StreamingInner {
    sender: broadcast::Sender<StreamingEvent>,
    db: roosty_db::DbConnection,
    database_url: String,
    origin_process_id: Uuid,
    event_retention: Duration,
    listener_ready: AtomicBool,
    metrics: Arc<StreamingMetrics>,
    shutdown_tx: watch::Sender<bool>,
    publication_tx: OnceLock<mpsc::Sender<NewStreamingEvent>>,
}

/// Bounded local event bus with PostgreSQL-backed multi-process fan-out.
#[derive(Clone)]
pub struct StreamingEvents {
    inner: Arc<StreamingInner>,
}

impl StreamingEvents {
    /// Create an empty streaming event bus.
    pub fn new(
        db: roosty_db::DbConnection,
        database_url: String,
        event_retention: Duration,
    ) -> Self {
        let (sender, _receiver) = broadcast::channel(1024);
        let (shutdown_tx, _shutdown_rx) = watch::channel(false);
        let metrics = Arc::new(StreamingMetrics::default());
        Self {
            inner: Arc::new(StreamingInner {
                sender,
                db,
                database_url,
                origin_process_id: Uuid::now_v7(),
                event_retention,
                listener_ready: AtomicBool::new(false),
                metrics,
                shutdown_tx,
                publication_tx: OnceLock::new(),
            }),
        }
    }

    /// Establish LISTEN before the process can become ready, then supervise it.
    pub async fn initialize_listener(&self) -> RoostyResult<()> {
        let mut listener = connect_listener(&self.inner.database_url).await?;
        listener
            .listen(STREAMING_CHANNEL)
            .await
            .map_err(sqlx_error)?;
        // LISTEN is active before the cursor snapshot, so commits racing with
        // startup are either included in the cursor or queued as notifications.
        let sequence = roosty_db::latest_streaming_event_sequence(&self.inner.db).await?;
        let (publication_tx, mut publication_rx) = mpsc::channel(1024);
        self.inner.publication_tx.set(publication_tx).map_err(|_| {
            RoostyError::Configuration(
                "PostgreSQL streaming listener is already initialized".to_owned(),
            )
        })?;
        let publication_db = self.inner.db.clone();
        let publication_metrics = self.inner.metrics.clone();
        tokio::spawn(async move {
            while let Some(event) = publication_rx.recv().await {
                if let Err(error) = roosty_db::publish_streaming_event(&publication_db, event).await
                {
                    publication_metrics
                        .publication_failures
                        .fetch_add(1, Ordering::Relaxed);
                    warn!(%error, "failed cross-process streaming publication");
                }
            }
        });
        self.inner.listener_ready.store(true, Ordering::Release);
        tokio::spawn(listener_loop(
            self.clone(),
            listener,
            sequence,
            self.inner.shutdown_tx.subscribe(),
        ));
        tokio::spawn(cleanup_loop(
            self.clone(),
            self.inner.shutdown_tx.subscribe(),
        ));
        info!(%sequence, "PostgreSQL streaming listener initialized");
        Ok(())
    }

    pub fn listener_is_ready(&self) -> bool {
        self.inner.listener_ready.load(Ordering::Acquire)
    }

    pub fn metrics(&self) -> Arc<StreamingMetrics> {
        self.inner.metrics.clone()
    }

    /// Stop background listener and cleanup tasks during graceful shutdown.
    pub fn shutdown(&self) {
        let _ = self.inner.shutdown_tx.send(true);
        self.inner.listener_ready.store(false, Ordering::Release);
    }

    /// Subscribe a WebSocket client to newly published streaming events.
    pub fn subscribe(&self) -> broadcast::Receiver<StreamingEvent> {
        self.inner.sender.subscribe()
    }

    /// Publish a Mastodon `update` event for a newly created local status.
    pub fn publish_status_update<T>(
        &self,
        status: &T,
        author_id: AccountId,
        visibility: StatusVisibility,
        user_recipient_ids: &[AccountId],
    ) where
        T: Serialize,
    {
        match streaming_update_message(
            status,
            author_id,
            visibility,
            user_recipient_ids,
            StreamingStatusOrigin::Local,
        ) {
            Ok(event) => self.publish(event),
            Err(error) => warn!(%error, "failed to serialize streaming update"),
        }
    }

    /// Publish a Mastodon `update` event for a cached remote status.
    pub fn publish_remote_status_update<T>(
        &self,
        status: &T,
        author_id: AccountId,
        visibility: StatusVisibility,
        user_recipient_ids: &[AccountId],
    ) where
        T: Serialize,
    {
        match streaming_update_message(
            status,
            author_id,
            visibility,
            user_recipient_ids,
            StreamingStatusOrigin::Remote,
        ) {
            Ok(event) => self.publish(event),
            Err(error) => warn!(%error, "failed to serialize remote streaming update"),
        }
    }

    /// Publish a Mastodon `status.update` event for an edited status.
    pub fn publish_status_edit<T>(
        &self,
        status: &T,
        author_id: AccountId,
        visibility: StatusVisibility,
        user_recipient_ids: &[AccountId],
        notification_recipient_ids: &[AccountId],
    ) where
        T: Serialize,
    {
        match streaming_status_update_message(
            status,
            author_id,
            visibility,
            user_recipient_ids,
            notification_recipient_ids,
            StreamingStatusOrigin::Local,
        ) {
            Ok(event) => self.publish(event),
            Err(error) => warn!(%error, "failed to serialize edited status"),
        }
    }

    /// Publish a Mastodon `status.update` event for a cached remote status.
    pub fn publish_remote_status_edit<T>(
        &self,
        status: &T,
        author_id: AccountId,
        visibility: StatusVisibility,
        user_recipient_ids: &[AccountId],
        notification_recipient_ids: &[AccountId],
    ) where
        T: Serialize,
    {
        match streaming_status_update_message(
            status,
            author_id,
            visibility,
            user_recipient_ids,
            notification_recipient_ids,
            StreamingStatusOrigin::Remote,
        ) {
            Ok(event) => self.publish(event),
            Err(error) => warn!(%error, "failed to serialize edited remote status"),
        }
    }

    /// Publish a Mastodon `notification` event to the recipient's user stream.
    pub fn publish_notification<T>(&self, notification: &T, recipient_id: AccountId)
    where
        T: Serialize,
    {
        match streaming_notification_message(notification, recipient_id) {
            Ok(event) => self.publish(event),
            Err(error) => warn!(%error, "failed to serialize streaming notification"),
        }
    }

    /// Publish a Mastodon `conversation` event to the recipient's direct stream.
    pub fn publish_conversation<T>(&self, conversation: &T, recipient_id: AccountId)
    where
        T: Serialize,
    {
        match streaming_conversation_message(conversation, recipient_id) {
            Ok(event) => self.publish(event),
            Err(error) => warn!(%error, "failed to serialize streaming conversation"),
        }
    }

    /// Publish a Mastodon `delete` event for a removed status-like entry.
    pub fn publish_delete(
        &self,
        status_id: &str,
        author_id: AccountId,
        visibility: StatusVisibility,
        user_recipient_ids: &[AccountId],
    ) {
        let event = streaming_delete_message(
            status_id,
            author_id,
            visibility,
            user_recipient_ids,
            StreamingStatusOrigin::Local,
            false,
        );
        self.publish(event);
    }

    /// Publish deletion of a public-capable local status with its media metadata.
    pub fn publish_local_status_delete(
        &self,
        status_id: &str,
        author_id: AccountId,
        visibility: StatusVisibility,
        user_recipient_ids: &[AccountId],
        has_media: bool,
    ) {
        self.publish(streaming_delete_message(
            status_id,
            author_id,
            visibility,
            user_recipient_ids,
            StreamingStatusOrigin::Local,
            has_media,
        ));
    }

    /// Publish deletion of a cached remote status to home and public streams.
    pub fn publish_remote_status_delete(
        &self,
        status_id: &str,
        author_id: AccountId,
        visibility: StatusVisibility,
        user_recipient_ids: &[AccountId],
        has_media: bool,
    ) {
        self.publish(streaming_delete_message(
            status_id,
            author_id,
            visibility,
            user_recipient_ids,
            StreamingStatusOrigin::Remote,
            has_media,
        ));
    }

    /// Publish a status update exclusively to selected users' home-capable streams.
    pub fn publish_home_update<T>(&self, status: &T, author_id: AccountId, recipients: &[AccountId])
    where
        T: Serialize,
    {
        self.publish_status_update(status, author_id, StatusVisibility::Unlisted, recipients);
    }

    /// Publish a status deletion exclusively to selected users' home-capable streams.
    pub fn publish_home_delete(
        &self,
        status_id: &str,
        author_id: AccountId,
        recipients: &[AccountId],
    ) {
        self.publish_delete(status_id, author_id, StatusVisibility::Unlisted, recipients);
    }

    fn publish(&self, event: StreamingEvent) {
        if let Err(error) = self.inner.sender.send(event.clone()) {
            debug!(%error, "streaming event had no active receivers");
        }
        let Some(publication_tx) = self.inner.publication_tx.get() else {
            return;
        };

        let persisted = event.to_persisted(self.inner.origin_process_id);
        if let Err(error) = publication_tx.try_send(persisted) {
            self.inner
                .metrics
                .publication_failures
                .fetch_add(1, Ordering::Relaxed);
            warn!(%error, "cross-process streaming publication queue is full or closed");
        }
    }
}

async fn connect_listener(database_url: &str) -> RoostyResult<PgListener> {
    PgListener::connect(database_url).await.map_err(sqlx_error)
}

fn sqlx_error(error: sqlx::Error) -> RoostyError {
    RoostyError::Configuration(format!(
        "could not initialize PostgreSQL streaming listener: {error}"
    ))
}

/// Receive sequence notifications and recover every retained row after the cursor.
async fn listener_loop(
    events: StreamingEvents,
    mut listener: PgListener,
    mut sequence: i64,
    mut shutdown_rx: watch::Receiver<bool>,
) {
    let mut reconnect_delay = Duration::from_secs(1);
    loop {
        let notification = tokio::select! {
            notification = listener.recv() => notification,
            changed = shutdown_rx.changed() => {
                if changed.is_err() || *shutdown_rx.borrow() {
                    return;
                }
                continue;
            }
        };
        match notification {
            Ok(notification) => {
                reconnect_delay = Duration::from_secs(1);
                if notification.payload().parse::<i64>().is_err() {
                    warn!(
                        payload = notification.payload(),
                        "ignored invalid streaming notification"
                    );
                    continue;
                }
                if let Err(error) = deliver_after(&events, &mut sequence).await {
                    warn!(%error, "failed to recover retained streaming events");
                }
            }
            Err(error) => {
                events.inner.listener_ready.store(false, Ordering::Release);
                warn!(%error, "PostgreSQL streaming listener disconnected");
                loop {
                    tokio::select! {
                        () = tokio::time::sleep(reconnect_delay) => {}
                        changed = shutdown_rx.changed() => {
                            if changed.is_err() || *shutdown_rx.borrow() {
                                return;
                            }
                            continue;
                        }
                    }
                    match connect_listener(&events.inner.database_url).await {
                        Ok(mut reconnected) => {
                            if let Err(error) = reconnected.listen(STREAMING_CHANNEL).await {
                                warn!(%error, "failed to restore PostgreSQL streaming LISTEN");
                            } else {
                                listener = reconnected;
                                events
                                    .inner
                                    .metrics
                                    .listener_reconnects
                                    .fetch_add(1, Ordering::Relaxed);
                                if let Err(error) = deliver_after(&events, &mut sequence).await {
                                    warn!(%error, "failed streaming recovery after reconnect");
                                }
                                events.inner.listener_ready.store(true, Ordering::Release);
                                info!(%sequence, "PostgreSQL streaming listener reconnected");
                                break;
                            }
                        }
                        Err(error) => {
                            warn!(%error, "failed to reconnect PostgreSQL streaming listener")
                        }
                    }
                    reconnect_delay = (reconnect_delay * 2).min(Duration::from_secs(30));
                }
            }
        }
    }
}

async fn deliver_after(events: &StreamingEvents, sequence: &mut i64) -> RoostyResult<()> {
    for retained in roosty_db::streaming_events_after(&events.inner.db, *sequence).await? {
        *sequence = retained.sequence;
        if retained.origin_process_id == events.inner.origin_process_id {
            continue;
        }
        let event = StreamingEvent::from_retained(retained);
        if let Err(error) = events.inner.sender.send(event) {
            debug!(%error, "remote streaming event had no active receivers");
        }
    }
    Ok(())
}

async fn cleanup_loop(events: StreamingEvents, mut shutdown_rx: watch::Receiver<bool>) {
    let cleanup_interval = events.inner.event_retention.min(Duration::from_secs(60));
    let mut interval = tokio::time::interval(cleanup_interval);
    loop {
        tokio::select! {
            _ = interval.tick() => {}
            changed = shutdown_rx.changed() => {
                if changed.is_err() || *shutdown_rx.borrow() {
                    return;
                }
                continue;
            }
        }
        let Ok(retention) = time::Duration::try_from(events.inner.event_retention) else {
            warn!("streaming retention exceeds supported database duration");
            return;
        };
        let cutoff = time::OffsetDateTime::now_utc() - retention;
        match roosty_db::delete_streaming_events_before(&events.inner.db, cutoff).await {
            Ok(deleted) if deleted > 0 => debug!(deleted, "expired retained streaming events"),
            Ok(_) => {}
            Err(error) => warn!(%error, "failed to expire retained streaming events"),
        }
    }
}

/// Event payload shared with connected WebSocket subscribers.
#[derive(Clone, Debug)]
pub struct StreamingEvent {
    event: StreamingEventType,
    payload: String,
    account_id: AccountId,
    user_recipient_ids: Vec<AccountId>,
    notification_recipient_ids: Vec<AccountId>,
    visibility: StatusVisibility,
    status_origin: StreamingStatusOrigin,
    has_media: bool,
}

impl StreamingEvent {
    fn to_persisted(&self, origin_process_id: Uuid) -> NewStreamingEvent {
        NewStreamingEvent {
            origin_process_id,
            kind: self.event.into(),
            payload: self.payload.clone(),
            account_id: self.account_id,
            recipient_ids: self.user_recipient_ids.clone(),
            notification_recipient_ids: self.notification_recipient_ids.clone(),
            visibility: self.visibility,
            status_origin: self.status_origin,
            has_media: self.has_media,
        }
    }

    fn from_retained(event: roosty_db::RetainedStreamingEvent) -> Self {
        Self {
            event: event.kind.into(),
            payload: event.payload,
            account_id: event.account_id,
            user_recipient_ids: event.recipient_ids,
            notification_recipient_ids: event.notification_recipient_ids,
            visibility: event.visibility,
            status_origin: event.status_origin,
            has_media: event.has_media,
        }
    }

    /// Serialize this event when it belongs to at least one subscribed stream.
    pub fn to_socket_message(
        &self,
        account_id: AccountId,
        streams: &[String],
    ) -> Result<Option<String>, serde_json::Error> {
        let matching_streams = self.matching_streams(account_id, streams);
        if matching_streams.is_empty() {
            return Ok(None);
        }

        serde_json::to_string(&SocketMessage {
            stream: &matching_streams,
            event: self.event,
            payload: &self.payload,
        })
        .map(Some)
    }

    /// Return the subscribed stream names that should receive this event.
    fn matching_streams(&self, account_id: AccountId, streams: &[String]) -> Vec<String> {
        streams
            .iter()
            .filter(|stream| {
                stream
                    .parse()
                    .is_ok_and(|stream| self.is_visible_to_stream(account_id, stream))
            })
            .cloned()
            .collect()
    }

    /// Return whether one subscribed stream should receive this event.
    fn is_visible_to_stream(&self, account_id: AccountId, stream: StreamingChannel) -> bool {
        match stream {
            StreamingChannel::User => {
                self.event != StreamingEventType::Conversation
                    && (self.account_id == account_id
                        || self.user_recipient_ids.contains(&account_id)
                        || self.notification_recipient_ids.contains(&account_id))
            }
            StreamingChannel::UserNotification => {
                (self.event == StreamingEventType::Notification && self.account_id == account_id)
                    || (self.event == StreamingEventType::StatusUpdate
                        && self.notification_recipient_ids.contains(&account_id))
            }
            StreamingChannel::Direct => {
                self.event == StreamingEventType::Conversation && self.account_id == account_id
            }
            StreamingChannel::Public => self.visibility == StatusVisibility::Public,
            StreamingChannel::PublicMedia => {
                self.visibility == StatusVisibility::Public && self.has_media
            }
            StreamingChannel::PublicLocal => {
                self.visibility == StatusVisibility::Public
                    && self.status_origin == StreamingStatusOrigin::Local
            }
            StreamingChannel::PublicLocalMedia => {
                self.visibility == StatusVisibility::Public
                    && self.status_origin == StreamingStatusOrigin::Local
                    && self.has_media
            }
            StreamingChannel::PublicRemote => {
                self.visibility == StatusVisibility::Public
                    && self.status_origin == StreamingStatusOrigin::Remote
            }
            StreamingChannel::PublicRemoteMedia => {
                self.visibility == StatusVisibility::Public
                    && self.status_origin == StreamingStatusOrigin::Remote
                    && self.has_media
            }
        }
    }
}

/// Streaming channels currently routed by Roosty.
#[derive(Clone, Copy, Debug, EnumString, Eq, PartialEq)]
enum StreamingChannel {
    #[strum(serialize = "user")]
    User,
    #[strum(serialize = "user:notification")]
    UserNotification,
    #[strum(serialize = "direct")]
    Direct,
    #[strum(serialize = "public")]
    Public,
    #[strum(serialize = "public:media")]
    PublicMedia,
    #[strum(serialize = "public:local")]
    PublicLocal,
    #[strum(serialize = "public:local:media")]
    PublicLocalMedia,
    #[strum(serialize = "public:remote")]
    PublicRemote,
    #[strum(serialize = "public:remote:media")]
    PublicRemoteMedia,
}

#[derive(Serialize)]
struct SocketMessage<'a> {
    stream: &'a [String],
    event: StreamingEventType,
    payload: &'a str,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
enum StreamingEventType {
    #[serde(rename = "update")]
    Update,
    #[serde(rename = "status.update")]
    StatusUpdate,
    #[serde(rename = "notification")]
    Notification,
    #[serde(rename = "conversation")]
    Conversation,
    #[serde(rename = "delete")]
    Delete,
}

impl From<StreamingEventType> for StreamingEventKind {
    fn from(value: StreamingEventType) -> Self {
        match value {
            StreamingEventType::Update => Self::Update,
            StreamingEventType::StatusUpdate => Self::StatusUpdate,
            StreamingEventType::Notification => Self::Notification,
            StreamingEventType::Conversation => Self::Conversation,
            StreamingEventType::Delete => Self::Delete,
        }
    }
}

impl From<StreamingEventKind> for StreamingEventType {
    fn from(value: StreamingEventKind) -> Self {
        match value {
            StreamingEventKind::Update => Self::Update,
            StreamingEventKind::StatusUpdate => Self::StatusUpdate,
            StreamingEventKind::Notification => Self::Notification,
            StreamingEventKind::Conversation => Self::Conversation,
            StreamingEventKind::Delete => Self::Delete,
        }
    }
}

/// Build the update event stored in the in-process broadcast channel.
fn streaming_update_message<T>(
    status: &T,
    author_id: AccountId,
    visibility: StatusVisibility,
    user_recipient_ids: &[AccountId],
    status_origin: StreamingStatusOrigin,
) -> Result<StreamingEvent, serde_json::Error>
where
    T: Serialize,
{
    let value = serde_json::to_value(status)?;
    let has_media = serialized_status_has_media(&value);
    let payload = serde_json::to_string(&value)?;
    Ok(StreamingEvent {
        event: StreamingEventType::Update,
        payload,
        account_id: author_id,
        user_recipient_ids: user_recipient_ids.to_owned(),
        notification_recipient_ids: Vec::new(),
        visibility,
        status_origin,
        has_media,
    })
}

/// Build the status-edit event stored in the in-process broadcast channel.
fn streaming_status_update_message<T>(
    status: &T,
    author_id: AccountId,
    visibility: StatusVisibility,
    user_recipient_ids: &[AccountId],
    notification_recipient_ids: &[AccountId],
    status_origin: StreamingStatusOrigin,
) -> Result<StreamingEvent, serde_json::Error>
where
    T: Serialize,
{
    let value = serde_json::to_value(status)?;
    let has_media = serialized_status_has_media(&value);
    let payload = serde_json::to_string(&value)?;
    Ok(StreamingEvent {
        event: StreamingEventType::StatusUpdate,
        payload,
        account_id: author_id,
        user_recipient_ids: user_recipient_ids.to_owned(),
        notification_recipient_ids: notification_recipient_ids.to_owned(),
        visibility,
        status_origin,
        has_media,
    })
}

/// Build the notification event stored in the in-process broadcast channel.
fn streaming_notification_message<T>(
    notification: &T,
    recipient_id: AccountId,
) -> Result<StreamingEvent, serde_json::Error>
where
    T: Serialize,
{
    let payload = serde_json::to_string(notification)?;
    Ok(StreamingEvent {
        event: StreamingEventType::Notification,
        payload,
        account_id: recipient_id,
        user_recipient_ids: Vec::new(),
        notification_recipient_ids: Vec::new(),
        visibility: StatusVisibility::Direct,
        status_origin: StreamingStatusOrigin::Local,
        has_media: false,
    })
}

/// Build the conversation event stored in the in-process broadcast channel.
fn streaming_conversation_message<T>(
    conversation: &T,
    recipient_id: AccountId,
) -> Result<StreamingEvent, serde_json::Error>
where
    T: Serialize,
{
    let payload = serde_json::to_string(conversation)?;
    Ok(StreamingEvent {
        event: StreamingEventType::Conversation,
        payload,
        account_id: recipient_id,
        user_recipient_ids: Vec::new(),
        notification_recipient_ids: Vec::new(),
        visibility: StatusVisibility::Direct,
        status_origin: StreamingStatusOrigin::Local,
        has_media: false,
    })
}

/// Build the delete event stored in the in-process broadcast channel.
fn streaming_delete_message(
    status_id: &str,
    author_id: AccountId,
    visibility: StatusVisibility,
    user_recipient_ids: &[AccountId],
    status_origin: StreamingStatusOrigin,
    has_media: bool,
) -> StreamingEvent {
    StreamingEvent {
        event: StreamingEventType::Delete,
        payload: status_id.to_owned(),
        account_id: author_id,
        user_recipient_ids: user_recipient_ids.to_owned(),
        notification_recipient_ids: Vec::new(),
        visibility,
        status_origin,
        has_media,
    }
}

fn serialized_status_has_media(value: &serde_json::Value) -> bool {
    value
        .get("media_attachments")
        .and_then(serde_json::Value::as_array)
        .is_some_and(|attachments| !attachments.is_empty())
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::Ordering;

    use roosty_core::AccountId;
    use roosty_db::{StatusVisibility, StreamingStatusOrigin};
    use serde_json::Value;
    use uuid::Uuid;

    use super::{
        StreamingMetrics, streaming_conversation_message, streaming_delete_message,
        streaming_notification_message, streaming_status_update_message, streaming_update_message,
    };

    #[test]
    fn streaming_metrics_render_every_operational_counter() {
        let metrics = StreamingMetrics::default();
        metrics.connection_opened();
        metrics.connection_rejected();
        metrics.send_timed_out();
        metrics.idle_disconnected();
        metrics.receiver_lagged();
        metrics.listener_reconnects.fetch_add(1, Ordering::Relaxed);
        metrics.publication_failures.fetch_add(1, Ordering::Relaxed);

        let text = metrics.text();
        for metric in [
            "roosty_streaming_active_connections 1",
            "roosty_streaming_rejected_connections_total 1",
            "roosty_streaming_send_timeouts_total 1",
            "roosty_streaming_idle_disconnects_total 1",
            "roosty_streaming_lagged_receivers_total 1",
            "roosty_streaming_listener_reconnects_total 1",
            "roosty_streaming_publication_failures_total 1",
        ] {
            assert!(text.contains(metric));
        }
    }

    #[test]
    /// Verifies streaming status payloads stay JSON-encoded strings.
    fn update_message_contains_a_string_payload() {
        // Mastodon clients expect the outer event as JSON and the status itself
        // as a JSON-encoded string in the payload field.
        let account_id = AccountId(Uuid::now_v7());
        let event = streaming_update_message(
            &serde_json::json!({"id": "1"}),
            account_id,
            StatusVisibility::Public,
            &[],
            StreamingStatusOrigin::Local,
        )
        .unwrap();
        let message = event
            .to_socket_message(account_id, &["user".to_owned()])
            .unwrap()
            .unwrap();
        let value: Value = serde_json::from_str(&message).unwrap();

        assert_eq!(
            value,
            serde_json::json!({
                "stream": ["user"],
                "event": "update",
                "payload": "{\"id\":\"1\"}"
            })
        );
    }

    #[test]
    /// An edit uses Mastodon's distinct `status.update` event while retaining a string payload.
    fn status_update_message_uses_the_edit_event_name() {
        let account_id = AccountId(Uuid::now_v7());
        let event = streaming_status_update_message(
            &serde_json::json!({"id": "1"}),
            account_id,
            StatusVisibility::Public,
            &[],
            &[],
            StreamingStatusOrigin::Local,
        )
        .unwrap();
        let message = event
            .to_socket_message(account_id, &["user".to_owned()])
            .unwrap()
            .unwrap();
        let value: Value = serde_json::from_str(&message).unwrap();

        assert_eq!(
            value,
            serde_json::json!({
                "stream": ["user"],
                "event": "status.update",
                "payload": "{\"id\":\"1\"}"
            })
        );
    }

    #[test]
    /// Verifies that user streams do not receive another local user's status.
    fn update_messages_are_scoped_to_matching_streams() {
        let author_id = AccountId(Uuid::now_v7());
        let viewer_id = AccountId(Uuid::now_v7());
        let event = streaming_update_message(
            &serde_json::json!({"id": "1"}),
            author_id,
            StatusVisibility::Public,
            &[],
            StreamingStatusOrigin::Local,
        )
        .unwrap();

        let user_message = event
            .to_socket_message(viewer_id, &["user".to_owned()])
            .unwrap();
        let public_message = event
            .to_socket_message(viewer_id, &["public".to_owned()])
            .unwrap();
        let mixed_message = event
            .to_socket_message(author_id, &["user".to_owned(), "public:local".to_owned()])
            .unwrap()
            .unwrap();
        let mixed_value: Value = serde_json::from_str(&mixed_message).unwrap();

        assert!(user_message.is_none());
        assert!(public_message.is_some());
        assert_eq!(
            mixed_value,
            serde_json::json!({
                "stream": ["user", "public:local"],
                "event": "update",
                "payload": "{\"id\":\"1\"}"
            })
        );
    }

    #[test]
    /// Remote media statuses reach federated and remote-media streams, but never local streams.
    fn remote_media_updates_are_scoped_to_public_origin_streams() {
        let author_id = AccountId(Uuid::now_v7());
        let event = streaming_update_message(
            &serde_json::json!({"id": "1", "media_attachments": [{"id": "media"}]}),
            author_id,
            StatusVisibility::Public,
            &[],
            StreamingStatusOrigin::Remote,
        )
        .unwrap();
        let streams = [
            "public".to_owned(),
            "public:media".to_owned(),
            "public:local".to_owned(),
            "public:local:media".to_owned(),
            "public:remote".to_owned(),
            "public:remote:media".to_owned(),
        ];
        let message = event
            .to_socket_message(author_id, &streams)
            .unwrap()
            .unwrap();
        let value: Value = serde_json::from_str(&message).unwrap();

        assert_eq!(
            value["stream"],
            serde_json::json!([
                "public",
                "public:media",
                "public:remote",
                "public:remote:media"
            ])
        );
    }

    #[test]
    /// Given a recipient-only notification, when serialized for streams, then the user's notification streams receive it.
    fn notification_messages_are_scoped_to_the_recipient_user_stream() {
        let recipient_id = AccountId(Uuid::now_v7());
        let viewer_id = AccountId(Uuid::now_v7());
        let event =
            streaming_notification_message(&serde_json::json!({"id": "1"}), recipient_id).unwrap();

        let recipient_message = event
            .to_socket_message(recipient_id, &["user".to_owned()])
            .unwrap()
            .unwrap();
        let notification_stream_message = event
            .to_socket_message(recipient_id, &["user:notification".to_owned()])
            .unwrap()
            .unwrap();
        let viewer_message = event
            .to_socket_message(viewer_id, &["user:notification".to_owned()])
            .unwrap();
        let public_message = event
            .to_socket_message(recipient_id, &["public".to_owned()])
            .unwrap();
        let recipient_value: Value = serde_json::from_str(&recipient_message).unwrap();
        let notification_stream_value: Value =
            serde_json::from_str(&notification_stream_message).unwrap();

        assert_eq!(
            recipient_value,
            serde_json::json!({
                "stream": ["user"],
                "event": "notification",
                "payload": "{\"id\":\"1\"}"
            })
        );
        assert_eq!(
            notification_stream_value,
            serde_json::json!({
                "stream": ["user:notification"],
                "event": "notification",
                "payload": "{\"id\":\"1\"}"
            })
        );
        assert!(viewer_message.is_none());
        assert!(public_message.is_none());
    }

    #[test]
    /// Given a status update, when serialized for notification-only streams, then it is not delivered there.
    fn update_messages_do_not_go_to_notification_only_streams() {
        let account_id = AccountId(Uuid::now_v7());
        let event = streaming_update_message(
            &serde_json::json!({"id": "1"}),
            account_id,
            StatusVisibility::Public,
            &[],
            StreamingStatusOrigin::Local,
        )
        .unwrap();

        let message = event
            .to_socket_message(account_id, &["user:notification".to_owned()])
            .unwrap();

        assert!(message.is_none());
    }

    #[test]
    /// Given an edited mention-only status, both combined user and notification streams receive it.
    fn mentioned_status_edits_reach_notification_streams() {
        let author_id = AccountId(Uuid::now_v7());
        let mentioned_id = AccountId(Uuid::now_v7());
        let event = streaming_status_update_message(
            &serde_json::json!({"id": "1"}),
            author_id,
            StatusVisibility::Public,
            &[],
            &[mentioned_id],
            StreamingStatusOrigin::Local,
        )
        .unwrap();

        let message = event
            .to_socket_message(
                mentioned_id,
                &["user".to_owned(), "user:notification".to_owned()],
            )
            .unwrap()
            .unwrap();
        let value: Value = serde_json::from_str(&message).unwrap();

        assert_eq!(value["event"], "status.update");
        assert_eq!(
            value["stream"],
            serde_json::json!(["user", "user:notification"])
        );
    }

    #[test]
    /// Given a conversation event, when serialized for streams, then only the recipient's direct stream receives it.
    fn conversation_messages_are_scoped_to_the_recipient_direct_stream() {
        let recipient_id = AccountId(Uuid::now_v7());
        let viewer_id = AccountId(Uuid::now_v7());
        let event =
            streaming_conversation_message(&serde_json::json!({"id": "1"}), recipient_id).unwrap();

        let direct_message = event
            .to_socket_message(recipient_id, &["direct".to_owned()])
            .unwrap()
            .unwrap();
        let user_message = event
            .to_socket_message(recipient_id, &["user".to_owned()])
            .unwrap();
        let viewer_message = event
            .to_socket_message(viewer_id, &["direct".to_owned()])
            .unwrap();
        let value: Value = serde_json::from_str(&direct_message).unwrap();

        assert_eq!(
            value,
            serde_json::json!({
                "stream": ["direct"],
                "event": "conversation",
                "payload": "{\"id\":\"1\"}"
            })
        );
        assert!(user_message.is_none());
        assert!(viewer_message.is_none());
    }

    #[test]
    /// Given an update with home recipients, when serialized for a recipient user stream, then it is delivered.
    fn update_messages_are_delivered_to_home_recipients() {
        let author_id = AccountId(Uuid::now_v7());
        let follower_id = AccountId(Uuid::now_v7());
        let event = streaming_update_message(
            &serde_json::json!({"id": "1"}),
            author_id,
            StatusVisibility::Public,
            &[follower_id],
            StreamingStatusOrigin::Local,
        )
        .unwrap();

        let message = event
            .to_socket_message(follower_id, &["user".to_owned()])
            .unwrap()
            .unwrap();
        let value: Value = serde_json::from_str(&message).unwrap();

        assert_eq!(
            value,
            serde_json::json!({
                "stream": ["user"],
                "event": "update",
                "payload": "{\"id\":\"1\"}"
            })
        );
    }

    #[test]
    /// Given a deleted boost id, when serialized for home recipients, then the payload is the plain id string.
    fn delete_messages_use_plain_identifier_payloads() {
        let author_id = AccountId(Uuid::now_v7());
        let follower_id = AccountId(Uuid::now_v7());
        let event = streaming_delete_message(
            "boost-id",
            author_id,
            StatusVisibility::Direct,
            &[follower_id],
            StreamingStatusOrigin::Local,
            false,
        );

        let message = event
            .to_socket_message(follower_id, &["user".to_owned()])
            .unwrap()
            .unwrap();
        let value: Value = serde_json::from_str(&message).unwrap();

        assert_eq!(
            value,
            serde_json::json!({
                "stream": ["user"],
                "event": "delete",
                "payload": "boost-id"
            })
        );

        let direct_message = event
            .to_socket_message(follower_id, &["direct".to_owned()])
            .unwrap();
        assert!(direct_message.is_none());
    }
}

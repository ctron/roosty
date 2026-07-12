use serde::Serialize;
use strum::IntoStaticStr;
use tokio::sync::broadcast;
use tracing::{debug, warn};

use roost_core::AccountId;

/// In-process event bus for Mastodon streaming API compatibility.
#[derive(Clone)]
pub struct StreamingEvents {
    sender: broadcast::Sender<StreamingEvent>,
}

impl StreamingEvents {
    /// Create an empty streaming event bus.
    pub fn new() -> Self {
        let (sender, _receiver) = broadcast::channel(1024);
        Self { sender }
    }

    /// Subscribe a WebSocket client to newly published streaming events.
    pub fn subscribe(&self) -> broadcast::Receiver<StreamingEvent> {
        self.sender.subscribe()
    }

    /// Publish a Mastodon `update` event for a newly created local status.
    pub fn publish_status_update<T>(&self, status: &T, author_id: AccountId, visibility: &str)
    where
        T: Serialize,
    {
        match streaming_update_message(status, author_id, visibility) {
            Ok(event) => match self.sender.send(event) {
                Ok(_) => {}
                Err(error) => debug!(%error, "streaming update had no active receivers"),
            },
            Err(error) => warn!(%error, "failed to serialize streaming update"),
        }
    }

    /// Publish a Mastodon `notification` event to the recipient's user stream.
    pub fn publish_notification<T>(&self, notification: &T, recipient_id: AccountId)
    where
        T: Serialize,
    {
        match streaming_notification_message(notification, recipient_id) {
            Ok(event) => match self.sender.send(event) {
                Ok(_) => {}
                Err(error) => debug!(%error, "streaming notification had no active receivers"),
            },
            Err(error) => warn!(%error, "failed to serialize streaming notification"),
        }
    }
}

/// Event payload shared with connected WebSocket subscribers.
#[derive(Clone, Debug)]
pub struct StreamingEvent {
    event: StreamingEventType,
    payload: String,
    account_id: AccountId,
    visibility: String,
}

impl StreamingEvent {
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
            event: self.event.as_str(),
            payload: &self.payload,
        })
        .map(Some)
    }

    /// Return the subscribed stream names that should receive this event.
    fn matching_streams(&self, account_id: AccountId, streams: &[String]) -> Vec<String> {
        streams
            .iter()
            .filter(|stream| self.is_visible_to_stream(account_id, stream))
            .cloned()
            .collect()
    }

    /// Return whether one subscribed stream should receive this event.
    fn is_visible_to_stream(&self, account_id: AccountId, stream: &str) -> bool {
        match stream {
            "user" => self.account_id == account_id,
            "user:notification" => {
                self.event == StreamingEventType::Notification && self.account_id == account_id
            }
            "public" | "public:local" => self.visibility == "public",
            _ => false,
        }
    }
}

#[derive(Serialize)]
struct SocketMessage<'a> {
    stream: &'a [String],
    event: &'static str,
    payload: &'a str,
}

#[derive(Clone, Copy, Debug, Eq, IntoStaticStr, PartialEq)]
enum StreamingEventType {
    #[strum(serialize = "update")]
    Update,
    #[strum(serialize = "notification")]
    Notification,
}

impl StreamingEventType {
    /// Return the Mastodon streaming event name.
    fn as_str(self) -> &'static str {
        self.into()
    }
}

/// Build the update event stored in the in-process broadcast channel.
fn streaming_update_message<T>(
    status: &T,
    author_id: AccountId,
    visibility: &str,
) -> Result<StreamingEvent, serde_json::Error>
where
    T: Serialize,
{
    let payload = serde_json::to_string(status)?;
    Ok(StreamingEvent {
        event: StreamingEventType::Update,
        payload,
        account_id: author_id,
        visibility: visibility.to_owned(),
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
        visibility: "direct".to_owned(),
    })
}

#[cfg(test)]
mod tests {
    use roost_core::AccountId;
    use serde_json::Value;
    use uuid::Uuid;

    use super::{streaming_notification_message, streaming_update_message};

    #[test]
    /// Verifies streaming status payloads stay JSON-encoded strings.
    fn update_message_contains_a_string_payload() {
        // Mastodon clients expect the outer event as JSON and the status itself
        // as a JSON-encoded string in the payload field.
        let account_id = AccountId(Uuid::now_v7());
        let event = streaming_update_message(&serde_json::json!({"id": "1"}), account_id, "public")
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
    /// Verifies that user streams do not receive another local user's status.
    fn update_messages_are_scoped_to_matching_streams() {
        let author_id = AccountId(Uuid::now_v7());
        let viewer_id = AccountId(Uuid::now_v7());
        let event =
            streaming_update_message(&serde_json::json!({"id": "1"}), author_id, "public").unwrap();

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
        let event = streaming_update_message(&serde_json::json!({"id": "1"}), account_id, "public")
            .unwrap();

        let message = event
            .to_socket_message(account_id, &["user:notification".to_owned()])
            .unwrap();

        assert!(message.is_none());
    }
}

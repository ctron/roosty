use serde::Serialize;
use tokio::sync::broadcast;
use tracing::{debug, warn};

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

    /// Publish a Mastodon `update` event containing a serialized status payload.
    pub fn publish_update<T>(&self, status: &T)
    where
        T: Serialize,
    {
        match streaming_update_message(status) {
            Ok(event) => match self.sender.send(event) {
                Ok(_) => {}
                Err(error) => debug!(%error, "streaming update had no active receivers"),
            },
            Err(error) => warn!(%error, "failed to serialize streaming update"),
        }
    }
}

/// Event payload shared with connected WebSocket subscribers.
#[derive(Clone, Debug)]
pub struct StreamingEvent {
    event: &'static str,
    payload: String,
}

impl StreamingEvent {
    /// Serialize this event for one client's subscribed stream names.
    pub fn to_socket_message(&self, streams: &[String]) -> Result<String, serde_json::Error> {
        serde_json::to_string(&SocketMessage {
            stream: streams,
            event: self.event,
            payload: &self.payload,
        })
    }
}

#[derive(Serialize)]
struct SocketMessage<'a> {
    stream: &'a [String],
    event: &'static str,
    payload: &'a str,
}

/// Build the update event stored in the in-process broadcast channel.
fn streaming_update_message<T>(status: &T) -> Result<StreamingEvent, serde_json::Error>
where
    T: Serialize,
{
    let payload = serde_json::to_string(status)?;
    Ok(StreamingEvent {
        event: "update",
        payload,
    })
}

#[cfg(test)]
mod tests {
    use serde_json::Value;

    use super::streaming_update_message;

    #[test]
    /// Verifies streaming status payloads stay JSON-encoded strings.
    fn update_message_contains_a_string_payload() {
        // Mastodon clients expect the outer event as JSON and the status itself
        // as a JSON-encoded string in the payload field.
        let event = streaming_update_message(&serde_json::json!({"id": "1"})).unwrap();
        let message = event.to_socket_message(&["user".to_owned()]).unwrap();
        let value: Value = serde_json::from_str(&message).unwrap();

        assert_eq!(value["stream"], serde_json::json!(["user"]));
        assert_eq!(value["event"], "update");
        assert_eq!(value["payload"], "{\"id\":\"1\"}");
    }
}

use std::sync::OnceLock;
use iced::Subscription;
use tokio::sync::broadcast;

use super::messages::Message;

static EVENT_TX: OnceLock<broadcast::Sender<crate::types::AppEvent>> = OnceLock::new();

/// Set the event sender for subscriptions. Call once during app boot.
pub fn set_event_sender(tx: broadcast::Sender<crate::types::AppEvent>) {
    EVENT_TX.set(tx).unwrap();
}

/// Create a subscription that listens for AppEvent broadcasts
/// and converts them to Message::AppEvent.
pub fn event_subscription() -> Subscription<Message> {
    Subscription::run(event_stream_fn)
}

fn event_stream_fn() -> impl futures::Stream<Item = Message> + 'static {
    let tx = EVENT_TX.get().expect("event sender not set").clone();
    async_stream::stream! {
        loop {
            let mut rx = tx.subscribe();
            match rx.recv().await {
                Ok(event) => yield Message::AppEvent(event),
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    tracing::debug!("Dropped {} app events", n);
                    while let Ok(_event) = rx.recv().await {
                        // drain
                    }
                }
                Err(broadcast::error::RecvError::Closed) => {
                    continue;
                }
            }
        }
    }
}

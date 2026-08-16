use crate::types::AppEvent;

/// Application message type for the iced update loop.
#[derive(Debug, Clone)]
pub enum Message {
    /// Dummy event for spike testing
    FakeEvent,
    /// Scroll event from the chat list
    Scrolled(f32),
    /// Received an event from the background channel
    AppEvent(AppEvent),
    /// User typed into the input field
    InputChanged(String),
    /// User pressed send
    Send,
    /// User pressed escape (stop generation)
    Stop,
    /// Toggle theme
    ThemeToggled,
    /// Settings button clicked
    SettingsClicked,
    /// Presets button clicked
    PresetsClicked,
    /// Agent config button clicked
    AgentConfigClicked,
    /// Save settings
    SettingsSaved,
    /// Close settings dialog
    SettingsClosed,
    /// Close presets dialog
    PresetsClosed,
    /// Close agent config dialog
    AgentConfigClosed,
    /// Session selected
    SessionSelected(String),
    /// New session created
    SessionCreated,
    /// Session renamed
    SessionRenamed(String, String),
    /// Session deleted
    SessionDeleted(String),
    /// Scroll to bottom button clicked
    JumpToBottom,
    /// Toggle follow-bottom mode
    ToggleFollowBottom,
    /// Pipeline cancelled
    PipelineCancelled,
    /// Error toast message
    ErrorToast(String),
    /// Dismiss error toast
    DismissErrorToast,
    /// Message edit mode entered
    MessageEditEntered(usize),
    /// Message edit content changed
    MessageEditChanged(String),
    /// Message edit committed
    MessageEditCommitted(usize),
    /// Message edit cancelled
    MessageEditCancelled,
    /// Message deleted
    MessageDeleted(usize),
    /// Image attachment selected
    ImageAttached(Option<String>),
}

impl From<AppEvent> for Message {
    fn from(event: AppEvent) -> Self {
        Message::AppEvent(event)
    }
}

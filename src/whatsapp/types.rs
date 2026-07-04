//! Inbound webhook payload types. Deliberately permissive — Meta adds
//! fields freely and also delivers status-only events on the same topic.

use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct WebhookPayload {
    #[serde(default)]
    pub entry: Vec<Entry>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Entry {
    #[serde(default)]
    pub changes: Vec<Change>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Change {
    pub value: ChangeValue,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ChangeValue {
    /// Present for inbound messages; absent for delivery/read status events.
    #[serde(default)]
    pub messages: Vec<Message>,
    #[serde(default)]
    pub contacts: Vec<Contact>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Contact {
    pub wa_id: String,
    #[serde(default)]
    pub profile: Option<Profile>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Profile {
    #[serde(default)]
    pub name: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Message {
    /// Sender phone in wa_id form, e.g. "9198xxxxxxx".
    pub from: String,
    /// Meta message id — our idempotency key.
    pub id: String,
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default)]
    pub text: Option<TextBody>,
    #[serde(default)]
    pub image: Option<Media>,
    #[serde(default)]
    pub audio: Option<Media>,
    #[serde(default)]
    pub context: Option<MessageContext>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TextBody {
    pub body: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Media {
    pub id: String,
    #[serde(default)]
    pub caption: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct MessageContext {
    #[serde(default)]
    pub forwarded: bool,
    #[serde(default)]
    pub frequently_forwarded: bool,
}

impl Message {
    pub fn was_forwarded(&self) -> bool {
        self.context
            .as_ref()
            .map(|c| c.forwarded || c.frequently_forwarded)
            .unwrap_or(false)
    }
}

impl ChangeValue {
    /// The sender's WhatsApp profile name, when Meta includes it.
    pub fn profile_name(&self, wa_id: &str) -> Option<String> {
        self.contacts
            .iter()
            .find(|c| c.wa_id == wa_id)
            .and_then(|c| c.profile.as_ref())
            .and_then(|p| p.name.clone())
            .filter(|n| !n.trim().is_empty())
    }
}

//! Desktop relay notices carried inside the existing unaddressed Error event.
//! This adds no MessagePack variant or protocol-version requirement. Older
//! clients show the notice as text; newer clients retain per-host pane state.

use serde::{Deserialize, Serialize};

const PREFIX: &str = "nebula relay: ";

/// Availability of one local relay transport, independent of terminal output.
#[derive(Debug, Serialize, Deserialize)]
pub struct RelayStatus {
    /// SSH host alias whose relay changed state.
    pub host: String,
    /// A host snapshot has arrived on the current connection.
    pub connected: bool,
}

impl RelayStatus {
    /// Encode a notice using the protocol's existing text error envelope.
    pub fn message(&self) -> String {
        format!(
            "{PREFIX}{}",
            serde_json::to_string(self).expect("relay status serializes")
        )
    }

    /// Recognize only relay notices; ordinary errors retain their usual meaning.
    pub fn parse(message: &str) -> Option<Self> {
        serde_json::from_str(message.strip_prefix(PREFIX)?).ok()
    }
}

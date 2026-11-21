//! Domain models for connection profiles.

use serde::{Deserialize, Serialize};

/// Remote Desktop Connection Profile shared across UI, Client, and Server crates.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConnectionProfile {
    pub id: String,
    pub friendly_name: String,
    pub group_name: String,
    pub host_address: String,
    #[serde(default)]
    pub vpn_binding: Option<String>,
    #[serde(default = "default_true")]
    pub auto_reconnect: bool,
    #[serde(default)]
    pub admin_session_takeover: bool,
    #[serde(default = "default_true")]
    pub ask_credentials_on_connect: bool,
    #[serde(default)]
    pub folder_shares: Vec<String>,
    #[serde(default = "default_true")]
    pub enable_speaker_audio: bool,
    #[serde(default)]
    pub enable_mic_passthrough: bool,
}

fn default_true() -> bool {
    true
}

impl Default for ConnectionProfile {
    fn default() -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            friendly_name: String::from("Remote Workstation"),
            group_name: String::from("Group 1"),
            host_address: String::from("127.0.0.1:4433"),
            vpn_binding: None,
            auto_reconnect: true,
            admin_session_takeover: false,
            ask_credentials_on_connect: true,
            folder_shares: Vec::new(),
            enable_speaker_audio: true,
            enable_mic_passthrough: false,
        }
    }
}

impl ConnectionProfile {
    pub fn name(&self) -> &str {
        &self.friendly_name
    }

    pub fn group(&self) -> &str {
        &self.group_name
    }
}

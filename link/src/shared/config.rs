//! Configuration data types for dynamic channel configuration.
//!
//! These types match the JSON schema sent by the config WebSocket feed.

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use serde::{Deserialize, Serialize};

/// A PTT/chat track identified by namespace segments and track name.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TrackRef {
    pub namespace: Vec<String>,
    pub name: String,
}

/// Language-specific tracks for a channel.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ChannelLanguageTracks {
    pub ptt: TrackRef,
    pub chat: TrackRef,
}

/// A channel configuration.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ChannelConfig {
    pub id: String,
    pub display_name: String,
    pub users: Vec<String>,
    pub languages: alloc::collections::BTreeMap<String, ChannelLanguageTracks>,
}

/// Language-specific tracks for the AI service.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AiLanguageTracks {
    pub namespace: Vec<String>,
    pub name: String,
}

/// AI service configuration.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AiConfig {
    pub name: String,
    pub languages: alloc::collections::BTreeMap<String, AiLanguageTracks>,
    pub response_ns: Vec<String>,
}

/// A WiFi network in the config feed.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ConfigWifiNetwork {
    pub ssid: String,
    #[serde(default)]
    pub password: Option<String>,
}

/// Top-level device configuration from the config feed.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DeviceConfig {
    pub relay_url: String,
    pub trial_name: String,
    pub trial_id: String,
    pub user_id: String,
    pub user_directory: String,
    pub ai: AiConfig,
    pub channels: Vec<ChannelConfig>,
    pub wifi_networks: Vec<ConfigWifiNetwork>,
}

/// Supported languages.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Language {
    En,
    De,
    Es,
    Hi,
    No,
}

impl Language {
    /// Parse from a language code string.
    pub fn from_str_code(s: &str) -> Option<Self> {
        match s {
            "en" => Some(Language::En),
            "de" => Some(Language::De),
            "es" => Some(Language::Es),
            "hi" => Some(Language::Hi),
            "no" => Some(Language::No),
            _ => None,
        }
    }

    /// Return the language code string.
    pub fn as_str(&self) -> &'static str {
        match self {
            Language::En => "en",
            Language::De => "de",
            Language::Es => "es",
            Language::Hi => "hi",
            Language::No => "no",
        }
    }

    /// All valid language code strings.
    pub const VALID_CODES: &[&str] = &["en", "de", "es", "hi", "no"];
}

impl Default for Language {
    fn default() -> Self {
        Language::En
    }
}

impl core::fmt::Display for Language {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Credentials for connecting to the config WebSocket feed.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ConfigCredentials {
    pub config_url: String,
    pub access_token: String,
    pub refresh_token: String,
}

//! WiFi credential types shared between ctl and net.
//!
//! This module is only compiled when the `ctl` or `net` feature is enabled.

use heapless::String;
use serde::{Deserialize, Serialize};

/// Maximum length for SSID (32 bytes per WiFi spec).
const MAX_SSID_LEN: usize = 32;

/// Maximum length for WiFi password (63 bytes per WPA2 spec).
const MAX_PASSWORD_LEN: usize = 63;

/// Maximum number of WiFi credentials to store.
#[cfg(feature = "net")]
pub const MAX_WIFI_SSIDS: usize = 8;

/// Maximum length for relay URL.
#[cfg(feature = "net")]
pub const MAX_RELAY_URL_LEN: usize = 128;

/// A WiFi SSID and password pair.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub struct WifiSsid {
    pub ssid: String<MAX_SSID_LEN>,
    pub password: String<MAX_PASSWORD_LEN>,
}

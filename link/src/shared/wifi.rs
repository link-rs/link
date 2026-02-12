//! WiFi credential types shared between ctl and net.
//!
//! This module is only compiled when the `ctl` or `net` feature is enabled.

#[cfg(feature = "alloc")]
extern crate alloc;
#[cfg(feature = "alloc")]
use alloc::string::String;

use serde::{Deserialize, Serialize};

/// Maximum length for SSID (32 bytes per WiFi spec).
#[cfg(not(feature = "alloc"))]
const MAX_SSID_LEN: usize = 32;

/// Maximum length for WiFi password (63 bytes per WPA2 spec).
#[cfg(not(feature = "alloc"))]
const MAX_PASSWORD_LEN: usize = 63;

/// Maximum number of WiFi credentials to store.
#[cfg(feature = "net")]
pub const MAX_WIFI_SSIDS: usize = 8;

/// Maximum length for relay URL.
#[cfg(feature = "net")]
pub const MAX_RELAY_URL_LEN: usize = 128;

/// A WiFi SSID and password pair (heap-allocated when alloc is available).
#[cfg(feature = "alloc")]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
// Note: Cannot derive defmt::Format with std::string::String (std types don't support defmt)
pub struct WifiSsid {
    pub ssid: String,
    pub password: String,
}

/// A WiFi SSID and password pair (stack-allocated for no_std).
#[cfg(not(feature = "alloc"))]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub struct WifiSsid {
    pub ssid: heapless::String<MAX_SSID_LEN>,
    pub password: heapless::String<MAX_PASSWORD_LEN>,
}

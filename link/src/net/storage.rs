//! Persistent storage for data on the NET chip.

use embedded_storage::{ReadStorage, Storage};
use heapless::{String, Vec};
use serde::{Deserialize, Serialize};

// Re-export WiFi types from shared
pub use crate::shared::wifi::{MAX_RELAY_URL_LEN, MAX_WIFI_SSIDS, WifiSsid};

// Re-export MoQ types from shared
pub use crate::shared::moq::{
    MAX_MOQ_NAMESPACE_LEN, MAX_MOQ_TRACK_NAME_LEN, MoqError, MoqExampleType,
};

/// Max length for language string (e.g., "en-US")
pub const MAX_LANGUAGE_LEN: usize = 16;
/// Max length for channel config JSON
pub const MAX_CHANNEL_LEN: usize = 256;
/// Max length for AI config JSON
pub const MAX_AI_LEN: usize = 512;

/// Persistent storage data for the NET chip.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
// Note: Only derive defmt::Format when not using alloc (WifiSsid contains String when alloc is enabled)
#[cfg_attr(all(feature = "defmt", not(feature = "alloc")), derive(defmt::Format))]
pub struct NetStorageData {
    pub wifi_ssids: Vec<WifiSsid, MAX_WIFI_SSIDS>,
    pub relay_url: String<MAX_RELAY_URL_LEN>,
    pub language: String<MAX_LANGUAGE_LEN>,
    pub channel: String<MAX_CHANNEL_LEN>,
    pub ai: String<MAX_AI_LEN>,
    pub logs_enabled: bool,
}

/// Flash storage interface for the NET chip.
pub struct NetStorage<F> {
    flash: F,
    offset: u32,
    data: NetStorageData,
}

/// Magic bytes to identify valid storage.
const MAGIC: [u8; 4] = *b"LNKS";

/// Storage format version.
/// V4 adds language, channel, ai, logs_enabled.
const VERSION: u8 = 4;

/// Header size: 4 bytes magic + 1 byte version + 2 bytes length.
const HEADER_SIZE: usize = 7;

impl<F> NetStorage<F>
where
    F: ReadStorage + Storage,
{
    /// Create a new storage interface and load existing data.
    pub fn new(flash: F, offset: u32) -> Self {
        let mut storage = Self {
            flash,
            offset,
            data: NetStorageData::default(),
        };
        storage.load();
        storage
    }

    /// Load storage from flash.
    fn load(&mut self) {
        let mut header = [0u8; HEADER_SIZE];
        if self.flash.read(self.offset, &mut header).is_err() {
            return;
        }

        // Check magic
        if header[0..4] != MAGIC {
            return;
        }

        // Check version
        if header[4] != VERSION {
            return;
        }

        // Get data length
        let len = u16::from_le_bytes([header[5], header[6]]) as usize;
        if len > 512 {
            return;
        }

        // Read data
        let mut buf = [0u8; 512];
        if self
            .flash
            .read(self.offset + HEADER_SIZE as u32, &mut buf[..len])
            .is_err()
        {
            return;
        }

        // Deserialize
        if let Ok((data, _)) = serde_json_core::from_slice(&buf[..len]) {
            self.data = data;
        }
    }

    /// Save storage to flash.
    pub fn save(&mut self) -> Result<(), F::Error> {
        // Serialize data
        let mut buf = [0u8; 512];
        let len =
            serde_json_core::to_slice(&self.data, &mut buf).expect("JSON serialization failed");

        // Prepare header + data in one buffer to write atomically
        let mut write_buf = [0u8; HEADER_SIZE + 512];
        write_buf[0..4].copy_from_slice(&MAGIC);
        write_buf[4] = VERSION;
        write_buf[5..7].copy_from_slice(&(len as u16).to_le_bytes());
        write_buf[HEADER_SIZE..HEADER_SIZE + len].copy_from_slice(&buf[..len]);

        // Write to flash
        self.flash
            .write(self.offset, &write_buf[..HEADER_SIZE + len])?;

        Ok(())
    }

    /// Add a WiFi SSID and password pair.
    pub fn add_wifi_ssid(&mut self, ssid: &str, password: &str) -> Result<(), ()> {
        if self.data.wifi_ssids.len() >= MAX_WIFI_SSIDS {
            return Err(());
        }

        let wifi = WifiSsid {
            ssid: ssid.try_into().map_err(|_| ())?,
            password: password.try_into().map_err(|_| ())?,
        };

        self.data.wifi_ssids.push(wifi).map_err(|_| ())?;
        Ok(())
    }

    /// Get the list of WiFi SSIDs.
    pub fn get_wifi_ssids(&self) -> &Vec<WifiSsid, MAX_WIFI_SSIDS> {
        &self.data.wifi_ssids
    }

    /// Clear all WiFi SSIDs.
    pub fn clear_wifi_ssids(&mut self) {
        self.data.wifi_ssids.clear();
    }

    /// Get the relay URL.
    pub fn get_relay_url(&self) -> &str {
        &self.data.relay_url
    }

    /// Set the relay URL.
    pub fn set_relay_url(&mut self, url: &str) -> Result<(), ()> {
        self.data.relay_url = String::try_from(url).map_err(|_| ())?;
        Ok(())
    }

    /// Get the language setting.
    pub fn get_language(&self) -> &str {
        &self.data.language
    }

    /// Set the language setting.
    pub fn set_language(&mut self, lang: &str) -> Result<(), ()> {
        self.data.language = String::try_from(lang).map_err(|_| ())?;
        Ok(())
    }

    /// Get the channel configuration.
    pub fn get_channel(&self) -> &str {
        &self.data.channel
    }

    /// Set the channel configuration.
    pub fn set_channel(&mut self, channel: &str) -> Result<(), ()> {
        self.data.channel = String::try_from(channel).map_err(|_| ())?;
        Ok(())
    }

    /// Get the AI configuration.
    pub fn get_ai(&self) -> &str {
        &self.data.ai
    }

    /// Set the AI configuration.
    pub fn set_ai(&mut self, config: &str) -> Result<(), ()> {
        self.data.ai = String::try_from(config).map_err(|_| ())?;
        Ok(())
    }

    /// Get logs enabled state.
    pub fn get_logs_enabled(&self) -> bool {
        self.data.logs_enabled
    }

    /// Set logs enabled state.
    pub fn set_logs_enabled(&mut self, enabled: bool) {
        self.data.logs_enabled = enabled;
    }

    /// Clear all stored configuration (reset to factory defaults).
    pub fn clear(&mut self) {
        self.data = NetStorageData::default();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Mock flash storage for testing.
    struct MockFlash {
        data: [u8; 4096],
    }

    impl MockFlash {
        fn new() -> Self {
            Self { data: [0xff; 4096] }
        }
    }

    impl embedded_storage::ReadStorage for MockFlash {
        type Error = core::convert::Infallible;

        fn read(&mut self, offset: u32, bytes: &mut [u8]) -> Result<(), Self::Error> {
            let start = offset as usize;
            let end = start + bytes.len();
            bytes.copy_from_slice(&self.data[start..end]);
            Ok(())
        }

        fn capacity(&self) -> usize {
            self.data.len()
        }
    }

    impl embedded_storage::Storage for MockFlash {
        fn write(&mut self, offset: u32, bytes: &[u8]) -> Result<(), Self::Error> {
            let start = offset as usize;
            self.data[start..start + bytes.len()].copy_from_slice(bytes);
            Ok(())
        }
    }

    #[test]
    fn default_storage_empty() {
        let flash = MockFlash::new();
        let storage = NetStorage::new(flash, 0);
        assert!(storage.get_wifi_ssids().is_empty());
        assert_eq!(storage.get_relay_url(), "");
    }

    #[test]
    fn add_and_get_wifi_ssid() {
        let flash = MockFlash::new();
        let mut storage = NetStorage::new(flash, 0);

        storage.add_wifi_ssid("MyNetwork", "MyPassword").unwrap();

        let ssids = storage.get_wifi_ssids();
        assert_eq!(ssids.len(), 1);
        assert_eq!(ssids[0].ssid.as_str(), "MyNetwork");
        assert_eq!(ssids[0].password.as_str(), "MyPassword");
    }

    #[test]
    fn add_multiple_wifi_ssids() {
        let flash = MockFlash::new();
        let mut storage = NetStorage::new(flash, 0);

        storage.add_wifi_ssid("Network1", "Pass1").unwrap();
        storage.add_wifi_ssid("Network2", "Pass2").unwrap();
        storage.add_wifi_ssid("Network3", "Pass3").unwrap();

        let ssids = storage.get_wifi_ssids();
        assert_eq!(ssids.len(), 3);
    }

    #[test]
    fn clear_wifi_ssids() {
        let flash = MockFlash::new();
        let mut storage = NetStorage::new(flash, 0);

        storage.add_wifi_ssid("Network1", "Pass1").unwrap();
        storage.add_wifi_ssid("Network2", "Pass2").unwrap();
        storage.clear_wifi_ssids();

        assert!(storage.get_wifi_ssids().is_empty());
    }

    #[test]
    fn set_and_get_relay_url() {
        let flash = MockFlash::new();
        let mut storage = NetStorage::new(flash, 0);

        storage
            .set_relay_url("https://relay.example.com/stream")
            .unwrap();
        assert_eq!(storage.get_relay_url(), "https://relay.example.com/stream");
    }

    #[test]
    fn save_and_load() {
        let mut flash = MockFlash::new();

        // Create and populate storage
        {
            let mut storage = NetStorage::new(MockFlash::new(), 0);
            storage.add_wifi_ssid("TestSSID", "TestPass").unwrap();
            storage.set_relay_url("https://test.relay").unwrap();
            storage.save().unwrap();

            // Copy flash data
            flash.data = storage.flash.data;
        }

        // Load into new instance
        let storage = NetStorage::new(flash, 0);
        assert_eq!(storage.get_wifi_ssids().len(), 1);
        assert_eq!(storage.get_wifi_ssids()[0].ssid.as_str(), "TestSSID");
        assert_eq!(storage.get_wifi_ssids()[0].password.as_str(), "TestPass");
        assert_eq!(storage.get_relay_url(), "https://test.relay");
    }

    #[test]
    fn max_wifi_ssids() {
        let flash = MockFlash::new();
        let mut storage = NetStorage::new(flash, 0);

        for i in 0..MAX_WIFI_SSIDS {
            storage
                .add_wifi_ssid(&format!("Net{}", i), &format!("Pass{}", i))
                .unwrap();
        }

        // Should fail when full
        assert!(storage.add_wifi_ssid("Overflow", "Pass").is_err());
    }
}

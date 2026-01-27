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

// Re-export channel configuration types from shared
pub use crate::shared::channel::{ChannelConfig, MAX_CHANNELS, MAX_CHANNEL_URL_LEN};

/// Persistent storage data for the NET chip.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub struct NetStorageData {
    pub wifi_ssids: Vec<WifiSsid, MAX_WIFI_SSIDS>,
    pub relay_url: String<MAX_RELAY_URL_LEN>,
    /// Channel-specific configurations (added in V2).
    pub channels: Vec<ChannelConfig, MAX_CHANNELS>,
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
/// V2 adds channel configurations.
const VERSION: u8 = 2;

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
        if let Ok(data) = postcard::from_bytes(&buf[..len]) {
            self.data = data;
        }
    }

    /// Save storage to flash.
    pub fn save(&mut self) -> Result<(), F::Error> {
        // Serialize data
        let mut buf = [0u8; 512];
        let serialized = postcard::to_slice(&self.data, &mut buf).unwrap();
        let len = serialized.len();

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

    /// Get configuration for a specific channel.
    pub fn get_channel_config(&self, channel_id: u8) -> Option<&ChannelConfig> {
        self.data
            .channels
            .iter()
            .find(|c| c.channel_id == channel_id)
    }

    /// Set configuration for a channel.
    /// Replaces existing config for that channel_id or adds new one.
    pub fn set_channel_config(&mut self, config: ChannelConfig) -> Result<(), ()> {
        // Find and replace existing, or add new
        if let Some(existing) = self
            .data
            .channels
            .iter_mut()
            .find(|c| c.channel_id == config.channel_id)
        {
            *existing = config;
        } else {
            self.data.channels.push(config).map_err(|_| ())?;
        }
        Ok(())
    }

    /// Get all channel configurations.
    pub fn get_all_channel_configs(&self) -> &Vec<ChannelConfig, MAX_CHANNELS> {
        &self.data.channels
    }

    /// Clear all channel configurations.
    pub fn clear_channel_configs(&mut self) {
        self.data.channels.clear();
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

    #[test]
    fn set_and_get_channel_config() {
        let flash = MockFlash::new();
        let mut storage = NetStorage::new(flash, 0);

        let config = ChannelConfig {
            channel_id: 0,
            enabled: true,
            relay_url: heapless::String::try_from("wss://ptt.relay.com").unwrap(),
        };

        storage.set_channel_config(config.clone()).unwrap();

        let retrieved = storage.get_channel_config(0).unwrap();
        assert_eq!(retrieved.channel_id, 0);
        assert!(retrieved.enabled);
        assert_eq!(retrieved.relay_url.as_str(), "wss://ptt.relay.com");
    }

    #[test]
    fn update_channel_config() {
        let flash = MockFlash::new();
        let mut storage = NetStorage::new(flash, 0);

        // Add initial config
        let config1 = ChannelConfig {
            channel_id: 1,
            enabled: true,
            relay_url: heapless::String::try_from("wss://first.relay").unwrap(),
        };
        storage.set_channel_config(config1).unwrap();

        // Update same channel
        let config2 = ChannelConfig {
            channel_id: 1,
            enabled: false,
            relay_url: heapless::String::try_from("wss://second.relay").unwrap(),
        };
        storage.set_channel_config(config2).unwrap();

        // Should only have one entry
        assert_eq!(storage.get_all_channel_configs().len(), 1);

        let retrieved = storage.get_channel_config(1).unwrap();
        assert!(!retrieved.enabled);
        assert_eq!(retrieved.relay_url.as_str(), "wss://second.relay");
    }

    #[test]
    fn clear_channel_configs() {
        let flash = MockFlash::new();
        let mut storage = NetStorage::new(flash, 0);

        storage
            .set_channel_config(ChannelConfig {
                channel_id: 0,
                enabled: true,
                relay_url: heapless::String::new(),
            })
            .unwrap();
        storage
            .set_channel_config(ChannelConfig {
                channel_id: 1,
                enabled: true,
                relay_url: heapless::String::new(),
            })
            .unwrap();

        assert_eq!(storage.get_all_channel_configs().len(), 2);

        storage.clear_channel_configs();
        assert!(storage.get_all_channel_configs().is_empty());
    }

    #[test]
    fn channel_config_not_found() {
        let flash = MockFlash::new();
        let storage = NetStorage::new(flash, 0);

        assert!(storage.get_channel_config(99).is_none());
    }
}

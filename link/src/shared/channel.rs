//! Channel configuration types shared between ctl and net.

use heapless::String;
use serde::{Deserialize, Serialize};

/// Maximum length for relay endpoint URL per channel.
pub const MAX_CHANNEL_URL_LEN: usize = 128;

/// Maximum number of channels that can be configured.
#[cfg(feature = "net")]
pub const MAX_CHANNELS: usize = 4;

/// Channel configuration for a single audio channel.
///
/// Each channel can optionally have its own relay URL.
/// If `relay_url` is empty, the global relay URL is used.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub struct ChannelConfig {
    /// Channel ID (Ptt=0, PttAi=1, ChatAi=3).
    pub channel_id: u8,
    /// Whether this channel is enabled.
    pub enabled: bool,
    /// Relay endpoint for this channel (WebSocket URL).
    /// If empty, uses the global relay_url.
    pub relay_url: String<MAX_CHANNEL_URL_LEN>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_channel_config_default() {
        let config = ChannelConfig::default();
        assert_eq!(config.channel_id, 0);
        assert!(!config.enabled);
        assert!(config.relay_url.is_empty());
    }

    #[test]
    fn test_channel_config_serde() {
        let config = ChannelConfig {
            channel_id: 1,
            enabled: true,
            relay_url: String::try_from("wss://relay.example.com").unwrap(),
        };

        let mut buf = [0u8; 256];
        let serialized = postcard::to_slice(&config, &mut buf).unwrap();
        let deserialized: ChannelConfig = postcard::from_bytes(serialized).unwrap();

        assert_eq!(config, deserialized);
    }
}

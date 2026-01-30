//! Shared types and utilities used across all chips.

pub mod chunk;

// Jitter buffer - for net firmware with audio-buffer, esp-idf, or tests
#[cfg(any(all(feature = "net", feature = "audio-buffer"), feature = "esp-idf", test))]
pub mod jitter_buffer;

pub mod led;

// Logging macro - provides info! that is a no-op when defmt is disabled
// Needed by firmware modules (mgmt, net, ui)
#[cfg(any(feature = "mgmt", feature = "net", feature = "ui"))]
mod logging;

#[cfg(test)]
pub mod mocks;
pub mod protocol;
pub mod tlv;
pub mod uart_config;

// MoQ types - only used by net
#[cfg(feature = "net")]
pub mod moq;

// WiFi types - only used by ctl, net, and async-ctl
#[cfg(any(feature = "ctl", feature = "net", feature = "async-ctl"))]
pub mod wifi;

// Channel configuration types - used by ctl and net
#[cfg(any(feature = "ctl", feature = "net"))]
pub mod channel;

// Re-export the info macro (no-op when defmt is disabled)
#[cfg(any(feature = "mgmt", feature = "net", feature = "ui"))]
pub(crate) use logging::info;

// Jitter buffer types - for net firmware with audio-buffer, esp-idf, or tests
#[cfg(any(all(feature = "net", feature = "audio-buffer"), feature = "esp-idf", test))]
#[allow(unused_imports)] // Re-exported for public API
pub use jitter_buffer::{BUFFER_FRAMES, JitterBuffer, JitterState, JitterStats, MIN_START_LEVEL};

// LED types - used by all
pub use led::{Color, InvertedPin, Led};

// Protocol types - used by all
pub use protocol::*;

// TLV types - core types used by all
pub use tlv::{MAX_VALUE_SIZE, Tlv};
// Sync TLV constants - used by ctl and esp-idf (bare-metal firmware uses async traits)
#[cfg(any(feature = "ctl", feature = "esp-idf"))]
pub use tlv::{HEADER_SIZE, SYNC_WORD};
// Async TLV traits and types - for firmware modules
#[cfg(any(feature = "mgmt", feature = "net", feature = "ui"))]
#[allow(unused_imports)] // Re-exported for public API
pub use tlv::{ReadTlv, Value, WriteTlv};

// MoQ types - used by net
#[cfg(feature = "net")]
#[allow(unused_imports)] // Re-exported for public API
pub use moq::{MAX_MOQ_NAMESPACE_LEN, MAX_MOQ_TRACK_NAME_LEN, MoqError, MoqExampleType};

// WiFi types - WifiSsid used by ctl, net, and async-ctl
#[cfg(any(feature = "ctl", feature = "net", feature = "async-ctl"))]
#[allow(unused_imports)] // Re-exported for public API
pub use wifi::WifiSsid;

// Channel configuration types - used by ctl and net
#[cfg(any(feature = "ctl", feature = "net"))]
#[allow(unused_imports)] // Re-exported for public API
pub use channel::{ChannelConfig, MAX_CHANNELS, MAX_CHANNEL_URL_LEN};

// Re-export embassy_sync types for use by firmware chip modules that need them
#[cfg(any(feature = "net", feature = "ui"))]
#[allow(unused_imports)] // Re-exported for public API
pub use embassy_sync::{
    blocking_mutex::raw::{CriticalSectionRawMutex, RawMutex},
    channel::{Channel, Receiver, Sender},
};

/// Read TLV messages from a reader and send them to a channel.
/// Only available for net and ui firmware.
#[cfg(any(feature = "net", feature = "ui"))]
pub async fn read_tlv_loop<'a, T, R, RM, E, F, const N: usize>(
    mut reader: R,
    sender: Sender<'a, RM, E, N>,
    wrap: F,
) -> !
where
    T: TryFrom<u16>,
    R: ReadTlv<T>,
    RM: RawMutex,
    F: Fn(Tlv<T>) -> E,
{
    loop {
        if let Ok(Some(tlv)) = reader.read_tlv().await {
            sender.send(wrap(tlv)).await;
        }
        // On error or None, continue looping
    }
}

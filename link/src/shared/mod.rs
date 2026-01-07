//! Shared types and utilities used across all chips.

pub mod jitter_buffer;
pub mod led;
mod logging;
#[cfg(test)]
pub mod mocks;
pub mod protocol;
pub mod tlv;
pub mod uart_config;
pub mod wifi;

pub(crate) use logging::info;

pub use jitter_buffer::{BUFFER_FRAMES, JitterBuffer, JitterState, JitterStats, MIN_START_LEVEL};
pub use led::{Color, InvertedPin, Led};
pub use protocol::*;
pub use tlv::{HEADER_SIZE, MAX_VALUE_SIZE, ReadTlv, SYNC_WORD, Tlv, Value, WriteTlv};
pub use wifi::{MAX_PASSWORD_LEN, MAX_RELAY_URL_LEN, MAX_SSID_LEN, MAX_WIFI_SSIDS, WifiSsid};

// Re-export embassy_sync types for use by chip modules
pub use embassy_sync::{
    blocking_mutex::raw::{CriticalSectionRawMutex, RawMutex},
    channel::{Channel, Receiver, Sender},
    mutex::Mutex,
};

/// Read TLV messages from a reader and send them to a channel.
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

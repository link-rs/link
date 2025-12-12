//! Shared types and utilities used across all chips.

pub mod led;
pub mod protocol;
pub mod tlv;

pub use led::{Color, InvertedPin, Led};
pub use protocol::*;
pub use tlv::{ReadTlv, Tlv, Value, WriteTlv, MAX_VALUE_SIZE, SYNC_WORD};

// Re-export embassy_sync types for use by chip modules
pub use embassy_sync::{
    blocking_mutex::raw::{CriticalSectionRawMutex, RawMutex},
    channel::{Channel, Sender},
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

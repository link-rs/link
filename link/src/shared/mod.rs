//! Shared types and utilities used across all chips.

pub mod led;
pub mod protocol;
pub mod tlv;

pub use led::{Color, InvertedPin, Led};
pub use protocol::*;
pub use tlv::{ReadTlv, Tlv, Value, WriteTlv, MAX_VALUE_SIZE, SYNC_WORD};

// Re-export embassy_sync types for use by chip modules
pub use embassy_sync::channel::{Channel, Sender};

/// Raw mutex type used for embassy channels
pub type RawMutex = embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;

use embedded_io_async::Read;

/// Read TLV messages from a reader and send them to a channel.
pub async fn read_tlv_loop<'a, T, R, E, F, const N: usize>(
    mut reader: R,
    sender: Sender<'a, RawMutex, E, N>,
    wrap: F,
) -> !
where
    T: TryFrom<u16>,
    R: ReadTlv<T>,
    F: Fn(Tlv<T>) -> E,
{
    loop {
        if let Ok(Some(tlv)) = reader.read_tlv().await {
            sender.send(wrap(tlv)).await;
        }
        // On error or None, continue looping
    }
}

/// Read raw bytes from a reader and send them to a channel.
pub async fn read_raw_loop<'a, R, E, F, const N: usize>(
    mut reader: R,
    sender: Sender<'a, RawMutex, E, N>,
    wrap: F,
) -> !
where
    R: Read,
    F: Fn(Value) -> E,
{
    let mut buffer = [0u8; MAX_VALUE_SIZE];
    loop {
        let Ok(n) = reader.read(&mut buffer).await else {
            // On error, continue looping
            continue;
        };

        let value = buffer[..n].try_into().unwrap();
        sender.send(wrap(value)).await;
    }
}

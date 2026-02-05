//! Flash target module.
//!
//! This module defines the traits and types used for flashing operations on a
//! target device.
//!
//! This module include an `FlashTarget` trait impl for `Esp32Target` and
//! `RamTarget`, enabling the writing of firmware images to the target device's
//! flash memory or static memory (SRAM). It also provides a `ProgressCallbacks`
//! trait which allows for progress updates during the flashing process.`

mod esp32;
mod ram;
pub use self::{esp32::Esp32Target, ram::RamTarget};
use crate::{Error, connection::{Connection, SerialInterface}, image_format::Segment};

/// Enum-based flash target to avoid trait object issues with async.
#[derive(Debug)]
#[cfg(feature = "serialport")]
pub enum FlashTargetType {
    /// ESP32 flash target for writing to flash memory.
    Esp32(Esp32Target),
    /// RAM target for writing to static memory.
    Ram(RamTarget),
}

#[cfg(feature = "serialport")]
impl FlashTargetType {
    /// Begin the flashing operation.
    pub async fn begin<P: SerialInterface>(&mut self, connection: &mut Connection<P>) -> Result<(), Error> {
        match self {
            FlashTargetType::Esp32(t) => t.begin(connection).await,
            FlashTargetType::Ram(t) => t.begin(connection).await,
        }
    }

    /// Write a segment to the target device.
    pub async fn write_segment<P: SerialInterface>(
        &mut self,
        connection: &mut Connection<P>,
        segment: Segment<'_>,
        progress: &mut dyn ProgressCallbacks,
    ) -> Result<(), Error> {
        match self {
            FlashTargetType::Esp32(t) => t.write_segment(connection, segment, progress).await,
            FlashTargetType::Ram(t) => t.write_segment(connection, segment, progress).await,
        }
    }

    /// Complete the flashing operation.
    pub async fn finish<P: SerialInterface>(&mut self, connection: &mut Connection<P>, reboot: bool) -> Result<(), Error> {
        match self {
            FlashTargetType::Esp32(t) => t.finish(connection, reboot).await,
            FlashTargetType::Ram(t) => t.finish(connection, reboot).await,
        }
    }
}

/// Progress update callbacks.
pub trait ProgressCallbacks {
    /// Initialize some progress report.
    fn init(&mut self, addr: u32, total: usize);
    /// Update some progress report.
    fn update(&mut self, current: usize);
    /// Indicate post-flash checksum verification has begun.
    fn verifying(&mut self);
    /// Finish some progress report.
    fn finish(&mut self, skipped: bool);
}

/// An empty implementation of [ProgressCallbacks] that does nothing.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct DefaultProgressCallback;

impl ProgressCallbacks for DefaultProgressCallback {
    fn init(&mut self, _addr: u32, _total: usize) {}
    fn update(&mut self, _current: usize) {}
    fn verifying(&mut self) {}
    fn finish(&mut self, _skipped: bool) {}
}

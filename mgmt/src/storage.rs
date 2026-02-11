//! Flash-based persistent storage for MGMT configuration.

use embassy_stm32::flash::{Blocking, Flash};
use link::mgmt::BaudRateStorage;

/// Flash storage offset (last 2 KB page of STM32F072CB, page 63 at offset 126 KB)
const STORAGE_OFFSET: u32 = 0x1F800;

/// Magic bytes to identify valid storage: "LINK"
const MAGIC: [u8; 4] = [0x4C, 0x49, 0x4E, 0x4B];

/// Storage format version
const VERSION: u8 = 0x01;

/// Flash-based baud rate storage implementation
pub struct FlashBaudRateStorage {
    flash: Flash<'static, Blocking>,
}

impl FlashBaudRateStorage {
    pub fn new(flash: Flash<'static, Blocking>) -> Self {
        Self { flash }
    }

    /// Validate that a baud rate is in the acceptable range
    fn is_valid_baud_rate(baud_rate: u32) -> bool {
        matches!(
            baud_rate,
            9600 | 19200 | 38400 | 57600 | 115200 | 230400 | 460800 | 921600
        )
    }
}

impl BaudRateStorage for FlashBaudRateStorage {
    fn get(&mut self) -> Option<u32> {
        let mut buf = [0u8; 12];

        // Read the storage area
        if self.flash.blocking_read(STORAGE_OFFSET, &mut buf).is_err() {
            return None;
        }

        // Check magic
        if buf[0..4] != MAGIC {
            return None;
        }

        // Check version
        if buf[4] != VERSION {
            return None;
        }

        // Read baud rate (big-endian u32 at offset 8)
        let baud_rate = u32::from_be_bytes([buf[8], buf[9], buf[10], buf[11]]);

        // Validate baud rate
        if Self::is_valid_baud_rate(baud_rate) {
            Some(baud_rate)
        } else {
            None
        }
    }

    fn set(&mut self, baud_rate: u32) -> bool {
        // Validate baud rate
        if !Self::is_valid_baud_rate(baud_rate) {
            return false;
        }

        // Prepare data to write
        let mut buf = [0u8; 12];
        buf[0..4].copy_from_slice(&MAGIC);
        buf[4] = VERSION;
        // Reserved bytes at 5..8 are already 0
        buf[8..12].copy_from_slice(&baud_rate.to_be_bytes());

        // Erase the page (required before writing)
        if self.flash.blocking_erase(STORAGE_OFFSET, STORAGE_OFFSET + 2048).is_err() {
            return false;
        }

        // Write the data
        if self.flash.blocking_write(STORAGE_OFFSET, &buf).is_err() {
            return false;
        }

        true
    }
}

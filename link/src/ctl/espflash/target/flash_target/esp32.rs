//! ESP32 flash target module.
//!
//! This module defines the traits and types used for flashing operations on a
//! target device's flash memory.

use log::debug;
use md5::{Digest, Md5};
use miniz_oxide::deflate::compress_to_vec_zlib;

use super::super::super::{
    Error,
    flasher::{FLASH_SECTOR_SIZE, SpiAttachParams},
    image_format::Segment,
    target::{Chip, WDT_WKEY},
};

use super::super::super::{
    command::{Command, CommandType},
    connection::{Connection, SerialInterface},
};
use super::ProgressCallbacks;

/// Applications running from an ESP32's (or variant's) flash
#[derive(Copy, Clone, Debug, Hash, PartialEq, Eq)]
pub struct Esp32Target {
    chip: Chip,
    spi_attach_params: SpiAttachParams,
    use_stub: bool,
    verify: bool,
    skip: bool,
    need_deflate_end: bool,
}

impl Esp32Target {
    /// Create a new ESP32 target.
    pub fn new(
        chip: Chip,
        spi_attach_params: SpiAttachParams,
        use_stub: bool,
        verify: bool,
        skip: bool,
    ) -> Self {
        Esp32Target {
            chip,
            spi_attach_params,
            use_stub,
            verify,
            skip,
            need_deflate_end: false,
        }
    }

    pub async fn begin<P: SerialInterface>(&mut self, connection: &mut Connection<P>) -> Result<(), Error> {
        let old_timeout = connection.serial.timeout();
        connection.serial.set_timeout(CommandType::SpiAttach.timeout())?;
        let command = if self.use_stub {
            Command::SpiAttachStub {
                spi_params: self.spi_attach_params,
            }
        } else {
            Command::SpiAttach {
                spi_params: self.spi_attach_params,
            }
        };
        let result = connection.command(command).await;
        connection.serial.set_timeout(old_timeout)?;
        result?;

        // The stub usually disables these watchdog timers, however if we're not using
        // the stub we need to disable them before flashing begins.
        //
        // TODO: the stub doesn't appear to disable the watchdog on ESP32-S3, so we
        //       explicitly disable the watchdog here.
        if connection.is_using_usb_serial_jtag() {
            if let (Some(wdt_wprotect), Some(wdt_config0)) =
                (self.chip.wdt_wprotect(), self.chip.wdt_config0())
            {
                connection.command(Command::WriteReg {
                    address: wdt_wprotect,
                    value: WDT_WKEY,
                    mask: None,
                }).await?; // WP disable
                connection.command(Command::WriteReg {
                    address: wdt_config0,
                    value: 0x0,
                    mask: None,
                }).await?; // turn off RTC WDT
                connection.command(Command::WriteReg {
                    address: wdt_wprotect,
                    value: 0x0,
                    mask: None,
                }).await?; // WP enable
            }
        }

        Ok(())
    }

    pub async fn write_segment<P: SerialInterface>(
        &mut self,
        connection: &mut Connection<P>,
        segment: Segment<'_>,
        progress: &mut dyn ProgressCallbacks,
    ) -> Result<(), Error> {
        let addr = segment.addr;

        let mut md5_hasher = Md5::new();
        md5_hasher.update(&segment.data);
        let checksum_md5 = md5_hasher.finalize();

        // Compress with zlib format at maximum compression level (10)
        let compressed = compress_to_vec_zlib(&segment.data, 10);

        let flash_write_size = self.chip.flash_write_size();
        let block_count = compressed.len().div_ceil(flash_write_size);
        let erase_count = segment.data.len().div_ceil(FLASH_SECTOR_SIZE);

        // round up to sector size
        let erase_size = (erase_count * FLASH_SECTOR_SIZE) as u32;

        let chunks = compressed.chunks(flash_write_size);
        let num_chunks = chunks.len();

        progress.init(addr, num_chunks);

        if self.skip {
            let old_timeout = connection.serial.timeout();
            connection.serial.set_timeout(CommandType::FlashMd5.timeout_for_size(segment.data.len() as u32))?;
            let result = connection
                .command(Command::FlashMd5 {
                    offset: addr,
                    size: segment.data.len() as u32,
                }).await;
            connection.serial.set_timeout(old_timeout)?;
            let flash_checksum_md5: u128 = result?.try_into()?;

            if checksum_md5[..] == flash_checksum_md5.to_be_bytes() {
                debug!("Segment at address '0x{addr:x}' has not changed, skipping write");

                progress.finish(true);
                return Ok(());
            }
        }

        let old_timeout = connection.serial.timeout();
        connection.serial.set_timeout(CommandType::FlashDeflBegin.timeout_for_size(erase_size))?;
        let result = connection.command(Command::FlashDeflBegin {
            size: segment.data.len() as u32,
            blocks: block_count as u32,
            block_size: flash_write_size as u32,
            offset: addr,
            supports_encryption: self.chip != Chip::Esp32 && !self.use_stub,
        }).await;
        connection.serial.set_timeout(old_timeout)?;
        result?;
        self.need_deflate_end = true;

        // Estimate decompressed size per chunk for timeout calculation.
        // We know the total uncompressed size, so we estimate per-chunk as average.
        let total_uncompressed = segment.data.len();
        let avg_decoded_per_chunk = total_uncompressed.div_ceil(num_chunks);

        for (i, block) in chunks.enumerate() {
            // Use average decompressed size for timeout (last chunk may be smaller)
            let estimated_size = if i == num_chunks - 1 {
                total_uncompressed - (avg_decoded_per_chunk * i)
            } else {
                avg_decoded_per_chunk
            };

            let old_timeout = connection.serial.timeout();
            connection.serial.set_timeout(CommandType::FlashDeflData.timeout_for_size(estimated_size as u32))?;
            let result = connection.command(Command::FlashDeflData {
                sequence: i as u32,
                pad_to: 0,
                pad_byte: 0xff,
                data: block,
            }).await;
            connection.serial.set_timeout(old_timeout)?;
            result?;

            progress.update(i + 1)
        }

        if self.verify {
            progress.verifying();
            let old_timeout = connection.serial.timeout();
            connection.serial.set_timeout(CommandType::FlashMd5.timeout_for_size(segment.data.len() as u32))?;
            let result = connection
                .command(Command::FlashMd5 {
                    offset: addr,
                    size: segment.data.len() as u32,
                }).await;
            connection.serial.set_timeout(old_timeout)?;
            let flash_checksum_md5: u128 = result?.try_into()?;

            if checksum_md5[..] != flash_checksum_md5.to_be_bytes() {
                return Err(Error::VerifyFailed);
            }
            debug!("Segment at address '0x{addr:x}' verified successfully");
        }

        progress.finish(false);

        Ok(())
    }

    pub async fn finish<P: SerialInterface>(&mut self, connection: &mut Connection<P>, reboot: bool) -> Result<(), Error> {
        if self.need_deflate_end {
            let old_timeout = connection.serial.timeout();
            connection.serial.set_timeout(CommandType::FlashDeflEnd.timeout())?;
            let result = connection.command(Command::FlashDeflEnd { reboot: false }).await;
            connection.serial.set_timeout(old_timeout)?;
            result?;
        }

        if reboot {
            connection.reset_after(self.use_stub, self.chip).await?;
        }

        Ok(())
    }
}

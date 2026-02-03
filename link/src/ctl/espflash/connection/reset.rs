//! Reset strategies for resetting a target device.
//!
//! This module defines the traits and types used for resetting a target device.

// Most of this module is copied from `esptool.py`:
// https://github.com/espressif/esptool/blob/a8586d0/esptool/reset.py

use log::debug;
use serde::{Deserialize, Serialize};
use strum::{Display, EnumIter, EnumString, VariantNames};

use super::{Connection, SerialInterface, USB_SERIAL_JTAG_PID};
use super::super::{
    Error,
    command::{Command, CommandType},
    flasher::FLASH_WRITE_SIZE,
};

/// Default time to wait before releasing the boot pin after a reset.
const DEFAULT_RESET_DELAY: u64 = 50; // ms
/// Amount of time to wait if the default reset delay does not work.
const EXTRA_RESET_DELAY: u64 = 500; // ms

async fn set_dtr<P: SerialInterface>(serial_port: &mut P, level: bool) -> Result<(), Error> {
    serial_port.write_data_terminal_ready(level).await?;
    Ok(())
}

async fn set_rts<P: SerialInterface>(serial_port: &mut P, level: bool) -> Result<(), Error> {
    serial_port.write_request_to_send(level).await?;
    Ok(())
}

/// Reset strategy types for resetting a target device.
#[derive(Debug, Clone, Copy, Serialize, Hash, Deserialize)]
pub enum ResetStrategy {
    /// Classic reset sequence, sets DTR and RTS sequentially.
    Classic { delay_ms: u64 },
    /// UNIX-only reset sequence with custom implementation.
    #[cfg(unix)]
    UnixTight { delay_ms: u64 },
    /// Custom reset sequence for USB-JTAG-Serial peripheral.
    UsbJtagSerial,
}

impl ResetStrategy {
    /// Create a classic reset strategy.
    pub fn classic(extra_delay: bool) -> Self {
        let delay_ms = if extra_delay {
            EXTRA_RESET_DELAY
        } else {
            DEFAULT_RESET_DELAY
        };
        ResetStrategy::Classic { delay_ms }
    }

    /// Create a UNIX tight reset strategy.
    #[cfg(unix)]
    pub fn unix_tight(extra_delay: bool) -> Self {
        let delay_ms = if extra_delay {
            EXTRA_RESET_DELAY
        } else {
            DEFAULT_RESET_DELAY
        };
        ResetStrategy::UnixTight { delay_ms }
    }

    /// Create a USB JTAG serial reset strategy.
    pub fn usb_jtag_serial() -> Self {
        ResetStrategy::UsbJtagSerial
    }

    /// Execute the reset strategy.
    pub async fn reset<P: SerialInterface>(&self, serial_port: &mut P) -> Result<(), Error> {
        match self {
            ResetStrategy::Classic { delay_ms } => {
                debug!("Using Classic reset strategy with delay of {}ms", delay_ms);
                set_rts(serial_port, false).await?;
                set_dtr(serial_port, false).await?;

                set_rts(serial_port, true).await?;
                set_dtr(serial_port, true).await?;

                set_rts(serial_port, true).await?; // EN = LOW, chip in reset
                set_dtr(serial_port, false).await?; // IO0 = HIGH

                serial_port.delay_ms(100).await;

                set_rts(serial_port, false).await?; // EN = HIGH, chip out of reset
                set_dtr(serial_port, true).await?; // IO0 = LOW

                serial_port.delay_ms(*delay_ms as u32).await;

                set_rts(serial_port, false).await?;
                set_dtr(serial_port, false).await?; // IO0 = HIGH, done

                Ok(())
            }
            #[cfg(unix)]
            ResetStrategy::UnixTight { delay_ms } => {
                debug!("Using UnixTight reset strategy with delay of {}ms", delay_ms);

                set_dtr(serial_port, false).await?;
                set_rts(serial_port, false).await?;
                set_dtr(serial_port, true).await?;
                set_rts(serial_port, true).await?;
                set_dtr(serial_port, false).await?; // IO = HIGH
                set_rts(serial_port, true).await?; // EN = LOW, chip in reset

                serial_port.delay_ms(100).await;

                set_dtr(serial_port, true).await?; // IO0 = LOW
                set_rts(serial_port, false).await?; // EN = HIGH, chip out of reset

                serial_port.delay_ms(*delay_ms as u32).await;

                set_dtr(serial_port, false).await?; // IO0 = HIGH, done
                set_rts(serial_port, false).await?;

                Ok(())
            }
            ResetStrategy::UsbJtagSerial => {
                debug!("Using UsbJtagSerial reset strategy");

                set_rts(serial_port, false).await?;
                set_dtr(serial_port, false).await?; // Idle

                serial_port.delay_ms(100).await;

                set_rts(serial_port, false).await?;
                set_dtr(serial_port, true).await?; // Set IO0

                serial_port.delay_ms(100).await;

                set_rts(serial_port, true).await?; // Reset. Calls inverted to go through (1,1) instead of (0,0)
                set_dtr(serial_port, false).await?;
                set_rts(serial_port, true).await?; // RTS set as Windows only propagates DTR on RTS setting

                serial_port.delay_ms(100).await;

                set_rts(serial_port, false).await?;
                set_dtr(serial_port, false).await?;

                Ok(())
            }
        }
    }
}

/// Resets the target device.
pub async fn reset_after_flash<P: SerialInterface>(serial: &mut P, pid: u16) -> Result<(), Error> {
    serial.delay_ms(100).await;

    if pid == USB_SERIAL_JTAG_PID {
        serial.write_data_terminal_ready(false).await?;

        serial.delay_ms(100).await;

        serial.write_request_to_send(true).await?;
        serial.write_data_terminal_ready(false).await?;
        serial.write_request_to_send(true).await?;

        serial.delay_ms(100).await;

        serial.write_request_to_send(false).await?;
    } else {
        serial.write_request_to_send(true).await?;

        serial.delay_ms(100).await;

        serial.write_request_to_send(false).await?;
    }

    Ok(())
}

/// Performs a hard reset of the chip.
pub async fn hard_reset<P: SerialInterface>(serial_port: &mut P, pid: u16) -> Result<(), Error> {
    debug!("Using HardReset reset strategy");

    // Using esptool HardReset strategy (https://github.com/espressif/esptool/blob/3301d0ff4638d4db1760a22540dbd9d07c55ec37/esptool/reset.py#L132-L153)
    // leads to https://github.com/esp-rs/espflash/issues/592 in Windows, using `reset_after_flash` instead works fine for all platforms.
    // We had similar issues in the past: https://github.com/esp-rs/espflash/pull/157
    reset_after_flash(serial_port, pid).await?;

    Ok(())
}

/// Performs a soft reset of the device.
pub async fn soft_reset<P: SerialInterface>(
    connection: &mut Connection<P>,
    stay_in_bootloader: bool,
    is_stub: bool,
) -> Result<(), Error> {
    debug!("Using SoftReset reset strategy");
    if !is_stub {
        if stay_in_bootloader {
            // ROM bootloader is already in bootloader
            return Ok(());
        } else {
            //  'run user code' is as close to a soft reset as we can do
            let old_timeout = connection.serial.timeout();
            connection.serial.set_timeout(CommandType::FlashBegin.timeout())?;
            let size: u32 = 0;
            let offset: u32 = 0;
            let blocks: u32 = size.div_ceil(FLASH_WRITE_SIZE as u32);
            let result = connection.command(Command::FlashBegin {
                size,
                blocks,
                block_size: FLASH_WRITE_SIZE.try_into().unwrap(),
                offset,
                supports_encryption: false,
            }).await;
            connection.serial.set_timeout(old_timeout)?;
            result?;

            let old_timeout = connection.serial.timeout();
            connection.serial.set_timeout(CommandType::FlashEnd.timeout())?;
            let result = connection.write_command(Command::FlashEnd { reboot: false }).await;
            connection.serial.set_timeout(old_timeout)?;
            result?;
        }
    } else if stay_in_bootloader {
        // Soft resetting from the stub loader will re-load the ROM bootloader
        let old_timeout = connection.serial.timeout();
        connection.serial.set_timeout(CommandType::FlashBegin.timeout())?;
        let size: u32 = 0;
        let offset: u32 = 0;
        let blocks: u32 = size.div_ceil(FLASH_WRITE_SIZE as u32);
        let result = connection.command(Command::FlashBegin {
            size,
            blocks,
            block_size: FLASH_WRITE_SIZE.try_into().unwrap(),
            offset,
            supports_encryption: false,
        }).await;
        connection.serial.set_timeout(old_timeout)?;
        result?;

        let old_timeout = connection.serial.timeout();
        connection.serial.set_timeout(CommandType::FlashEnd.timeout())?;
        let result = connection.write_command(Command::FlashEnd { reboot: true }).await;
        connection.serial.set_timeout(old_timeout)?;
        result?;
    } else {
        // Running user code from stub loader requires some hacks in the stub loader
        let old_timeout = connection.serial.timeout();
        connection.serial.set_timeout(CommandType::RunUserCode.timeout())?;
        let result = connection.command(Command::RunUserCode).await;
        connection.serial.set_timeout(old_timeout)?;
        result?;
    }

    Ok(())
}

/// Constructs a sequence of reset strategies based on the OS and chip.
///
/// Returns a [Vec] containing one or more reset strategies to be attempted
/// sequentially.
#[allow(unused_variables)]
pub fn construct_reset_strategy_sequence(
    port_name: &str,
    pid: u16,
    mode: ResetBeforeOperation,
) -> Vec<ResetStrategy> {
    // USB-JTAG/Serial mode
    if pid == USB_SERIAL_JTAG_PID || mode == ResetBeforeOperation::UsbReset {
        return vec![ResetStrategy::usb_jtag_serial()];
    }

    // USB-to-Serial bridge
    #[cfg(unix)]
    if cfg!(unix) && !port_name.starts_with("rfc2217:") {
        return vec![
            ResetStrategy::unix_tight(false),
            ResetStrategy::unix_tight(true),
            ResetStrategy::classic(false),
            ResetStrategy::classic(true),
        ];
    }

    // Windows
    vec![
        ResetStrategy::classic(false),
        ResetStrategy::classic(true),
    ]
}

/// Enum to represent different reset behaviors before an operation.
#[derive(
    Debug,
    Default,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Display,
    EnumIter,
    EnumString,
    VariantNames,
    Hash,
    Serialize,
    Deserialize,
)]
#[non_exhaustive]
#[strum(serialize_all = "lowercase")]
pub enum ResetBeforeOperation {
    /// Uses DTR & RTS serial control lines to try to reset the chip into
    /// bootloader mode.
    #[default]
    DefaultReset,
    /// Skips DTR/RTS control signal assignments and just start sending a serial
    /// synchronisation command to the chip.
    NoReset,
    /// Skips DTR/RTS control signal assignments and also skips the serial
    /// synchronization command.
    NoResetNoSync,
    /// Reset sequence for USB-JTAG-Serial peripheral.
    UsbReset,
}

/// Enum to represent different reset behaviors after an operation.
#[derive(
    Debug,
    Default,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Display,
    EnumIter,
    EnumString,
    VariantNames,
    Hash,
    Serialize,
    Deserialize,
)]
#[non_exhaustive]
pub enum ResetAfterOperation {
    /// The DTR serial control line is used to reset the chip into a normal boot
    /// sequence.
    #[default]
    HardReset,
    /// Leaves the chip in the serial bootloader, no reset is performed.
    NoReset,
    /// Leaves the chip in the stub bootloader, no reset is performed.
    NoResetNoStub,
    /// Hard-resets the chip by triggering an internal watchdog reset.
    WatchdogReset,
}

//! Establish a connection with a target device.
//!
//! The [Connection] struct abstracts over the serial connection and
//! sending/decoding of commands, and provides higher-level operations with the
//! device.

use alloc::collections::BTreeMap;
use core::{fmt, iter::zip, time::Duration};
use embedded_io_async::Write;

use embedded_hal_async::delay::DelayNs;

use log::{debug, info};
use regex::Regex;
use serde::{Deserialize, Serialize};
use serialport::{SerialPort, UsbPortInfo};
use slip_codec::SlipDecoder;

use self::{
    encoder::SlipEncoder,
    reset::{
        ResetStrategy,
        construct_reset_strategy_sequence,
        hard_reset,
        reset_after_flash,
        soft_reset,
    },
};
use super::{
    command::{Command, CommandResponse, CommandResponseValue, CommandType},
    error::{ConnectionError, Error, ResultExt, RomError, RomErrorKind},
    flasher::stubs::CHIP_DETECT_MAGIC_REG_ADDR,
    target::Chip,
};

pub(crate) mod reset;

pub use reset::{ResetAfterOperation, ResetBeforeOperation};

const MAX_CONNECT_ATTEMPTS: usize = 7;
const MAX_SYNC_ATTEMPTS: usize = 5;
const USB_SERIAL_JTAG_PID: u16 = 0x1001;

#[cfg(unix)]
/// Alias for the serial TTYPort.
pub type Port = serialport::TTYPort;
#[cfg(windows)]
/// Alias for the serial COMPort.
pub type Port = serialport::COMPort;

/// Trait alias for types that can be used as a serial port.
pub trait SerialInterface: SerialPort + Send {}

/// Blanket implementation for all types that implement SerialPort + Send.
impl<T: SerialPort + Send> SerialInterface for T {}

/// A delay implementation using std::thread::sleep.
///
/// This is the default delay provider for host-side flashing operations.
#[derive(Debug, Clone, Copy, Default)]
pub struct StdDelay;

impl DelayNs for StdDelay {
    async fn delay_ns(&mut self, ns: u32) {
        std::thread::sleep(Duration::from_nanos(ns as u64));
    }
}

/// Security Info Response containing chip security information
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize, Serialize)]
pub struct SecurityInfo {
    /// 32 bits flags
    pub flags: u32,
    /// 1 byte flash_crypt_cnt
    pub flash_crypt_cnt: u8,
    /// 7 bytes key purposes
    pub key_purposes: [u8; 7],
    /// 32-bit word chip id
    pub chip_id: Option<u32>,
    /// 32-bit word eco version
    pub eco_version: Option<u32>,
}

impl SecurityInfo {
    fn security_flag_map() -> BTreeMap<&'static str, u32> {
        BTreeMap::from([
            ("SECURE_BOOT_EN", 1 << 0),
            ("SECURE_BOOT_AGGRESSIVE_REVOKE", 1 << 1),
            ("SECURE_DOWNLOAD_ENABLE", 1 << 2),
            ("SECURE_BOOT_KEY_REVOKE0", 1 << 3),
            ("SECURE_BOOT_KEY_REVOKE1", 1 << 4),
            ("SECURE_BOOT_KEY_REVOKE2", 1 << 5),
            ("SOFT_DIS_JTAG", 1 << 6),
            ("HARD_DIS_JTAG", 1 << 7),
            ("DIS_USB", 1 << 8),
            ("DIS_DOWNLOAD_DCACHE", 1 << 9),
            ("DIS_DOWNLOAD_ICACHE", 1 << 10),
        ])
    }

    pub(crate) fn security_flag_status(&self, flag_name: &str) -> bool {
        if let Some(&flag) = Self::security_flag_map().get(flag_name) {
            (self.flags & flag) != 0
        } else {
            false
        }
    }
}

impl TryFrom<&[u8]> for SecurityInfo {
    type Error = Error;

    fn try_from(bytes: &[u8]) -> Result<Self, Self::Error> {
        let esp32s2 = bytes.len() == 12;

        if bytes.len() < 12 {
            return Err(Error::InvalidResponse(format!(
                "expected response of at least 12 bytes, received {} bytes",
                bytes.len()
            )));
        }

        // Parse response bytes
        let flags = u32::from_le_bytes(bytes[0..4].try_into()?);
        let flash_crypt_cnt = bytes[4];
        let key_purposes: [u8; 7] = bytes[5..12].try_into()?;

        let (chip_id, eco_version) = if esp32s2 {
            (None, None) // ESP32-S2 doesn't have these values
        } else {
            if bytes.len() < 20 {
                return Err(Error::InvalidResponse(format!(
                    "expected response of at least 20 bytes, received {} bytes",
                    bytes.len()
                )));
            }
            let chip_id = u32::from_le_bytes(bytes[12..16].try_into()?);
            let eco_version = u32::from_le_bytes(bytes[16..20].try_into()?);
            (Some(chip_id), Some(eco_version))
        };

        Ok(SecurityInfo {
            flags,
            flash_crypt_cnt,
            key_purposes,
            chip_id,
            eco_version,
        })
    }
}

impl fmt::Display for SecurityInfo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let key_purposes_str = self
            .key_purposes
            .iter()
            .map(|b| format!("{b}"))
            .collect::<Vec<_>>()
            .join(", ");

        writeln!(f, "\nSecurity Information:")?;
        writeln!(f, "=====================")?;
        writeln!(f, "Flags: {:#010x} ({:b})", self.flags, self.flags)?;
        writeln!(f, "Key Purposes: [{key_purposes_str}]")?;

        // Only print Chip ID if it's Some(value)
        if let Some(chip_id) = self.chip_id {
            writeln!(f, "Chip ID: {chip_id}")?;
        }

        // Only print API Version if it's Some(value)
        if let Some(api_version) = self.eco_version {
            writeln!(f, "API Version: {api_version}")?;
        }

        // Secure Boot
        if self.security_flag_status("SECURE_BOOT_EN") {
            writeln!(f, "Secure Boot: Enabled")?;
            if self.security_flag_status("SECURE_BOOT_AGGRESSIVE_REVOKE") {
                writeln!(f, "Secure Boot Aggressive key revocation: Enabled")?;
            }

            let revoked_keys: Vec<_> = [
                "SECURE_BOOT_KEY_REVOKE0",
                "SECURE_BOOT_KEY_REVOKE1",
                "SECURE_BOOT_KEY_REVOKE2",
            ]
            .iter()
            .enumerate()
            .filter(|(_, key)| self.security_flag_status(key))
            .map(|(i, _)| format!("Secure Boot Key{i} is Revoked"))
            .collect();

            if !revoked_keys.is_empty() {
                writeln!(
                    f,
                    "Secure Boot Key Revocation Status:\n  {}",
                    revoked_keys.join("\n  ")
                )?;
            }
        } else {
            writeln!(f, "Secure Boot: Disabled")?;
        }

        // Flash Encryption
        if self.flash_crypt_cnt.count_ones() % 2 != 0 {
            writeln!(f, "Flash Encryption: Enabled")?;
        } else {
            writeln!(f, "Flash Encryption: Disabled")?;
        }

        let crypt_cnt_str = "SPI Boot Crypt Count (SPI_BOOT_CRYPT_CNT)";
        writeln!(f, "{}: 0x{:x}", crypt_cnt_str, self.flash_crypt_cnt)?;

        // Cache Disabling
        if self.security_flag_status("DIS_DOWNLOAD_DCACHE") {
            writeln!(f, "Dcache in UART download mode: Disabled")?;
        }
        if self.security_flag_status("DIS_DOWNLOAD_ICACHE") {
            writeln!(f, "Icache in UART download mode: Disabled")?;
        }

        // JTAG Status
        if self.security_flag_status("HARD_DIS_JTAG") {
            writeln!(f, "JTAG: Permanently Disabled")?;
        } else if self.security_flag_status("SOFT_DIS_JTAG") {
            writeln!(f, "JTAG: Software Access Disabled")?;
        }

        // USB Access
        if self.security_flag_status("DIS_USB") {
            writeln!(f, "USB Access: Disabled")?;
        }

        Ok(())
    }
}

/// An established connection with a target device.
pub struct Connection<P: SerialInterface> {
    pub serial: P,
    port_info: UsbPortInfo,
    decoder: SlipDecoder,
    after_operation: ResetAfterOperation,
    before_operation: ResetBeforeOperation,
    pub(crate) secure_download_mode: bool,
    pub(crate) baud: u32,
    delay: StdDelay,
}

impl<P: SerialInterface + fmt::Debug> fmt::Debug for Connection<P> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Connection")
            .field("serial", &self.serial)
            .field("port_info", &self.port_info)
            .field("after_operation", &self.after_operation)
            .field("before_operation", &self.before_operation)
            .field("secure_download_mode", &self.secure_download_mode)
            .field("baud", &self.baud)
            .field("delay", &self.delay)
            .finish()
    }
}

impl<P: SerialInterface> Connection<P> {
    /// Creates a new connection with a target device.
    pub fn new(
        serial: P,
        port_info: UsbPortInfo,
        after_operation: ResetAfterOperation,
        before_operation: ResetBeforeOperation,
        baud: u32,
    ) -> Self {
        Connection {
            serial,
            port_info,
            decoder: SlipDecoder::new(),
            after_operation,
            before_operation,
            secure_download_mode: false,
            baud,
            delay: StdDelay,
        }
    }

    /// Returns a mutable reference to the delay provider.
    pub fn delay(&mut self) -> &mut StdDelay {
        &mut self.delay
    }

    /// Initializes a connection with a device.
    pub async fn begin(&mut self) -> Result<(), Error> {
        let port_name = self.serial.name().unwrap_or_default();
        let reset_sequence = construct_reset_strategy_sequence(
            &port_name,
            self.port_info.pid,
            self.before_operation,
        );

        for (_, reset_strategy) in zip(0..MAX_CONNECT_ATTEMPTS, reset_sequence.iter().cycle()) {
            match self.connect_attempt(reset_strategy).await {
                Ok(_) => {
                    return Ok(());
                }
                Err(e) => {
                    debug!("Failed to reset, error {e:#?}, retrying");
                }
            }
        }

        Err(Error::Connection(Box::new(
            ConnectionError::ConnectionFailed,
        )))
    }

    /// Connects to a device.
    async fn connect_attempt(&mut self, reset_strategy: &ResetStrategy) -> Result<(), Error> {
        // If we're doing no_sync, we're likely communicating as a pass through
        // with an intermediate device to the ESP32
        if self.before_operation == ResetBeforeOperation::NoResetNoSync {
            return Ok(());
        }
        let mut download_mode: bool = false;
        let mut boot_mode = String::new();
        let mut boot_log_detected = false;
        let mut buff: Vec<u8>;
        if self.before_operation != ResetBeforeOperation::NoReset {
            // Reset the chip to bootloader (download mode)
            reset_strategy.reset(&mut self.serial, &mut self.delay).await?;

            // S2 in USB download mode responds with 0 available bytes here
            let available_bytes = self.serial.bytes_to_read()?;

            buff = vec![0; available_bytes as usize];
            let read_bytes = if available_bytes > 0 {
                let read_bytes = self.serial.read(&mut buff)? as u32;

                if read_bytes != available_bytes {
                    return Err(Error::Connection(Box::new(ConnectionError::ReadMismatch(
                        available_bytes,
                        read_bytes,
                    ))));
                }
                read_bytes
            } else {
                0
            };

            let read_slice = String::from_utf8_lossy(&buff[..read_bytes as usize]).into_owned();

            let pattern =
                Regex::new(r"boot:(0x[0-9a-fA-F]+)([\s\S]*waiting for download)?").unwrap();

            // Search for the pattern in the read data
            if let Some(data) = pattern.captures(&read_slice) {
                boot_log_detected = true;
                // Boot log detected
                boot_mode = data
                    .get(1)
                    .map(|m| m.as_str())
                    .unwrap_or_default()
                    .to_string();
                download_mode = data.get(2).is_some();

                // Further processing or printing the results
                debug!("Boot Mode: {boot_mode}");
                debug!("Download Mode: {download_mode}");
            };
        }

        for _ in 0..MAX_SYNC_ATTEMPTS {
            self.flush()?;

            if self.sync().await.is_ok() {
                return Ok(());
            }
        }

        if boot_log_detected {
            if download_mode {
                return Err(Error::Connection(Box::new(ConnectionError::NoSyncReply)));
            } else {
                return Err(Error::Connection(Box::new(ConnectionError::WrongBootMode(
                    boot_mode.to_string(),
                ))));
            }
        }

        Err(Error::Connection(Box::new(
            ConnectionError::ConnectionFailed,
        )))
    }

    /// Syncs with a device.
    pub(crate) async fn sync(&mut self) -> Result<(), Error> {
        let old_timeout = self.serial.timeout();
        self.serial.set_timeout(CommandType::Sync.timeout())?;

        let result = async {
            self.command(Command::Sync).await?;
            self.flush()?;

            self.delay.delay_ms(10).await;

            for _ in 0..MAX_CONNECT_ATTEMPTS {
                match self.read_response()? {
                    Some(response) if response.return_op == CommandType::Sync as u8 => {
                        if response.status == 1 {
                            self.flush().ok();
                            return Err(Error::RomError(Box::new(RomError::new(
                                CommandType::Sync,
                                RomErrorKind::from(response.error),
                            ))));
                        }
                    }
                    _ => {
                        return Err(Error::RomError(Box::new(RomError::new(
                            CommandType::Sync,
                            RomErrorKind::InvalidMessage,
                        ))));
                    }
                }
            }

            Ok(())
        }.await;

        self.serial.set_timeout(old_timeout)?;
        result
    }

    /// Resets the device.
    pub async fn reset(&mut self) -> Result<(), Error> {
        reset_after_flash(&mut self.serial, self.port_info.pid, &mut self.delay).await?;
        Ok(())
    }

    /// Resets the device taking into account the reset after argument.
    pub async fn reset_after(&mut self, is_stub: bool, chip: Chip) -> Result<(), Error> {
        let pid = self.usb_pid();

        match self.after_operation {
            ResetAfterOperation::HardReset => hard_reset(&mut self.serial, pid, &mut self.delay).await,
            ResetAfterOperation::NoReset => {
                info!("Staying in bootloader");
                soft_reset(self, true, is_stub).await?;

                Ok(())
            }
            ResetAfterOperation::NoResetNoStub => {
                info!("Staying in flasher stub");
                Ok(())
            }
            ResetAfterOperation::WatchdogReset => {
                info!("Resetting device with watchdog");

                match chip {
                    Chip::Esp32c3 => {
                        if self.is_using_usb_serial_jtag() {
                            chip.rtc_wdt_reset(self).await?;
                        }
                    }
                    Chip::Esp32p4 => {
                        // Check if the connection is USB OTG
                        if chip.is_using_usb_otg(self).await? {
                            chip.rtc_wdt_reset(self).await?;
                        }
                    }
                    Chip::Esp32s2 => {
                        // Check if the connection is USB OTG
                        if chip.is_using_usb_otg(self).await? {
                            // Check the strapping register to see if we can perform RTC WDT
                            // reset
                            if chip.can_rtc_wdt_reset(self).await? {
                                chip.rtc_wdt_reset(self).await?;
                            }
                        }
                    }
                    Chip::Esp32s3 => {
                        if self.is_using_usb_serial_jtag() || chip.is_using_usb_otg(self).await? {
                            // Check the strapping register to see if we can perform RTC WDT
                            // reset
                            if chip.can_rtc_wdt_reset(self).await? {
                                chip.rtc_wdt_reset(self).await?;
                            }
                        }
                    }
                    _ => {
                        return Err(Error::UnsupportedFeature {
                            chip,
                            feature: "watchdog reset".into(),
                        });
                    }
                }

                Ok(())
            }
        }
    }

    /// Resets the device to flash mode.
    pub async fn reset_to_flash(&mut self, extra_delay: bool) -> Result<(), Error> {
        if self.is_using_usb_serial_jtag() {
            ResetStrategy::usb_jtag_serial().reset(&mut self.serial, &mut self.delay).await
        } else {
            #[cfg(unix)]
            if ResetStrategy::unix_tight(extra_delay)
                .reset(&mut self.serial, &mut self.delay)
                .await
                .is_ok()
            {
                return Ok(());
            }

            ResetStrategy::classic(extra_delay).reset(&mut self.serial, &mut self.delay).await
        }
    }

    /// Sets the timeout for the serial port.
    pub fn set_timeout(&mut self, timeout: Duration) -> Result<(), Error> {
        self.serial.set_timeout(timeout)?;
        Ok(())
    }

    /// Sets the baud rate for the serial port.
    pub fn set_baud(&mut self, baud: u32) -> Result<(), Error> {
        self.serial.set_baud_rate(baud)?;
        self.baud = baud;
        Ok(())
    }

    /// Returns the current baud rate of the serial port.
    pub fn baud(&self) -> Result<u32, Error> {
        Ok(self.serial.baud_rate()?)
    }

    /// Runs a command with a timeout defined by the command type.
    pub async fn with_timeout<T, F, Fut>(&mut self, timeout: Duration, f: F) -> Result<T, Error>
    where
        F: FnOnce(&mut Connection<P>) -> Fut,
        Fut: core::future::Future<Output = Result<T, Error>>,
    {
        let old_timeout = {
            let mut binding = Box::new(&mut self.serial);
            let serial = binding.as_mut();
            let old_timeout = serial.timeout();
            serial.set_timeout(timeout)?;
            old_timeout
        };

        let result = f(self).await;

        self.serial.set_timeout(old_timeout)?;

        result
    }

    /// Reads the response from a serial port.
    pub fn read_flash_response(&mut self) -> Result<Option<CommandResponse>, Error> {
        let mut response = Vec::new();

        self.decoder.decode(&mut self.serial, &mut response)?;

        if response.is_empty() {
            return Ok(None);
        }
        let value = CommandResponseValue::Vector(response.clone());

        let header = CommandResponse {
            resp: 1_u8,
            return_op: CommandType::ReadFlash as u8,
            return_length: response.len() as u16,
            value,
            error: 0_u8,
            status: 0_u8,
        };

        Ok(Some(header))
    }

    /// Reads the response from a serial port.
    pub fn read_response(&mut self) -> Result<Option<CommandResponse>, Error> {
        match self.read(10)? {
            None => Ok(None),
            Some(response) => {
                // Here is what esptool does: https://github.com/espressif/esptool/blob/81b2eaee261aed0d3d754e32c57959d6b235bfed/esptool/loader.py#L518
                // from esptool: things are a bit weird here, bear with us

                // We rely on the known and expected response sizes which should be fine for now
                // - if that changes we need to pass the command type we are parsing the
                // response for.
                //
                // For most commands the response length is 10 (for the stub) or 12 (for ROM
                // code). The MD5 command response is 44 for ROM loader, 26 for the stub.
                //
                // See:
                // - https://docs.espressif.com/projects/esptool/en/latest/esp32/advanced-topics/serial-protocol.html?highlight=md5#response-packet
                // - https://docs.espressif.com/projects/esptool/en/latest/esp32/advanced-topics/serial-protocol.html?highlight=md5#status-bytes
                // - https://docs.espressif.com/projects/esptool/en/latest/esp32/advanced-topics/serial-protocol.html?highlight=md5#verifying-uploaded-data

                let status_len = if response.len() == 10 || response.len() == 26 {
                    2
                } else {
                    4
                };

                let value = match response.len() {
                    10 | 12 => CommandResponseValue::ValueU32(u32::from_le_bytes(
                        response[4..][..4].try_into()?,
                    )),
                    // MD5 is in ASCII
                    44 => CommandResponseValue::ValueU128(u128::from_str_radix(
                        core::str::from_utf8(&response[8..][..32])?,
                        16,
                    )?),
                    // MD5 is BE bytes
                    26 => CommandResponseValue::ValueU128(u128::from_be_bytes(
                        response[8..][..16].try_into()?,
                    )),
                    _ => CommandResponseValue::Vector(response.clone()),
                };

                let header = CommandResponse {
                    resp: response[0],
                    return_op: response[1],
                    return_length: u16::from_le_bytes(response[2..][..2].try_into()?),
                    value,
                    error: response[response.len() - status_len + 1],
                    status: response[response.len() - status_len],
                };

                Ok(Some(header))
            }
        }
    }

    /// Writes raw data to the serial port.
    pub async fn write_raw(&mut self, data: u32) -> Result<(), Error> {
        use std::io::Write as StdWrite;

        self.serial.clear(serialport::ClearBuffer::Input)?;

        // Serialize and SLIP-encode to a buffer (infallible operations)
        let mut buf = Vec::new();
        let mut writer = io::VecWriter::new(&mut buf);
        let mut encoder = SlipEncoder::new(&mut writer).await.unwrap();
        encoder.write_all(&data.to_le_bytes()).await.unwrap();
        encoder.finish().await.unwrap();

        // Write buffer to serial port
        self.serial.write_all(&buf)?;
        StdWrite::flush(&mut self.serial)?;
        Ok(())
    }

    /// Writes a command to the serial port.
    pub async fn write_command(&mut self, command: Command<'_>) -> Result<(), Error> {
        use std::io::Write as StdWrite;

        debug!("Writing command: {command:02x?}");
        self.serial.clear(serialport::ClearBuffer::Input)?;

        // Serialize and SLIP-encode to a buffer (infallible operations)
        let mut buf = Vec::new();
        let mut writer = io::VecWriter::new(&mut buf);
        let mut encoder = SlipEncoder::new(&mut writer).await.unwrap();
        command.write(&mut encoder).await.unwrap();
        encoder.finish().await.unwrap();

        // Write buffer to serial port
        self.serial.write_all(&buf)?;
        StdWrite::flush(&mut self.serial)?;
        Ok(())
    }

    /// Writes a command and reads the response.
    pub async fn command(&mut self, command: Command<'_>) -> Result<CommandResponseValue, Error> {
        let ty = command.command_type();
        self.write_command(command).await.for_command(ty)?;
        for _ in 0..100 {
            match self.read_response().for_command(ty)? {
                Some(response) if response.return_op == ty as u8 => {
                    return if response.status != 0 {
                        let _error = self.flush();
                        Err(Error::RomError(Box::new(RomError::new(
                            command.command_type(),
                            RomErrorKind::from(response.error),
                        ))))
                    } else {
                        // Check if the response is a Vector and strip header (first 8 bytes)
                        // https://github.com/espressif/esptool/blob/749d1ad/esptool/loader.py#L481
                        let modified_value = match response.value {
                            CommandResponseValue::Vector(mut vec) if vec.len() >= 8 => {
                                vec = vec[8..][..response.return_length as usize].to_vec();
                                CommandResponseValue::Vector(vec)
                            }
                            _ => response.value, // If not Vector, return as is
                        };

                        Ok(modified_value)
                    };
                }
                _ => continue,
            }
        }
        Err(Error::Connection(Box::new(
            ConnectionError::ConnectionFailed,
        )))
    }

    /// Reads a register command with a timeout.
    pub async fn read_reg(&mut self, addr: u32) -> Result<u32, Error> {
        let old_timeout = self.serial.timeout();
        self.serial.set_timeout(CommandType::ReadReg.timeout())?;
        let resp = self.command(Command::ReadReg { address: addr }).await;
        self.serial.set_timeout(old_timeout)?;
        resp?.try_into()
    }

    /// Writes a register command with a timeout.
    pub async fn write_reg(&mut self, addr: u32, value: u32, mask: Option<u32>) -> Result<(), Error> {
        let old_timeout = self.serial.timeout();
        self.serial.set_timeout(CommandType::WriteReg.timeout())?;
        let result = self.command(Command::WriteReg {
            address: addr,
            value,
            mask,
        }).await;
        self.serial.set_timeout(old_timeout)?;
        result?;
        Ok(())
    }

    /// Updates a register by applying the new value to the masked out portion
    /// of the old value.
    #[allow(dead_code)]
    pub(crate) async fn update_reg(&mut self, addr: u32, mask: u32, new_value: u32) -> Result<(), Error> {
        let masked_new_value = new_value.checked_shl(mask.trailing_zeros()).unwrap_or(0) & mask;

        let masked_old_value = self.read_reg(addr).await? & !mask;

        self.write_reg(addr, masked_old_value | masked_new_value, None).await
    }

    /// Reads a register command with a timeout.
    pub(crate) fn read(&mut self, len: usize) -> Result<Option<Vec<u8>>, Error> {
        let mut tmp = Vec::with_capacity(1024);
        loop {
            self.decoder.decode(&mut self.serial, &mut tmp)?;
            if tmp.len() >= len {
                return Ok(Some(tmp));
            }
        }
    }

    /// Flushes  the serial port.
    pub fn flush(&mut self) -> Result<(), Error> {
        self.serial.flush()?;
        Ok(())
    }

    /// Turns a connection into its serial port.
    pub fn into_serial(self) -> P {
        self.serial
    }

    /// Returns the USB PID of the serial port.
    pub fn usb_pid(&self) -> u16 {
        self.port_info.pid
    }

    /// Returns if the connection is using USB serial JTAG.
    pub(crate) fn is_using_usb_serial_jtag(&self) -> bool {
        self.port_info.pid == USB_SERIAL_JTAG_PID
    }

    /// Returns the reset after operation.
    pub fn after_operation(&self) -> ResetAfterOperation {
        self.after_operation
    }

    /// Returns the reset before operation.
    pub fn before_operation(&self) -> ResetBeforeOperation {
        self.before_operation
    }

    /// Gets security information from the chip.

    pub async fn security_info(&mut self, use_stub: bool) -> Result<SecurityInfo, super::error::Error> {
        let old_timeout = self.serial.timeout();
        self.serial.set_timeout(CommandType::GetSecurityInfo.timeout())?;
        let response = self.command(Command::GetSecurityInfo).await;
        self.serial.set_timeout(old_timeout)?;
        let response = response?;
        // Extract raw bytes and convert them into `SecurityInfo`
        if let super::command::CommandResponseValue::Vector(data) = response {
            // HACK: Not quite sure why there seem to be 4 extra bytes at the end of the
            //       response when the stub is not being used...
            let end = if use_stub { data.len() } else { data.len() - 4 };
            SecurityInfo::try_from(&data[..end])
        } else {
            Err(Error::InvalidResponse(
                "response was not a vector of bytes".into(),
            ))
        }
    }

    /// Detects which chip is connected to this connection.

    pub async fn detect_chip(
        &mut self,
        use_stub: bool,
    ) -> Result<super::target::Chip, super::error::Error> {
        match self.security_info(use_stub).await {
            Ok(info) if info.chip_id.is_some() => {
                let chip_id = info.chip_id.unwrap() as u16;
                let chip = Chip::try_from(chip_id)?;

                Ok(chip)
            }
            _ => {
                // Fall back to reading the magic value from the chip
                let magic = if use_stub {
                    let old_timeout = self.serial.timeout();
                    self.serial.set_timeout(CommandType::ReadReg.timeout())?;
                    let result = self.command(Command::ReadReg {
                        address: CHIP_DETECT_MAGIC_REG_ADDR,
                    }).await;
                    self.serial.set_timeout(old_timeout)?;
                    result?.try_into()?
                } else {
                    self.read_reg(CHIP_DETECT_MAGIC_REG_ADDR).await?
                };
                debug!("Read chip magic value: 0x{magic:08x}");
                Chip::from_magic(magic)
            }
        }
    }
}

/// I/O adapters for bridging embedded_io_async with std::io
mod io {
    use alloc::vec::Vec;
    use core::convert::Infallible;

    /// A writer that appends to a Vec<u8>, implementing embedded_io_async::Write.
    pub struct VecWriter<'a> {
        vec: &'a mut Vec<u8>,
    }

    impl<'a> VecWriter<'a> {
        pub fn new(vec: &'a mut Vec<u8>) -> Self {
            Self { vec }
        }
    }

    impl embedded_io_async::ErrorType for VecWriter<'_> {
        type Error = Infallible;
    }

    impl embedded_io_async::Write for VecWriter<'_> {
        async fn write(&mut self, buf: &[u8]) -> Result<usize, Self::Error> {
            self.vec.extend_from_slice(buf);
            Ok(buf.len())
        }

        async fn flush(&mut self) -> Result<(), Self::Error> {
            Ok(())
        }
    }
}

mod encoder {
    use embedded_io_async::Write;

    use serde::Serialize;

    const END: u8 = 0xC0;
    const ESC: u8 = 0xDB;
    const ESC_END: u8 = 0xDC;
    const ESC_ESC: u8 = 0xDD;

    /// Encoder for the SLIP protocol.
    #[derive(Debug, PartialEq, Eq, Serialize, Hash)]
    pub struct SlipEncoder<'a, W: Write> {
        writer: &'a mut W,
        len: usize,
    }

    impl<'a, W: Write> SlipEncoder<'a, W> {
        /// Creates a new encoder context.
        pub async fn new(writer: &'a mut W) -> Result<Self, W::Error> {
            let len = writer.write(&[END]).await?;
            Ok(Self { writer, len })
        }

        /// Finishes the encoding.
        pub async fn finish(mut self) -> Result<usize, W::Error> {
            self.len += self.writer.write(&[END]).await?;
            Ok(self.len)
        }
    }

    impl<W: Write> embedded_io_async::ErrorType for SlipEncoder<'_, W> {
        type Error = W::Error;
    }

    impl<W: Write> Write for SlipEncoder<'_, W> {
        /// Writes the given buffer replacing the END and ESC bytes.
        ///
        /// See <https://docs.espressif.com/projects/esptool/en/latest/esp32c3/advanced-topics/serial-protocol.html#low-level-protocol>
        async fn write(&mut self, buf: &[u8]) -> Result<usize, Self::Error> {
            for value in buf.iter() {
                match *value {
                    END => {
                        self.len += self.writer.write(&[ESC, ESC_END]).await?;
                    }
                    ESC => {
                        self.len += self.writer.write(&[ESC, ESC_ESC]).await?;
                    }
                    _ => {
                        self.len += self.writer.write(&[*value]).await?;
                    }
                }
            }

            Ok(buf.len())
        }

        async fn flush(&mut self) -> Result<(), Self::Error> {
            self.writer.flush().await
        }
    }
}

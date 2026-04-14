//! Flashing support for STM32 chips (MGMT and UI) and ESP32 (NET).
//!
//! This module provides async flashing methods for CtlCore. It requires the `ctl` feature.

use super::core::CtlCore;
use super::port::{CtlPort, SetBaudRate, SetTimeout};
use super::stm::{self, Bootloader};
use crate::shared::chip_config::stm32::f072::{
    FLASH_BASE as F072_FLASH_BASE, PAGE_SIZE, WRITE_CHUNK_SIZE as F072_WRITE_CHUNK_SIZE,
};
use crate::shared::chip_config::stm32::f405::WRITE_CHUNK_SIZE as F405_WRITE_CHUNK_SIZE;
use crate::shared::chip_config::stm32::f405::{FLASH_BASE, SECTOR_SIZES, VERIFY_CHUNK_SIZE};
use crate::shared::chip_config::tlv::PADDING_BYTES;
use crate::shared::tlv::buffer;
use crate::shared::uart_config;
use crate::shared::{CtlToMgmt, HEADER_SIZE, MAX_VALUE_SIZE, MgmtToCtl, SYNC_WORD};
use espflash::connection::{ClearBufferType, SerialInterface, SerialPortError};
use std::io::{Error as IoError, ErrorKind};
use std::time::Duration;

const F072_OPTION_BYTES_BASE: u32 = 0x1FFF_F800;
const F072_DATA0_BLOCK_ADDR: u32 = F072_OPTION_BYTES_BASE + 0x04;

/// Information retrieved from the MGMT chip when it's in bootloader mode.
#[derive(Debug, Clone, Default)]
pub struct MgmtBootloaderInfo {
    /// Bootloader protocol version (e.g., 0x31 = v3.1).
    pub bootloader_version: u8,
    /// Chip product ID.
    pub chip_id: u16,
    /// Supported command codes.
    pub commands: [u8; 16],
    /// Number of valid commands in the `commands` array.
    pub command_count: usize,
    /// First 32 bytes of flash memory (vector table).
    pub flash_sample: Option<[u8; 32]>,
}

impl MgmtBootloaderInfo {
    /// Major version number (upper nibble of bootloader_version).
    pub fn version_major(&self) -> u8 {
        self.bootloader_version >> 4
    }

    /// Minor version number (lower nibble of bootloader_version).
    pub fn version_minor(&self) -> u8 {
        self.bootloader_version & 0x0F
    }

    /// Initial stack pointer from the vector table, if flash sample is available.
    pub fn sp(&self) -> Option<u32> {
        self.flash_sample
            .map(|f| u32::from_le_bytes([f[0], f[1], f[2], f[3]]))
    }

    /// Reset handler address from the vector table, if flash sample is available.
    pub fn reset_handler(&self) -> Option<u32> {
        self.flash_sample
            .map(|f| u32::from_le_bytes([f[4], f[5], f[6], f[7]]))
    }

    /// Whether the initial SP appears valid (points to SRAM).
    pub fn sp_valid(&self) -> bool {
        self.sp()
            .map_or(false, |sp| (0x2000_0000..0x2002_0000).contains(&sp))
    }

    /// Whether the reset handler appears valid (points to Flash, Thumb mode).
    pub fn reset_valid(&self) -> bool {
        self.reset_handler().map_or(false, |reset| {
            (0x0800_0000..0x0810_0000).contains(&reset) && (reset & 1) == 1
        })
    }
}

/// Phase of the flash operation, reported to progress callbacks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlashPhase {
    /// Compressing firmware data.
    Compressing,
    /// Erasing flash memory.
    Erasing,
    /// Writing firmware data.
    Writing,
    /// Verifying written data.
    Verifying,
}

impl core::fmt::Display for FlashPhase {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            FlashPhase::Compressing => write!(f, "compressing"),
            FlashPhase::Erasing => write!(f, "erasing"),
            FlashPhase::Writing => write!(f, "writing"),
            FlashPhase::Verifying => write!(f, "verifying"),
        }
    }
}

/// Errors that can occur during flash operations.
#[derive(Debug)]
pub enum FlashError<E> {
    /// Bootloader protocol error.
    Bootloader(stm::Error<E>),
    /// Verification failed - data read back doesn't match what was written.
    VerifyFailed {
        address: u32,
        expected: heapless::Vec<u8, VERIFY_CHUNK_SIZE>,
        actual: heapless::Vec<u8, VERIFY_CHUNK_SIZE>,
    },
}

impl<E> From<stm::Error<E>> for FlashError<E> {
    fn from(e: stm::Error<E>) -> Self {
        FlashError::Bootloader(e)
    }
}

/// Calculate the number of sectors needed for a given firmware size on STM32F405.
/// Sectors 0-3: 16KB each, Sector 4: 64KB, Sectors 5-11: 128KB each.
fn sectors_for_size_f405(size: usize) -> usize {
    let mut total = 0;
    for (i, &sector_size) in SECTOR_SIZES.iter().enumerate() {
        if total >= size {
            return i.max(1); // At least 1 sector
        }
        total += sector_size;
    }
    12 // All sectors needed
}

// ============================================================================
// Async TunnelPort for UI flashing through MGMT
// ============================================================================

/// Async tunnel port for flashing the UI chip through the MGMT tunnel.
///
/// This implements `CtlPort` by wrapping a mutable reference to a port and tunneling
/// data through FromUi/ToUi TLVs. The STM32 bootloader can use this
/// to communicate with the UI chip's bootloader.
struct TunnelPort<'a, P> {
    port: &'a mut P,
    buffer: heapless::Vec<u8, MAX_VALUE_SIZE>,
}

impl<'a, P> TunnelPort<'a, P> {
    fn new(port: &'a mut P) -> Self {
        Self {
            port,
            buffer: heapless::Vec::new(),
        }
    }
}

impl<P: CtlPort<Error = std::io::Error>> CtlPort for TunnelPort<'_, P> {
    type Error = std::io::Error;

    async fn read(&mut self, buf: &mut [u8]) -> Result<usize, Self::Error> {
        // Return buffered data first
        while self.buffer.is_empty() {
            // Read a complete TLV from the port
            // We need to accumulate bytes into a temporary buffer for parsing
            let mut temp_buf =
                heapless::Vec::<u8, { SYNC_WORD.len() + HEADER_SIZE + MAX_VALUE_SIZE }>::new();

            // Scan for sync word
            let mut matched = 0usize;
            while matched < SYNC_WORD.len() {
                let mut byte = [0u8; 1];
                self.port.read_exact(&mut byte).await?;
                let _ = temp_buf.push(byte[0]);
                if byte[0] == SYNC_WORD[matched] {
                    matched += 1;
                } else {
                    // Restart sync search - discard accumulated bytes
                    temp_buf.clear();
                    matched = 0;
                    if byte[0] == SYNC_WORD[0] {
                        let _ = temp_buf.push(byte[0]);
                        matched = 1;
                    }
                }
            }

            // Read header
            let mut header = [0u8; HEADER_SIZE];
            self.port.read_exact(&mut header).await?;
            let _ = temp_buf.extend_from_slice(&header);

            // Parse header to get length
            let raw_type = u16::from_le_bytes([header[0], header[1]]);
            let length = u32::from_le_bytes([header[2], header[3], header[4], header[5]]) as usize;

            if length > MAX_VALUE_SIZE {
                return Err(IoError::new(ErrorKind::InvalidData, "TLV too long"));
            }

            // Read value
            let mut value = heapless::Vec::<u8, MAX_VALUE_SIZE>::new();
            if value.resize(length, 0).is_err() {
                return Err(IoError::new(ErrorKind::InvalidData, "TLV too long"));
            }
            self.port.read_exact(&mut value).await?;

            // Check if it's FromUi
            if let Ok(tlv_type) = MgmtToCtl::try_from(raw_type) {
                if tlv_type == MgmtToCtl::FromUi {
                    let _ = self.buffer.extend_from_slice(&value);
                    break;
                }
            }
            // Not FromUi, continue reading
        }

        // Return data from buffer
        let to_copy = core::cmp::min(self.buffer.len(), buf.len());
        buf[..to_copy].copy_from_slice(&self.buffer[..to_copy]);

        // Drain from front
        let remaining = self.buffer.len() - to_copy;
        for i in 0..remaining {
            self.buffer[i] = self.buffer[i + to_copy];
        }
        self.buffer.truncate(remaining);

        Ok(to_copy)
    }

    async fn write_all(&mut self, buf: &[u8]) -> Result<(), Self::Error> {
        // Build ToUi TLV: sync_word + type + length + value
        let tlv_type: u16 = CtlToMgmt::ToUi.into();

        // Build complete packet to send atomically
        let mut packet = heapless::Vec::<u8, { SYNC_WORD.len() + 2 + 4 + MAX_VALUE_SIZE }>::new();
        let _ = packet.extend_from_slice(&SYNC_WORD);
        let _ = packet.extend_from_slice(&tlv_type.to_le_bytes());
        let _ = packet.extend_from_slice(&(buf.len() as u32).to_le_bytes());
        let _ = packet.extend_from_slice(buf);

        self.port.write_all(&packet).await
    }

    async fn flush(&mut self) -> Result<(), Self::Error> {
        self.port.flush().await
    }

    async fn read_exact(&mut self, buf: &mut [u8]) -> Result<(), Self::Error> {
        let mut filled = 0;
        while filled < buf.len() {
            let n = self.read(&mut buf[filled..]).await?;
            if n == 0 {
                return Err(IoError::new(ErrorKind::UnexpectedEof, "unexpected EOF"));
            }
            filled += n;
        }
        Ok(())
    }
}

// ============================================================================
// Async TunnelSerialInterface for NET flashing through MGMT (espflash)
// ============================================================================

/// Trait for providing async delays.
///
/// This allows callers to provide platform-appropriate delay implementations
/// (e.g., `std::thread::sleep` for native, browser timers for WASM).
#[allow(async_fn_in_trait)]
pub trait AsyncDelay {
    /// Delay for the specified number of milliseconds.
    async fn delay_ms(&self, ms: u32);
}

/// Default delay implementation using `std::thread::sleep`.
///
/// This works for native platforms but will panic on WASM.
#[derive(Clone, Copy)]
pub struct StdDelay;

impl AsyncDelay for StdDelay {
    async fn delay_ms(&self, ms: u32) {
        std::thread::sleep(Duration::from_millis(ms as u64));
    }
}

/// Size of the TLV header (type: 2 bytes + length: 4 bytes).
const TLV_HEADER_SIZE: usize = 6;

/// Maximum size for the raw buffer (must hold at least one complete TLV).
const RAW_BUFFER_SIZE: usize = SYNC_WORD.len() + TLV_HEADER_SIZE + MAX_VALUE_SIZE + PADDING_BYTES;

/// Async serial interface for flashing the NET chip (ESP32) through the MGMT tunnel.
///
/// This implements `SerialInterface` for use with espflash. It owns the port `P` directly
/// since espflash's Connection takes ownership. After flashing, use `into_port()` to
/// get the port back. DTR/RTS signals are mapped to BOOT/RST pins.
///
/// The `D` type parameter provides the delay implementation, allowing platform-specific
/// delays (e.g., `StdDelay` for native, a JS-based delay for WASM).
///
/// ## Two-Stage Buffering
///
/// To handle timeouts gracefully, this struct uses two buffers:
/// - `raw_buffer`: Accumulates raw bytes from the port. Partial TLV data is preserved
///   across timeouts.
/// - `buffer`: Holds extracted FromNet TLV values ready for the SLIP decoder.
///
/// This ensures that SYNC responses inside partially-received TLVs are never lost
/// due to read timeouts.
pub struct TunnelSerialInterface<P, D> {
    port: P,
    delay: D,
    /// Raw bytes from the port, may contain partial TLVs.
    raw_buffer: heapless::Vec<u8, RAW_BUFFER_SIZE>,
    /// Extracted FromNet TLV values, ready for espflash's SLIP decoder.
    buffer: heapless::Vec<u8, MAX_VALUE_SIZE>,
    timeout: Duration,
    baud_rate: u32,
}

impl<P, D> TunnelSerialInterface<P, D> {
    /// Create a new TunnelSerialInterface for NET chip communication.
    pub fn new(port: P, baud_rate: u32, delay: D) -> Self {
        Self {
            port,
            delay,
            raw_buffer: heapless::Vec::new(),
            buffer: heapless::Vec::new(),
            timeout: Duration::from_secs(30), // Increased for large flash operations
            baud_rate,
        }
    }

    /// Consume this interface and return the underlying port.
    pub fn into_port(self) -> P {
        self.port
    }
}

impl<P: CtlPort<Error = std::io::Error>, D: AsyncDelay> TunnelSerialInterface<P, D> {
    /// Helper to convert io::Error to SerialPortError
    fn io_to_serial(e: std::io::Error) -> SerialPortError {
        SerialPortError::io(e.to_string())
    }

    /// Try to parse complete TLVs from the raw buffer.
    ///
    /// This function scans the raw buffer for complete TLVs (LINK + header + value).
    /// When a complete FromNet TLV is found, its value is appended to `self.buffer`
    /// and the TLV is removed from the raw buffer.
    ///
    /// Returns the number of TLVs successfully parsed.
    fn try_parse_tlvs(&mut self) -> usize {
        let mut parsed_count = 0;

        loop {
            // Try to parse a TLV using shared buffer utilities
            match buffer::try_parse_from_buffer::<MgmtToCtl>(&self.raw_buffer) {
                Ok(Some((tlv, consumed))) => {
                    // Successfully parsed a TLV
                    if tlv.tlv_type == MgmtToCtl::FromNet {
                        // Append FromNet value to buffer
                        let _ = self.buffer.extend_from_slice(&tlv.value);
                    }

                    // Remove consumed bytes from raw buffer
                    let new_len = self.raw_buffer.len() - consumed;
                    for i in 0..new_len {
                        self.raw_buffer[i] = self.raw_buffer[i + consumed];
                    }
                    self.raw_buffer.truncate(new_len);

                    parsed_count += 1;
                }
                Ok(None) => {
                    // No complete TLV yet - check if we should discard garbage
                    if let Some(sync_pos) = buffer::find_sync_word(&self.raw_buffer) {
                        // Sync word found but TLV incomplete - keep from sync word
                        if sync_pos > 0 {
                            let new_len = self.raw_buffer.len() - sync_pos;
                            for i in 0..new_len {
                                self.raw_buffer[i] = self.raw_buffer[i + sync_pos];
                            }
                            self.raw_buffer.truncate(new_len);
                        }
                    }
                    break;
                }
                Err(buffer::ParseError::TooLong) => {
                    // Invalid length - skip one byte and retry
                    let new_len = self.raw_buffer.len() - 1;
                    for i in 0..new_len {
                        self.raw_buffer[i] = self.raw_buffer[i + 1];
                    }
                    self.raw_buffer.truncate(new_len);
                }
                Err(_) => {
                    // Other parse error - skip one byte and retry
                    if self.raw_buffer.is_empty() {
                        break;
                    }
                    let new_len = self.raw_buffer.len() - 1;
                    for i in 0..new_len {
                        self.raw_buffer[i] = self.raw_buffer[i + 1];
                    }
                    self.raw_buffer.truncate(new_len);
                }
            }
        }

        parsed_count
    }

    /// Read available bytes from the port into the raw buffer.
    ///
    /// This is a non-blocking read that returns whatever bytes are currently available.
    /// Returns Ok(n) where n is the number of bytes read, or Err on timeout/error.
    async fn fill_raw_buffer(&mut self) -> Result<usize, std::io::Error> {
        // Calculate how much space is available
        let space = RAW_BUFFER_SIZE - self.raw_buffer.len();
        if space == 0 {
            return Ok(0);
        }

        // Read into a temporary buffer
        let read_size = space.min(1024);
        let mut tmp = [0u8; 1024];
        let n = self.port.read(&mut tmp[..read_size]).await?;

        // Append to raw buffer
        let _ = self.raw_buffer.extend_from_slice(&tmp[..n]);

        Ok(n)
    }

    /// Write a ToNet TLV to the port.
    async fn write_net_tlv(&mut self, data: &[u8]) -> Result<(), std::io::Error> {
        let tlv_type: u16 = CtlToMgmt::ToNet.into();

        // Build complete packet to send atomically
        let mut packet = heapless::Vec::<u8, { SYNC_WORD.len() + 2 + 4 + MAX_VALUE_SIZE }>::new();
        let _ = packet.extend_from_slice(&SYNC_WORD);
        let _ = packet.extend_from_slice(&tlv_type.to_le_bytes());
        let _ = packet.extend_from_slice(&(data.len() as u32).to_le_bytes());
        let _ = packet.extend_from_slice(data);

        self.port.write_all(&packet).await
    }

    /// Write a command TLV to MGMT without waiting for Ack.
    async fn write_mgmt_command(
        &mut self,
        cmd: CtlToMgmt,
        value: &[u8],
    ) -> Result<(), std::io::Error> {
        let tlv_type: u16 = cmd.into();

        // Build complete packet
        let mut packet = heapless::Vec::<u8, { SYNC_WORD.len() + 2 + 4 + MAX_VALUE_SIZE }>::new();
        let _ = packet.extend_from_slice(&SYNC_WORD);
        let _ = packet.extend_from_slice(&tlv_type.to_le_bytes());
        let _ = packet.extend_from_slice(&(value.len() as u32).to_le_bytes());
        let _ = packet.extend_from_slice(value);

        self.port.write_all(&packet).await?;
        self.port.flush().await
    }
}

impl<P: CtlPort<Error = std::io::Error> + SetTimeout + SetBaudRate + 'static, D: AsyncDelay>
    SerialInterface for TunnelSerialInterface<P, D>
{
    fn name(&self) -> Option<String> {
        Some("tunnel-net".to_string())
    }

    fn baud_rate(&self) -> Result<u32, SerialPortError> {
        Ok(self.baud_rate)
    }

    async fn set_baud_rate(&mut self, _baud_rate: u32) -> Result<(), SerialPortError> {
        // We're already at max speed. ESP32 bootloader auto-detects the baud rate
        // from the initial SYNC, so espflash's baud rate changes are unnecessary and would
        // break the tunnel. Make this a no-op.
        Ok(())
    }

    fn timeout(&self) -> Duration {
        self.timeout
    }

    fn set_timeout(&mut self, timeout: Duration) -> Result<(), SerialPortError> {
        self.timeout = timeout;
        self.port.set_timeout(timeout).map_err(Self::io_to_serial)?;
        Ok(())
    }

    fn bytes_to_read(&self) -> Result<u32, SerialPortError> {
        Ok(self.buffer.len() as u32)
    }

    async fn read(&mut self, buf: &mut [u8]) -> Result<usize, SerialPortError> {
        // Keep trying until we have data in the buffer
        while self.buffer.is_empty() {
            // First, try to parse any complete TLVs from existing raw data
            self.try_parse_tlvs();

            // If we got data, break out
            if !self.buffer.is_empty() {
                break;
            }

            // Read more raw bytes from the port (this may timeout)
            let n = self.fill_raw_buffer().await.map_err(Self::io_to_serial)?;

            // Try to parse again after reading new data
            self.try_parse_tlvs();

            // If we still have no data and read returned 0, return what we have
            // (even if empty - caller will handle)
            if self.buffer.is_empty() && n == 0 {
                return Ok(0);
            }
        }

        let to_copy = core::cmp::min(self.buffer.len(), buf.len());
        buf[..to_copy].copy_from_slice(&self.buffer[..to_copy]);

        // Drain from front
        let remaining = self.buffer.len() - to_copy;
        for i in 0..remaining {
            self.buffer[i] = self.buffer[i + to_copy];
        }
        self.buffer.truncate(remaining);

        Ok(to_copy)
    }

    async fn write(&mut self, buf: &[u8]) -> Result<usize, SerialPortError> {
        let to_write = core::cmp::min(MAX_VALUE_SIZE, buf.len());
        self.write_net_tlv(&buf[..to_write])
            .await
            .map_err(Self::io_to_serial)?;
        Ok(to_write)
    }

    async fn write_all(&mut self, buf: &[u8]) -> Result<(), SerialPortError> {
        let mut written = 0;
        while written < buf.len() {
            let chunk_size = core::cmp::min(MAX_VALUE_SIZE, buf.len() - written);
            self.write_net_tlv(&buf[written..written + chunk_size])
                .await
                .map_err(Self::io_to_serial)?;
            written += chunk_size;
        }
        Ok(())
    }

    async fn flush(&mut self) -> Result<(), SerialPortError> {
        self.port.flush().await.map_err(Self::io_to_serial)
    }

    async fn clear(&mut self, _buffer_to_clear: ClearBufferType) -> Result<(), SerialPortError> {
        // Clear both the raw buffer and the parsed buffer
        self.raw_buffer.clear();
        self.buffer.clear();
        // Also clear the underlying port's buffer to discard any pending data
        self.port.clear_buffer();
        Ok(())
    }

    async fn write_data_terminal_ready(&mut self, level: bool) -> Result<(), SerialPortError> {
        use crate::shared::{Pin, PinValue};
        // DTR HIGH → BOOT LOW (bootloader mode), DTR LOW → BOOT HIGH (normal)
        // Note: Don't wait for Ack - just send the command (matches legacy behavior)
        let boot = if level { PinValue::Low } else { PinValue::High };
        self.write_mgmt_command(CtlToMgmt::SetPin, &[Pin::NetBoot as u8, boot as u8])
            .await
            .map_err(Self::io_to_serial)
    }

    async fn write_request_to_send(&mut self, level: bool) -> Result<(), SerialPortError> {
        use crate::shared::{Pin, PinValue};
        // RTS HIGH → RST LOW (chip in reset), RTS LOW → RST HIGH (chip running)
        // Note: Don't wait for Ack - just send the command (matches legacy behavior)
        let rst = if level { PinValue::Low } else { PinValue::High };
        self.write_mgmt_command(CtlToMgmt::SetPin, &[Pin::NetRst as u8, rst as u8])
            .await
            .map_err(Self::io_to_serial)
    }

    async fn delay_ms(&mut self, ms: u32) {
        self.delay.delay_ms(ms).await;
    }
}

// ============================================================================
// Flashing implementation for CtlCore
// ============================================================================

/// Result of attempting to enter MGMT bootloader mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MgmtBootloaderEntry {
    /// Successfully entered bootloader via DTR/RTS reset (EV16).
    AutoReset,
    /// Bootloader was already active (user pre-reset the device).
    AlreadyActive,
    /// Could not detect bootloader - manual intervention required.
    NotDetected,
}

/// Flashing methods for CtlCore.
///
/// These methods require the port to implement `CtlPort<Error = std::io::Error>`.
/// All I/O is performed through the async `CtlPort` trait.
impl<P: CtlPort<Error = std::io::Error>> CtlCore<P> {
    /// Read the MGMT chip's DATA0 option byte.
    pub async fn get_mgmt_data0_option_byte(
        &mut self,
        skip_init: bool,
    ) -> Result<u8, stm::Error<P::Error>> {
        if !skip_init {
            self.drain();
        }

        let mut bl = Bootloader::new(self.port_mut());
        if !skip_init {
            bl.init().await?;
        }

        let mut data = [0u8; 4];
        bl.read_memory(F072_DATA0_BLOCK_ADDR, &mut data).await?;
        Ok(data[0])
    }

    /// Write the MGMT chip's DATA0 option byte and update its complement.
    pub async fn set_mgmt_data0_option_byte(
        &mut self,
        skip_init: bool,
        value: u8,
    ) -> Result<(), stm::Error<P::Error>> {
        if !skip_init {
            self.drain();
        }

        let mut bl = Bootloader::new(self.port_mut());
        if !skip_init {
            bl.init().await?;
        }

        let mut data = [0u8; 4];
        bl.read_memory(F072_DATA0_BLOCK_ADDR, &mut data).await?;
        data[0] = value;
        data[1] = !value;
        bl.write_memory(F072_DATA0_BLOCK_ADDR, &data).await?;

        let mut verify = [0u8; 4];
        bl.read_memory(F072_DATA0_BLOCK_ADDR, &mut verify).await?;
        if verify != data {
            return Err(stm::Error::Io(IoError::new(
                ErrorKind::InvalidData,
                "option byte verification failed",
            )));
        }

        Ok(())
    }

    /// Attempt to enter MGMT bootloader mode automatically.
    ///
    /// This implements Strategy 1 for EV15/EV16 detection:
    /// 1. Send DTR/RTS reset sequence (works on EV16, harmless no-op on EV15)
    /// 2. Wait for bootloader to start
    /// 3. Send 0x7F init byte and wait for ACK with short timeout
    /// 4. If ACK received, bootloader is ready
    /// 5. If no ACK, return `NotDetected` for manual fallback
    ///
    /// On EV16:
    /// - RTS high sets BOOT0 high (bootloader mode)
    /// - DTR pulse (high→low) triggers reset
    ///
    /// On EV15 (or if DTR/RTS not connected):
    /// - DTR/RTS commands are ignored
    /// - If user already reset to bootloader manually, we'll detect it
    ///
    /// The `delay_ms` callback should return a future that sleeps for the given
    /// number of milliseconds. Use `tokio::time::sleep` for native or `js_sleep`
    /// for WASM.
    pub async fn try_enter_mgmt_bootloader<D, F>(&mut self, delay_ms: D) -> MgmtBootloaderEntry
    where
        D: Fn(u64) -> F,
        F: core::future::Future<Output = ()>,
        P: crate::ctl::SetBaudRate,
    {
        // Set to bootloader baud rate
        let _ = self
            .port_mut()
            .set_baud_rate(uart_config::STM32_BOOTLOADER.baudrate)
            .await;

        self.drain();

        // Establish known starting state (both signals low)
        let _ = self.port_mut().write_dtr(false).await;
        let _ = self.port_mut().write_rts(false).await;
        delay_ms(100).await;

        // BOOT0 high, then pulse reset (RTS=true, DTR high→low)
        let _ = self.port_mut().write_rts(true).await;
        delay_ms(50).await;
        let _ = self.port_mut().write_dtr(true).await;
        delay_ms(50).await;
        let _ = self.port_mut().write_dtr(false).await;

        self.drain();

        // Poll for bootloader ready (up to 2 seconds)
        // Track elapsed time via delay count instead of std::time::Instant,
        // which is not available on wasm32-unknown-unknown.
        use crate::timing::bootloader::{MAX_WAIT_MS, PROBE_RETRY_INTERVAL_MS};
        let mut elapsed_ms: u64 = 0;
        loop {
            // Probe for bootloader. This consumes the init byte (0x7F → ACK),
            // so callers should pass skip_init=true to flash_mgmt/get_mgmt_bootloader_info.
            if self.probe_mgmt_bootloader().await {
                return MgmtBootloaderEntry::AutoReset;
            }

            if elapsed_ms >= MAX_WAIT_MS {
                return MgmtBootloaderEntry::NotDetected;
            }

            delay_ms(PROBE_RETRY_INTERVAL_MS).await;
            elapsed_ms += PROBE_RETRY_INTERVAL_MS;
        }
    }

    /// Probe if MGMT bootloader is currently active.
    ///
    /// Sends 0x7F init byte and waits briefly for ACK (0x79).
    /// Returns `true` if bootloader responds, `false` otherwise.
    ///
    /// Note: This has a short timeout (~200ms) to avoid blocking.
    async fn probe_mgmt_bootloader(&mut self) -> bool {
        // Drain any stale bytes from the serial buffer (OS buffer, WebSerial
        // stream, etc.) so they don't get mistaken for a bootloader ACK.
        self.port_mut().drain_port().await;

        // Send 0x7F init byte (STM32 bootloader auto-baud detection)
        let init_byte = [0x7F];
        if self.port_mut().write_all(&init_byte).await.is_err() {
            return false;
        }
        if self.port_mut().flush().await.is_err() {
            return false;
        }

        // Wait for ACK (0x79) - the read will timeout if no bootloader
        // The timeout is controlled by the port's configured timeout
        let mut response = [0u8; 1];
        match self.port_mut().read_exact(&mut response).await {
            Ok(()) => response[0] == 0x79,
            Err(_) => false,
        }
    }

    /// Exit the MGMT bootloader and return to user code.
    ///
    /// Issues the STM32 Go command to jump to the application, then does a
    /// clean hardware reset via DTR/RTS (BOOT0 low + NRST pulse) if available.
    /// The hardware reset ensures peripherals are properly reinitialized.
    /// On EV15 (no DTR/RTS), only the Go command is used.
    pub async fn exit_mgmt_bootloader<D, F>(&mut self, delay_ms: D)
    where
        D: Fn(u64) -> F,
        F: core::future::Future<Output = ()>,
    {
        let mut bl = Bootloader::new(self.port_mut());
        let _ = bl.go(0x0800_0000).await;

        // Release BOOT0 and do a clean hardware reset so peripherals
        // are properly reinitialized (Go alone doesn't reset them).
        let _ = self.port_mut().write_rts(false).await;
        delay_ms(50).await;
        let _ = self.port_mut().write_dtr(true).await;
        delay_ms(50).await;
        let _ = self.port_mut().write_dtr(false).await;
    }

    /// Get MGMT bootloader information.
    ///
    /// This assumes the MGMT chip is already in bootloader mode and the serial
    /// connection is configured correctly (even parity, 115200 baud).
    ///
    /// Set `skip_init` to `true` if the bootloader has already been initialized
    /// (e.g., by `try_enter_mgmt_bootloader` which probes with 0x7F).
    pub async fn get_mgmt_bootloader_info(
        &mut self,
        skip_init: bool,
    ) -> Result<MgmtBootloaderInfo, stm::Error<P::Error>> {
        if !skip_init {
            self.drain();
        }

        let mut bl = Bootloader::new(self.port_mut());

        // Initialize communication (sends 0x7F for auto-baud detection)
        if !skip_init {
            bl.init().await?;
        }

        // Get bootloader info
        let info = bl.get().await?;

        // Get chip ID
        let chip_id = bl.get_id().await?;

        // Try to read a small amount of memory from the start of flash
        let mut flash_sample = [0u8; 32];
        let flash_result = bl.read_memory(0x0800_0000, &mut flash_sample).await;
        let flash_sample = if flash_result.is_ok() {
            Some(flash_sample)
        } else {
            None // Read protection may be enabled
        };

        // Reset MGMT chip back to normal operation
        bl.go(0x0800_0000).await?;

        Ok(MgmtBootloaderInfo {
            bootloader_version: info.version,
            chip_id,
            commands: info.commands,
            command_count: info.command_count,
            flash_sample,
        })
    }

    /// Flash firmware to the MGMT chip (STM32F072CB).
    ///
    /// This assumes the MGMT chip is already in bootloader mode.
    /// The progress callback is called with (phase, bytes_processed, total_bytes).
    ///
    /// Set `skip_init` to `true` if the bootloader has already been initialized
    /// (e.g., by `try_enter_mgmt_bootloader` which probes with 0x7F).
    pub async fn flash_mgmt<F, D, Fut>(
        &mut self,
        firmware: &[u8],
        skip_init: bool,
        mut progress: F,
        delay_ms: D,
    ) -> Result<(), FlashError<P::Error>>
    where
        F: FnMut(FlashPhase, usize, usize),
        D: Fn(u64) -> Fut,
        Fut: core::future::Future<Output = ()>,
        P: crate::ctl::SetTimeout + crate::ctl::SetBaudRate,
    {
        // Switch to bootloader baud rate BEFORE any bootloader interaction
        let _ = self
            .port_mut()
            .set_baud_rate(uart_config::STM32_BOOTLOADER.baudrate)
            .await;

        // Perform the flash operation, capturing the result
        let result = async {
            if !skip_init {
                self.drain();
            }

            let mut bl = Bootloader::new(self.port_mut());

            // Initialize communication
            if !skip_init {
                bl.init().await?;
            }

            // Erase pages needed for firmware (STM32F072CB has 2KB pages)
            let pages_needed = (firmware.len() + PAGE_SIZE - 1) / PAGE_SIZE;
            let pages_needed = pages_needed.max(1);

            for page in 0..pages_needed {
                progress(FlashPhase::Erasing, page, pages_needed);
                let page_num = page as u16;
                if bl.extended_erase(Some(&[page_num]), None).await.is_err() {
                    bl.erase(Some(&[page as u8])).await?;
                }
            }
            progress(FlashPhase::Erasing, pages_needed, pages_needed);

            // Write firmware in 256-byte chunks
            let total = firmware.len();
            let mut written = 0;
            let base_address: u32 = F072_FLASH_BASE;

            for chunk in firmware.chunks(F072_WRITE_CHUNK_SIZE) {
                let address = base_address + written as u32;
                bl.write_memory(address, chunk).await?;
                written += chunk.len();
                progress(FlashPhase::Writing, written, total);
            }

            // Verify by reading back
            let mut verified = 0;
            let mut read_buf = [0u8; F072_WRITE_CHUNK_SIZE];

            for chunk in firmware.chunks(F072_WRITE_CHUNK_SIZE) {
                let address = base_address + verified as u32;
                let len = bl
                    .read_memory(address, &mut read_buf[..chunk.len()])
                    .await?;
                if &read_buf[..len] != chunk {
                    return Err(FlashError::VerifyFailed {
                        address,
                        expected: heapless::Vec::from_slice(chunk).unwrap(),
                        actual: heapless::Vec::from_slice(&read_buf[..len]).unwrap(),
                    });
                }
                verified += len;
                progress(FlashPhase::Verifying, verified, total);
            }

            // Jump to new firmware
            bl.go(0x0800_0000).await?;

            // Wait for MGMT to come back online
            // Try hello() every 100ms, up to 50 attempts (5 seconds total)
            // This helps avoid the need for retries in UI flashing
            drop(bl); // Drop bootloader to release port reference
            let _ = self.wait_for_mgmt_ready(50).await;

            Ok(())
        }
        .await;

        // Always restore CTL-MGMT UART to normal operation baud rate (1000000)
        let _ = self
            .port_mut()
            .set_baud_rate(uart_config::HIGH_SPEED.baudrate)
            .await;

        // On success, do a hardware reset to ensure peripherals are properly initialized
        if result.is_ok() {
            // Release BOOT0 and do a clean hardware reset
            let _ = self.port_mut().write_rts(false).await;
            delay_ms(50).await;
            let _ = self.port_mut().write_dtr(true).await;
            delay_ms(50).await;
            let _ = self.port_mut().write_dtr(false).await;

            // Wait for MGMT firmware to come online and be ready for commands
            let _ = self.wait_for_mgmt_ready(50).await;
        }

        result
    }

    /// Get UI bootloader information.
    ///
    /// This resets the UI chip into bootloader mode, queries information,
    /// and resets it back to user mode.
    ///
    /// The `delay_ms` callback should sleep for the given number of milliseconds.
    ///
    /// Note: If MGMT was recently flashed, disconnect/reconnect the serial port
    /// to clear all buffers and allow MGMT firmware to fully boot before calling
    /// this method.
    pub async fn get_ui_bootloader_info<D, F>(
        &mut self,
        delay_ms: D,
    ) -> Result<MgmtBootloaderInfo, stm::Error<std::io::Error>>
    where
        D: Fn(u64) -> F,
        F: core::future::Future<Output = ()>,
    {
        // Switch MGMT-UI UART to bootloader baud rate (115200)
        let _ = self
            .set_ui_baud_rate(uart_config::STM32_BOOTLOADER.baudrate)
            .await;

        // Drain any stale data from buffers
        self.drain();

        // Reset UI chip into bootloader mode
        let _ = self.reset_ui_to_bootloader(&delay_ms).await;

        // Wait for bootloader to start before attempting init.
        delay_ms(crate::timing::reset::STM32_INITIAL_STABILIZATION_MS).await;

        // Query bootloader info (init + get + get_id)
        let result = self.query_ui_bootloader().await;

        // Always reset UI chip back to user mode
        let _ = self.reset_ui_to_user(&delay_ms).await;

        // Switch MGMT-UI UART back to normal operation baud rate
        let _ = self
            .set_ui_baud_rate(uart_config::HIGH_SPEED.baudrate)
            .await;

        result
    }

    /// Query the UI bootloader when it's already in bootloader mode.
    ///
    /// This is useful for platforms like WASM where you need to handle the
    /// reset and delay asynchronously before calling this method.
    pub async fn query_ui_bootloader(
        &mut self,
    ) -> Result<MgmtBootloaderInfo, stm::Error<std::io::Error>> {
        let ui_tunnel = TunnelPort::new(self.port_mut());
        let mut bl = Bootloader::new(ui_tunnel);

        // Initialize communication
        bl.init().await?;

        // Get bootloader info
        let info = bl.get().await?;

        // Get chip ID
        let chip_id = bl.get_id().await?;

        // Try to read flash sample
        let mut flash_sample = [0u8; 32];
        let flash_sample = match bl.read_memory(0x0800_0000, &mut flash_sample).await {
            Ok(_) => Some(flash_sample),
            Err(_) => None,
        };

        Ok(MgmtBootloaderInfo {
            bootloader_version: info.version,
            chip_id,
            commands: info.commands,
            command_count: info.command_count,
            flash_sample,
        })
    }

    /// Flash firmware to the UI chip (STM32F405RG).
    ///
    /// This method:
    /// 1. Resets the UI chip into bootloader mode
    /// 2. Erases the required sectors
    /// 3. Writes the firmware in 256-byte chunks
    /// 4. Optionally verifies by reading back
    /// 5. Resets the UI chip back to user mode
    ///
    /// The `delay_ms` callback should sleep for the given number of milliseconds.
    /// The progress callback is called with (phase, bytes_processed, total_bytes).
    ///
    /// Note: If MGMT was recently flashed, disconnect/reconnect the serial port
    /// to clear all buffers and allow MGMT firmware to fully boot before calling
    /// this method. This ensures reliable UI tunneling through MGMT.
    pub async fn flash_ui<Cb, D, Fut>(
        &mut self,
        firmware: &[u8],
        delay_ms: D,
        verify: bool,
        mut progress: Cb,
    ) -> Result<(), FlashError<std::io::Error>>
    where
        Cb: FnMut(FlashPhase, usize, usize),
        D: Fn(u64) -> Fut,
        Fut: core::future::Future<Output = ()>,
    {
        // Switch MGMT-UI UART to bootloader baud rate (115200)
        let _ = self
            .set_ui_baud_rate(uart_config::STM32_BOOTLOADER.baudrate)
            .await;

        // Drain any stale data from buffers to prevent contamination
        // when tunneling UI bootloader commands through MGMT.
        self.drain();

        // Hold NET chip in reset during UI flashing to avoid interference
        let _ = self.hold_net_reset().await;

        // Reset UI chip into bootloader mode
        let _ = self.reset_ui_to_bootloader(&delay_ms).await;

        // Wait for bootloader to start before attempting init.
        // The STM32 bootloader's 0x7F auto-baud init must only be sent once —
        // a second 0x7F is interpreted as a command byte, corrupting state.
        delay_ms(crate::timing::reset::STM32_INITIAL_STABILIZATION_MS).await;

        // Flash the firmware (init + erase + write + verify)
        let result = self
            .flash_ui_in_bootloader_mode(firmware, verify, &mut progress)
            .await;

        // Always reset UI chip back to user mode
        let _ = self.reset_ui_to_user(&delay_ms).await;

        // Release NET chip from reset
        let _ = self.reset_net_to_user(&delay_ms).await;

        // Switch MGMT-UI UART back to normal operation baud rate (1000000)
        let _ = self
            .set_ui_baud_rate(uart_config::HIGH_SPEED.baudrate)
            .await;

        result
    }

    /// Flash the UI chip when it's already in bootloader mode.
    ///
    /// This is useful for platforms like WASM where you need to handle the
    /// reset and delay asynchronously before calling this method.
    ///
    /// Typical usage:
    /// 1. Call `reset_ui_to_bootloader(delay_ms)`
    /// 2. Wait for bootloader to be ready (e.g., 100ms)
    /// 3. Call this method
    /// 4. Call `reset_ui_to_user(delay_ms)`
    pub async fn flash_ui_in_bootloader_mode<F>(
        &mut self,
        firmware: &[u8],
        verify: bool,
        progress: &mut F,
    ) -> Result<(), FlashError<std::io::Error>>
    where
        F: FnMut(FlashPhase, usize, usize),
    {
        // Drain any stale data from the serial stream before starting.
        // UART glitches during the UI chip reset and baud rate mismatch between
        // MGMT and UI can leave stale FromUi TLVs in the stream that would
        // contaminate the bootloader protocol (shifting ACK/data alignment).
        self.port_mut().drain_port().await;

        let total = firmware.len();
        let base_address: u32 = FLASH_BASE;
        let sectors_needed = sectors_for_size_f405(firmware.len());

        // Init + Erase + Write phase
        {
            let ui_tunnel = TunnelPort::new(self.port_mut());
            let mut bl = Bootloader::new(ui_tunnel);

            // Initialize communication
            bl.init().await?;

            // Erase sectors needed for firmware (STM32F405RG has variable sector sizes)
            for sector in 0..sectors_needed {
                progress(FlashPhase::Erasing, sector, sectors_needed);
                bl.extended_erase(Some(&[sector as u16]), None).await?;
            }
            progress(FlashPhase::Erasing, sectors_needed, sectors_needed);

            // Write firmware in 256-byte chunks
            let mut written = 0;
            for chunk in firmware.chunks(F405_WRITE_CHUNK_SIZE) {
                let address = base_address + written as u32;
                bl.write_memory(address, chunk).await?;
                written += chunk.len();
                progress(FlashPhase::Writing, written, total);
            }
        }

        // Verify by reading back (optional)
        if verify {
            // Drain between write and verify to clear any accumulated stale data.
            // During the write phase, ACK-only responses are self-correcting if
            // shifted, but verification reads 256 data bytes per chunk where any
            // alignment shift would cause mismatches.
            self.port_mut().drain_port().await;

            let ui_tunnel = TunnelPort::new(self.port_mut());
            let mut bl = Bootloader::new(ui_tunnel);

            let mut verified = 0;
            let mut read_buf = [0u8; F405_WRITE_CHUNK_SIZE];

            for chunk in firmware.chunks(F405_WRITE_CHUNK_SIZE) {
                let address = base_address + verified as u32;
                let len = bl
                    .read_memory(address, &mut read_buf[..chunk.len()])
                    .await?;
                if &read_buf[..len] != chunk {
                    return Err(FlashError::VerifyFailed {
                        address,
                        expected: heapless::Vec::from_slice(chunk).unwrap(),
                        actual: heapless::Vec::from_slice(&read_buf[..len]).unwrap(),
                    });
                }
                verified += len;
                progress(FlashPhase::Verifying, verified, total);
            }
        }

        Ok(())
    }
}

// ============================================================================
// NET chip (ESP32) flashing implementation
// ============================================================================

use espflash::connection::{Connection, PortInfo, ResetAfterOperation, ResetBeforeOperation};
use espflash::flasher::{FlashData, FlashSettings, Flasher};
use espflash::image_format::idf::IdfBootloaderFormat;
use espflash::target::{Chip, ProgressCallbacks};

/// Errors that can occur during ESP32 flash operations.
#[derive(Debug)]
pub enum EspflashError {
    /// I/O error
    Io(std::io::Error),
    /// Bootloader communication timed out
    BootloaderTimeout,
    /// espflash error
    Espflash(String),
}

impl core::fmt::Display for EspflashError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            EspflashError::Io(e) => write!(f, "I/O error: {}", e),
            EspflashError::BootloaderTimeout => write!(f, "Bootloader timeout"),
            EspflashError::Espflash(msg) => write!(f, "espflash error: {}", msg),
        }
    }
}

impl From<std::io::Error> for EspflashError {
    fn from(e: std::io::Error) -> Self {
        EspflashError::Io(e)
    }
}

/// Device information returned by espflash.
#[derive(Debug)]
pub struct EspflashDeviceInfo {
    /// Core device info (chip type, flash size, etc).
    pub device_info: espflash::flasher::DeviceInfo,
    /// Security info (secure boot, flash encryption status).
    pub security_info: espflash::flasher::SecurityInfo,
}

/// NET chip (ESP32) flashing methods for CtlCore.
///
/// These methods require the port to implement:
/// - `CtlPort<Error = std::io::Error>` (for async TLV protocol)
/// - `SetTimeout + SetBaudRate + 'static` (for espflash SerialInterface)
impl<P> CtlCore<P>
where
    P: CtlPort<Error = std::io::Error> + SetTimeout + SetBaudRate + 'static,
{
    /// Flash firmware to the NET chip (ESP32).
    ///
    /// The `elf_data` parameter should be an ELF file - espflash converts it
    /// to IDF bootloader format. Progress is reported via the `ProgressCallbacks` trait.
    ///
    /// If `partition_table` is provided, it should be the bytes of a CSV or binary
    /// partition table. Otherwise, the default partition table is used.
    ///
    /// The `delay` parameter provides platform-appropriate async delays
    /// (e.g., `StdDelay` for native, a JS-based delay for WASM).
    ///
    /// This method automatically holds the UI chip in reset during flashing
    /// to avoid interference, and releases it afterward.
    pub async fn flash_net<D: AsyncDelay + Clone>(
        &mut self,
        elf_data: &[u8],
        partition_table: Option<&[u8]>,
        progress: &mut dyn ProgressCallbacks,
        delay: D,
        max_baud: u32,
    ) -> Result<(), EspflashError> {
        // Clone delay for reset operations after the original is consumed by TunnelSerialInterface
        let reset_delay = delay.clone();
        let delay_fn = move |ms: u64| {
            let d = reset_delay.clone();
            async move { d.delay_ms(ms as u32).await }
        };

        // Hold UI chip in reset during NET flashing to avoid interference
        // (done before taking the port)
        let _ = self.hold_ui_reset().await;

        self.drain();

        // Take the port out of CtlCore (CTL-MGMT stays at 1000000)
        let port = self.take_port();

        // MGMT-NET is already at max_baud from boot.
        // ESP32 bootloader will auto-detect from the SYNC command.
        let serial_interface = TunnelSerialInterface::new(port, max_baud, delay);

        let port_info = PortInfo {
            vid: 0x303A,
            pid: 0x1002,
            serial_number: Some("MGMT_BRIDGE".to_string()),
            manufacturer: Some("Link".to_string()),
            product: Some("MGMT Bridge".to_string()),
        };

        let connection = Connection::new(
            serial_interface,
            port_info,
            ResetAfterOperation::HardReset,
            ResetBeforeOperation::DefaultReset,
            max_baud,
        );

        // Connect to ESP32 bootloader at max_baud
        // use_stub=false uses ROM bootloader (stub upload fails through tunnel)
        let mut flasher = match Flasher::connect(connection, false, false, true, None, None).await {
            Ok(f) => f,
            Err((connection, e)) => {
                // Recover port from the returned connection
                self.recover_port_from_connection(connection).await;
                let _ = self.reset_ui_to_user(&delay_fn).await;
                return Err(EspflashError::Espflash(format!("connect: {:?}", e)));
            }
        };

        // Get device info for flash settings
        let info = match flasher.device_info().await {
            Ok(info) => info,
            Err(e) => {
                self.recover_port_from_connection(flasher.into_connection())
                    .await;
                let _ = self.reset_ui_to_user(&delay_fn).await;
                return Err(EspflashError::Espflash(format!(
                    "device_info (connect succeeded, now at {} baud): {:?}",
                    max_baud, e
                )));
            }
        };
        let chip = flasher.chip();

        let flash_settings = FlashSettings::new(None, Some(info.flash_size), None);
        let flash_data = FlashData::new(flash_settings, 0, None, chip, info.crystal_frequency);

        let image_format = match IdfBootloaderFormat::new(
            elf_data,
            &flash_data,
            partition_table,
            None,
            None,
            None,
        ) {
            Ok(fmt) => fmt,
            Err(e) => {
                self.recover_port_from_connection(flasher.into_connection())
                    .await;
                let _ = self.reset_ui_to_user(&delay_fn).await;
                return Err(EspflashError::Espflash(format!(
                    "IdfBootloaderFormat: {:?}",
                    e
                )));
            }
        };

        if let Err(e) = flasher
            .load_image_to_flash(progress, image_format.into())
            .await
        {
            self.recover_port_from_connection(flasher.into_connection())
                .await;
            let _ = self.reset_ui_to_user(&delay_fn).await;
            return Err(EspflashError::Espflash(format!(
                "load_image_to_flash: {:?}",
                e
            )));
        }

        // load_image_to_flash already calls target.finish(connection, true) which
        // resets the chip via reset_after_flash. No need to reset again here.

        // Recover port and release UI
        self.recover_port_from_connection(flasher.into_connection())
            .await;
        let _ = self.reset_ui_to_user(&delay_fn).await;

        Ok(())
    }

    /// Helper to recover the port from an espflash Connection and return it to CtlCore.
    async fn recover_port_from_connection<D: AsyncDelay>(
        &mut self,
        connection: Connection<TunnelSerialInterface<P, D>>,
    ) {
        let port = connection.into_serial().into_port();
        self.put_port(port);
    }

    /// Get NET chip bootloader info.
    ///
    /// Returns detailed device information including chip type, revision,
    /// flash size, features, MAC address, and security info.
    ///
    /// The `delay` parameter provides platform-appropriate async delays
    /// (e.g., `StdDelay` for native, a JS-based delay for WASM).
    pub async fn get_net_bootloader_info<D: AsyncDelay>(
        &mut self,
        delay: D,
    ) -> Result<EspflashDeviceInfo, EspflashError> {
        self.drain();

        // Take the port out of CtlCore
        let port = self.take_port();
        let serial_interface =
            TunnelSerialInterface::new(port, uart_config::STM32_BOOTLOADER.baudrate, delay);

        let port_info = PortInfo {
            vid: 0,
            pid: 0,
            serial_number: None,
            manufacturer: None,
            product: None,
        };

        let connection = Connection::new(
            serial_interface,
            port_info,
            ResetAfterOperation::NoReset,
            ResetBeforeOperation::DefaultReset,
            uart_config::STM32_BOOTLOADER.baudrate,
        );

        let mut flasher = match Flasher::connect(
            connection,
            false,
            false,
            false,
            Some(Chip::Esp32s3),
            None,
        )
        .await
        {
            Ok(f) => f,
            Err((connection, e)) => {
                self.recover_port_from_connection(connection).await;
                return Err(EspflashError::Espflash(format!("{:?}", e)));
            }
        };

        let device_info = match flasher.device_info().await {
            Ok(info) => info,
            Err(e) => {
                self.recover_port_from_connection(flasher.into_connection())
                    .await;
                return Err(EspflashError::Espflash(format!("device_info: {:?}", e)));
            }
        };

        let security_info = match flasher.security_info().await {
            Ok(info) => info,
            Err(e) => {
                self.recover_port_from_connection(flasher.into_connection())
                    .await;
                return Err(EspflashError::Espflash(format!("security_info: {:?}", e)));
            }
        };

        // Get the port back and return it to CtlCore
        self.recover_port_from_connection(flasher.into_connection())
            .await;

        Ok(EspflashDeviceInfo {
            device_info,
            security_info,
        })
    }

    /// Erase the NET chip's entire flash.
    ///
    /// The `delay` parameter provides platform-appropriate async delays
    /// (e.g., `StdDelay` for native, a JS-based delay for WASM).
    pub async fn erase_net<D: AsyncDelay>(&mut self, delay: D) -> Result<(), EspflashError> {
        self.drain();

        // Take the port out of CtlCore
        let port = self.take_port();
        let serial_interface =
            TunnelSerialInterface::new(port, uart_config::STM32_BOOTLOADER.baudrate, delay);

        let port_info = PortInfo {
            vid: 0x303A,
            pid: 0x1002,
            serial_number: Some("MGMT_BRIDGE".to_string()),
            manufacturer: Some("Link".to_string()),
            product: Some("MGMT Bridge".to_string()),
        };

        let connection = Connection::new(
            serial_interface,
            port_info,
            ResetAfterOperation::HardReset,
            ResetBeforeOperation::DefaultReset,
            115_200,
        );

        let mut flasher =
            match Flasher::connect(connection, false, false, true, Some(Chip::Esp32s3), None).await
            {
                Ok(f) => f,
                Err((connection, e)) => {
                    self.recover_port_from_connection(connection).await;
                    return Err(EspflashError::Espflash(format!("{:?}", e)));
                }
            };

        if let Err(e) = flasher.erase_flash().await {
            self.recover_port_from_connection(flasher.into_connection())
                .await;
            return Err(EspflashError::Espflash(format!("{:?}", e)));
        }

        if let Err(e) = flasher.connection().reset().await {
            self.recover_port_from_connection(flasher.into_connection())
                .await;
            return Err(EspflashError::Espflash(format!("reset: {:?}", e)));
        }

        // Get the port back and return it to CtlCore
        self.recover_port_from_connection(flasher.into_connection())
            .await;

        Ok(())
    }
}

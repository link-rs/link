//! Flashing support for STM32 chips (MGMT and UI) and ESP32 (NET).
//!
//! This module provides async flashing methods for CtlCore. It requires the `ctl` feature.

use super::core::CtlCore;
use super::espflash::connection::{ClearBufferType, SerialInterface, SerialPortError};
use super::port::{CtlPort, SetBaudRate, SetTimeout};
use super::stm::{self, Bootloader};
use crate::shared::{CtlToMgmt, MgmtToCtl, HEADER_SIZE, MAX_VALUE_SIZE, SYNC_WORD};
use std::time::Duration;

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

/// Maximum size for verification error data (matches write chunk size).
const VERIFY_CHUNK_SIZE: usize = 256;

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
    const SECTOR_SIZES: [usize; 12] = [
        16 * 1024,  // Sector 0
        16 * 1024,  // Sector 1
        16 * 1024,  // Sector 2
        16 * 1024,  // Sector 3
        64 * 1024,  // Sector 4
        128 * 1024, // Sector 5
        128 * 1024, // Sector 6
        128 * 1024, // Sector 7
        128 * 1024, // Sector 8
        128 * 1024, // Sector 9
        128 * 1024, // Sector 10
        128 * 1024, // Sector 11
    ];

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
            // Scan for sync word
            let mut matched = 0usize;
            while matched < SYNC_WORD.len() {
                let mut byte = [0u8; 1];
                self.port.read_exact(&mut byte).await?;
                if byte[0] == SYNC_WORD[matched] {
                    matched += 1;
                } else {
                    matched = 0;
                    if byte[0] == SYNC_WORD[0] {
                        matched = 1;
                    }
                }
            }

            // Read header
            let mut header = [0u8; HEADER_SIZE];
            self.port.read_exact(&mut header).await?;

            // Decode header
            let raw_type = u16::from_be_bytes([header[0], header[1]]);
            let length = u32::from_be_bytes([header[2], header[3], header[4], header[5]]) as usize;

            // Read value
            let mut value = heapless::Vec::<u8, MAX_VALUE_SIZE>::new();
            if value.resize(length, 0).is_err() {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "TLV too long",
                ));
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
        let _ = packet.extend_from_slice(&tlv_type.to_be_bytes());
        let _ = packet.extend_from_slice(&(buf.len() as u32).to_be_bytes());
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
                return Err(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "unexpected EOF",
                ));
            }
            filled += n;
        }
        Ok(())
    }
}

// ============================================================================
// Async TunnelSerialInterface for NET flashing through MGMT (espflash)
// ============================================================================

/// Async serial interface for flashing the NET chip (ESP32) through the MGMT tunnel.
///
/// This implements `SerialInterface` for use with espflash. It owns the port `P` directly
/// since espflash's Connection takes ownership. After flashing, use `into_port()` to
/// get the port back. DTR/RTS signals are mapped to BOOT/RST pins.
pub struct TunnelSerialInterface<P> {
    port: P,
    buffer: heapless::Vec<u8, MAX_VALUE_SIZE>,
    timeout: Duration,
    baud_rate: u32,
}

impl<P> TunnelSerialInterface<P> {
    /// Create a new TunnelSerialInterface for NET chip communication.
    pub fn new(port: P, baud_rate: u32) -> Self {
        Self {
            port,
            buffer: heapless::Vec::new(),
            timeout: Duration::from_secs(3),
            baud_rate,
        }
    }

    /// Consume this interface and return the underlying port.
    pub fn into_port(self) -> P {
        self.port
    }
}

impl<P: CtlPort<Error = std::io::Error>> TunnelSerialInterface<P> {
    /// Helper to convert io::Error to SerialPortError
    fn io_to_serial(e: std::io::Error) -> SerialPortError {
        SerialPortError::io(e.to_string())
    }

    /// Read a TLV from the port, filtering for FromNet messages.
    async fn read_net_tlv(&mut self) -> Result<(), std::io::Error> {
        // Scan for sync word
        let mut matched = 0usize;
        while matched < SYNC_WORD.len() {
            let mut byte = [0u8; 1];
            self.port.read_exact(&mut byte).await?;
            if byte[0] == SYNC_WORD[matched] {
                matched += 1;
            } else {
                matched = 0;
                if byte[0] == SYNC_WORD[0] {
                    matched = 1;
                }
            }
        }

        // Read header
        let mut header = [0u8; HEADER_SIZE];
        self.port.read_exact(&mut header).await?;

        // Decode header
        let raw_type = u16::from_be_bytes([header[0], header[1]]);
        let length = u32::from_be_bytes([header[2], header[3], header[4], header[5]]) as usize;

        // Read value
        let mut value = heapless::Vec::<u8, MAX_VALUE_SIZE>::new();
        if value.resize(length, 0).is_err() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "TLV too long",
            ));
        }
        self.port.read_exact(&mut value).await?;

        // Check if it's FromNet
        if let Ok(tlv_type) = MgmtToCtl::try_from(raw_type) {
            if tlv_type == MgmtToCtl::FromNet {
                self.buffer.clear();
                let _ = self.buffer.extend_from_slice(&value);
            }
        }

        Ok(())
    }

    /// Write a ToNet TLV to the port.
    async fn write_net_tlv(&mut self, data: &[u8]) -> Result<(), std::io::Error> {
        let tlv_type: u16 = CtlToMgmt::ToNet.into();

        // Build complete packet to send atomically
        let mut packet = heapless::Vec::<u8, { SYNC_WORD.len() + 2 + 4 + MAX_VALUE_SIZE }>::new();
        let _ = packet.extend_from_slice(&SYNC_WORD);
        let _ = packet.extend_from_slice(&tlv_type.to_be_bytes());
        let _ = packet.extend_from_slice(&(data.len() as u32).to_be_bytes());
        let _ = packet.extend_from_slice(data);

        self.port.write_all(&packet).await
    }

    /// Write a command TLV to MGMT without waiting for Ack.
    async fn write_mgmt_command(&mut self, cmd: CtlToMgmt, value: &[u8]) -> Result<(), std::io::Error> {
        let tlv_type: u16 = cmd.into();

        // Build complete packet
        let mut packet = heapless::Vec::<u8, { SYNC_WORD.len() + 2 + 4 + MAX_VALUE_SIZE }>::new();
        let _ = packet.extend_from_slice(&SYNC_WORD);
        let _ = packet.extend_from_slice(&tlv_type.to_be_bytes());
        let _ = packet.extend_from_slice(&(value.len() as u32).to_be_bytes());
        let _ = packet.extend_from_slice(value);

        self.port.write_all(&packet).await?;
        self.port.flush().await
    }

    /// Send a command TLV to MGMT and wait for Ack.
    async fn send_mgmt_command(&mut self, cmd: CtlToMgmt, value: &[u8]) -> Result<(), std::io::Error> {
        // Write command
        self.write_mgmt_command(cmd, value).await?;

        // Wait for Ack (skip FromNet messages)
        loop {
            // Scan for sync word
            let mut matched = 0usize;
            while matched < SYNC_WORD.len() {
                let mut byte = [0u8; 1];
                self.port.read_exact(&mut byte).await?;
                if byte[0] == SYNC_WORD[matched] {
                    matched += 1;
                } else {
                    matched = 0;
                    if byte[0] == SYNC_WORD[0] {
                        matched = 1;
                    }
                }
            }

            // Read header
            let mut header = [0u8; HEADER_SIZE];
            self.port.read_exact(&mut header).await?;

            let raw_type = u16::from_be_bytes([header[0], header[1]]);
            let length = u32::from_be_bytes([header[2], header[3], header[4], header[5]]) as usize;

            // Read value
            let mut value_buf = heapless::Vec::<u8, MAX_VALUE_SIZE>::new();
            if value_buf.resize(length, 0).is_err() {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "TLV too long",
                ));
            }
            self.port.read_exact(&mut value_buf).await?;

            if let Ok(tlv_type) = MgmtToCtl::try_from(raw_type) {
                match tlv_type {
                    MgmtToCtl::Ack => return Ok(()),
                    MgmtToCtl::FromNet => {
                        // Buffer NET data for later reads
                        self.buffer.clear();
                        let _ = self.buffer.extend_from_slice(&value_buf);
                        continue;
                    }
                    _ => continue,
                }
            }
        }
    }

    /// Change baud rate on both CTL-MGMT and MGMT-NET links.
    pub async fn change_baud_rate(&mut self, baud_rate: u32) -> Result<(), std::io::Error> {
        let baud_bytes = baud_rate.to_le_bytes();

        // Set NET baud rate
        self.send_mgmt_command(CtlToMgmt::SetNetBaudRate, &baud_bytes).await?;

        // Set CTL baud rate (ACK comes at old rate, then MGMT switches)
        self.send_mgmt_command(CtlToMgmt::SetCtlBaudRate, &baud_bytes).await?;

        // Small delay for MGMT to complete the baud rate switch
        // Use async sleep if available, otherwise fall back to thread sleep
        std::thread::sleep(Duration::from_millis(10));

        // Update local baud rate tracking
        self.baud_rate = baud_rate;

        Ok(())
    }
}


impl<P: CtlPort<Error = std::io::Error> + SetTimeout + SetBaudRate + 'static> SerialInterface for TunnelSerialInterface<P> {
    fn name(&self) -> Option<String> {
        Some("tunnel-net".to_string())
    }

    fn baud_rate(&self) -> Result<u32, SerialPortError> {
        Ok(self.baud_rate)
    }

    fn set_baud_rate(&mut self, baud_rate: u32) -> Result<(), SerialPortError> {
        // Change baud rate on both links and local port (sync wrapper for async)
        futures::executor::block_on(self.change_baud_rate(baud_rate)).map_err(Self::io_to_serial)?;
        self.port.set_baud_rate(baud_rate).map_err(Self::io_to_serial)?;
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
        // Return buffered data first
        while self.buffer.is_empty() {
            self.read_net_tlv().await.map_err(Self::io_to_serial)?;
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
        self.write_net_tlv(&buf[..to_write]).await.map_err(Self::io_to_serial)?;
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
        self.buffer.clear();
        Ok(())
    }

    async fn write_data_terminal_ready(&mut self, level: bool) -> Result<(), SerialPortError> {
        // DTR HIGH → BOOT LOW (bootloader mode), DTR LOW → BOOT HIGH (normal)
        // Note: Don't wait for Ack - just send the command (matches legacy behavior)
        let boot = !level;
        self.write_mgmt_command(CtlToMgmt::SetNetBoot, &[boot as u8])
            .await
            .map_err(Self::io_to_serial)
    }

    async fn write_request_to_send(&mut self, level: bool) -> Result<(), SerialPortError> {
        // RTS HIGH → RST LOW (chip in reset), RTS LOW → RST HIGH (chip running)
        // Note: Don't wait for Ack - just send the command (matches legacy behavior)
        let rst = !level;
        self.write_mgmt_command(CtlToMgmt::SetNetRst, &[rst as u8])
            .await
            .map_err(Self::io_to_serial)
    }

    async fn delay_ms(&mut self, ms: u32) {
        std::thread::sleep(Duration::from_millis(ms as u64));
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
    /// The `delay_ms` callback should sleep for the given number of milliseconds.
    pub async fn try_enter_mgmt_bootloader<D>(&mut self, delay_ms: D) -> MgmtBootloaderEntry
    where
        D: Fn(u64),
    {
        // Clear any stale data
        self.drain();

        // Try DTR/RTS reset sequence (EV16)
        // RTS=high sets BOOT0 high (bootloader mode)
        // DTR pulse triggers reset
        let _ = self.port_mut().write_rts(true).await;
        let _ = self.port_mut().write_dtr(true).await;
        let _ = self.port_mut().write_dtr(false).await;

        // Wait for bootloader to initialize (100ms as per hactar-cli)
        delay_ms(100);

        // Clear buffer again after reset
        self.drain();

        // Probe for bootloader with short timeout
        match self.probe_mgmt_bootloader().await {
            true => MgmtBootloaderEntry::AutoReset,
            false => MgmtBootloaderEntry::NotDetected,
        }
    }

    /// Probe if MGMT bootloader is currently active.
    ///
    /// Sends 0x7F init byte and waits briefly for ACK (0x79).
    /// Returns `true` if bootloader responds, `false` otherwise.
    ///
    /// Note: This has a short timeout (~200ms) to avoid blocking.
    async fn probe_mgmt_bootloader(&mut self) -> bool {
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

    /// Reset MGMT chip back to user mode via DTR/RTS (EV16).
    ///
    /// This sets RTS low (BOOT0 low) and pulses DTR to trigger reset.
    /// On EV15 or unsupported ports, this is a no-op.
    pub async fn reset_mgmt_to_user(&mut self) {
        let _ = self.port_mut().write_rts(false).await;
        let _ = self.port_mut().write_dtr(true).await;
        let _ = self.port_mut().write_dtr(false).await;
    }

    /// Get MGMT bootloader information.
    ///
    /// This assumes the MGMT chip is already in bootloader mode and the serial
    /// connection is configured correctly (even parity, 115200 baud).
    pub async fn get_mgmt_bootloader_info(
        &mut self,
    ) -> Result<MgmtBootloaderInfo, stm::Error<P::Error>> {
        // Drain any stale data
        self.drain();

        let mut bl = Bootloader::new(self.port_mut());

        // Initialize communication (sends 0x7F for auto-baud detection)
        bl.init().await?;

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
    pub async fn flash_mgmt<F>(
        &mut self,
        firmware: &[u8],
        mut progress: F,
    ) -> Result<(), FlashError<P::Error>>
    where
        F: FnMut(FlashPhase, usize, usize),
    {
        // Drain any stale data
        self.drain();

        let mut bl = Bootloader::new(self.port_mut());

        // Initialize communication
        bl.init().await?;

        // Erase pages needed for firmware (STM32F072CB has 2KB pages)
        const PAGE_SIZE: usize = 2048;
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
        let base_address: u32 = 0x0800_0000;

        for chunk in firmware.chunks(256) {
            let address = base_address + written as u32;
            bl.write_memory(address, chunk).await?;
            written += chunk.len();
            progress(FlashPhase::Writing, written, total);
        }

        // Verify by reading back
        let mut verified = 0;
        let mut read_buf = [0u8; 256];

        for chunk in firmware.chunks(256) {
            let address = base_address + verified as u32;
            let len = bl.read_memory(address, &mut read_buf[..chunk.len()]).await?;
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

        Ok(())
    }

    /// Get UI bootloader information.
    ///
    /// This resets the UI chip into bootloader mode, queries information,
    /// and resets it back to user mode.
    ///
    /// The `delay_ms` callback should sleep for the given number of milliseconds.
    pub async fn get_ui_bootloader_info<D>(
        &mut self,
        delay_ms: D,
    ) -> Result<MgmtBootloaderInfo, stm::Error<std::io::Error>>
    where
        D: FnOnce(u64),
    {
        // Reset UI chip into bootloader mode
        let _ = self.reset_ui_to_bootloader().await;

        // Wait for bootloader to be ready
        delay_ms(1000);

        // Query bootloader info
        let result = self.query_ui_bootloader().await;

        // Always reset UI chip back to user mode
        let _ = self.reset_ui_to_user().await;

        result
    }

    /// Helper to query the UI bootloader.
    async fn query_ui_bootloader(&mut self) -> Result<MgmtBootloaderInfo, stm::Error<std::io::Error>> {
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
    pub async fn flash_ui<F, D>(
        &mut self,
        firmware: &[u8],
        delay_ms: D,
        verify: bool,
        mut progress: F,
    ) -> Result<(), FlashError<std::io::Error>>
    where
        F: FnMut(FlashPhase, usize, usize),
        D: FnOnce(u64),
    {
        // Reset UI chip into bootloader mode
        let _ = self.reset_ui_to_bootloader().await;

        // Wait for bootloader to be ready
        delay_ms(100);

        // Flash the firmware
        let result = self.do_flash_ui(firmware, verify, &mut progress).await;

        // Always reset UI chip back to user mode
        let _ = self.reset_ui_to_user().await;

        result
    }

    /// Helper to flash the UI chip.
    async fn do_flash_ui<F>(
        &mut self,
        firmware: &[u8],
        verify: bool,
        progress: &mut F,
    ) -> Result<(), FlashError<std::io::Error>>
    where
        F: FnMut(FlashPhase, usize, usize),
    {
        let ui_tunnel = TunnelPort::new(self.port_mut());
        let mut bl = Bootloader::new(ui_tunnel);

        // Initialize communication
        bl.init().await?;

        // Erase sectors needed for firmware (STM32F405RG has variable sector sizes)
        let sectors_needed = sectors_for_size_f405(firmware.len());

        for sector in 0..sectors_needed {
            progress(FlashPhase::Erasing, sector, sectors_needed);
            bl.extended_erase(Some(&[sector as u16]), None).await?;
        }
        progress(FlashPhase::Erasing, sectors_needed, sectors_needed);

        // Write firmware in 256-byte chunks
        let total = firmware.len();
        let mut written = 0;
        let base_address: u32 = 0x0800_0000;

        for chunk in firmware.chunks(256) {
            let address = base_address + written as u32;
            bl.write_memory(address, chunk).await?;
            written += chunk.len();
            progress(FlashPhase::Writing, written, total);
        }

        // Verify by reading back (optional)
        if verify {
            let mut verified = 0;
            let mut read_buf = [0u8; 256];

            for chunk in firmware.chunks(256) {
                let address = base_address + verified as u32;
                let len = bl.read_memory(address, &mut read_buf[..chunk.len()]).await?;
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

use super::espflash::connection::{Connection, PortInfo, ResetAfterOperation, ResetBeforeOperation};
use super::espflash::flasher::{FlashData, FlashSettings, Flasher};
use super::espflash::image_format::idf::IdfBootloaderFormat;
use super::espflash::target::{Chip, ProgressCallbacks};

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
    pub device_info: super::espflash::flasher::DeviceInfo,
    /// Security info (secure boot, flash encryption status).
    pub security_info: super::espflash::flasher::SecurityInfo,
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
    pub async fn flash_net(
        &mut self,
        elf_data: &[u8],
        partition_table: Option<&[u8]>,
        progress: &mut dyn ProgressCallbacks,
    ) -> Result<(), EspflashError> {
        const INITIAL_BAUD: u32 = 115_200;

        self.drain();

        // Take the port out of CtlCore for exclusive use by espflash
        let port = self.take_port();
        let serial_interface = TunnelSerialInterface::new(port, INITIAL_BAUD);

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
            INITIAL_BAUD,
        );

        // Connect to ESP32 bootloader (allow baud rate negotiation up to 460800)
        let flasher_result = Flasher::connect(connection, false, false, true, None, Some(460_800)).await;

        let mut flasher = match flasher_result {
            Ok(f) => f,
            Err(e) => {
                // Get the port back even on error
                // Note: We can't easily recover the port from a failed Connection,
                // so this path may leave the port in a bad state
                return Err(EspflashError::Espflash(format!("{:?}", e)));
            }
        };

        // Get device info for flash settings
        let info = flasher
            .device_info()
            .await
            .map_err(|e| EspflashError::Espflash(format!("device_info: {:?}", e)))?;
        let chip = flasher.chip();

        let flash_settings = FlashSettings::new(None, Some(info.flash_size), None);
        let flash_data = FlashData::new(flash_settings, 0, None, chip, info.crystal_frequency);

        let image_format =
            IdfBootloaderFormat::new(elf_data, &flash_data, partition_table, None, None, None)
                .map_err(|e| EspflashError::Espflash(format!("IdfBootloaderFormat: {:?}", e)))?;

        flasher
            .load_image_to_flash(progress, image_format.into())
            .await
            .map_err(|e| EspflashError::Espflash(format!("{:?}", e)))?;

        flasher
            .connection()
            .reset()
            .await
            .map_err(|e| EspflashError::Espflash(format!("reset: {:?}", e)))?;

        // Get the port back via into_connection().into_serial().into_port()
        let mut tunnel = flasher.into_connection().into_serial();

        // Restore baud rate to initial value before returning port
        tunnel.change_baud_rate(INITIAL_BAUD)
            .await
            .map_err(|e| EspflashError::Espflash(format!("restore baud rate: {:?}", e)))?;

        // Put the port back into CtlCore
        let mut port = tunnel.into_port();
        port.set_baud_rate(INITIAL_BAUD)
            .map_err(|e| EspflashError::Espflash(format!("set local baud rate: {:?}", e)))?;
        self.put_port(port);

        Ok(())
    }

    /// Get NET chip bootloader info.
    ///
    /// Returns detailed device information including chip type, revision,
    /// flash size, features, MAC address, and security info.
    pub async fn get_net_bootloader_info(&mut self) -> Result<EspflashDeviceInfo, EspflashError> {
        self.drain();

        // Take the port out of CtlCore
        let port = self.take_port();
        let serial_interface = TunnelSerialInterface::new(port, 115_200);

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
            115_200,
        );

        let mut flasher =
            Flasher::connect(connection, false, false, false, Some(Chip::Esp32s3), None)
                .await
                .map_err(|e| EspflashError::Espflash(format!("{:?}", e)))?;

        let device_info = flasher
            .device_info()
            .await
            .map_err(|e| EspflashError::Espflash(format!("device_info: {:?}", e)))?;

        let security_info = flasher
            .security_info()
            .await
            .map_err(|e| EspflashError::Espflash(format!("security_info: {:?}", e)))?;

        // Get the port back and return it to CtlCore
        let port = flasher.into_connection().into_serial().into_port();
        self.put_port(port);

        Ok(EspflashDeviceInfo {
            device_info,
            security_info,
        })
    }

    /// Erase the NET chip's entire flash.
    pub async fn erase_net(&mut self) -> Result<(), EspflashError> {
        self.drain();

        // Take the port out of CtlCore
        let port = self.take_port();
        let serial_interface = TunnelSerialInterface::new(port, 115_200);

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
            Flasher::connect(connection, false, false, true, Some(Chip::Esp32s3), None)
                .await
                .map_err(|e| EspflashError::Espflash(format!("{:?}", e)))?;

        flasher
            .erase_flash()
            .await
            .map_err(|e| EspflashError::Espflash(format!("{:?}", e)))?;

        flasher
            .connection()
            .reset()
            .await
            .map_err(|e| EspflashError::Espflash(format!("reset: {:?}", e)))?;

        // Get the port back and return it to CtlCore
        let port = flasher.into_connection().into_serial().into_port();
        self.put_port(port);

        Ok(())
    }
}

//! CTL (Controller) chip - the host computer interface.
//!
//! This module requires `std` for synchronous I/O operations.

extern crate alloc;

pub mod stm;

use crate::shared::{
    CtlToMgmt, MgmtToCtl, MgmtToNet, MgmtToUi, NetToMgmt, Tlv, UiToMgmt, WifiSsid, HEADER_SIZE,
    MAX_VALUE_SIZE, SYNC_WORD,
};
use espflash::connection::{Connection, ResetAfterOperation, ResetBeforeOperation};
use espflash::flasher::{FlashData, FlashSettings, Flasher};
use espflash::image_format::idf::IdfBootloaderFormat;
use espflash::target::Chip;
use serialport::{ClearBuffer, DataBits, FlowControl, Parity, SerialPort, StopBits, UsbPortInfo};
use std::io::{self, Read, Write};
use std::path::Path;
use std::time::Duration;
use stm::Bootloader;

// Re-export espflash types
pub use espflash::flasher::{DeviceInfo, FlashSize, SecurityInfo};
pub use espflash::target::{DefaultProgressCallback, ProgressCallbacks, XtalFrequency};

/// Errors from NET chip flashing.
#[derive(Debug)]
pub enum EspflashError {
    /// I/O error
    Io(io::Error),
    /// Bootloader timeout
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

impl From<io::Error> for EspflashError {
    fn from(e: io::Error) -> Self {
        EspflashError::Io(e)
    }
}

/// Combined device and security information from the NET chip.
#[derive(Debug, Clone)]
pub struct EspflashDeviceInfo {
    /// Device information (chip type, revision, flash size, features, MAC).
    pub device_info: DeviceInfo,
    /// Security information (flags, key purposes, chip ID).
    pub security_info: SecurityInfo,
}

/// Maximum size for verification error data (matches write chunk size).
const VERIFY_CHUNK_SIZE: usize = 256;

/// Results from the WebSocket echo test.
#[derive(Debug, Clone, Default)]
pub struct EchoTestResults {
    /// Number of packets sent.
    pub sent: u8,
    /// Number of packets received (before jitter buffer).
    pub received: u8,
    /// Number of packets output from jitter buffer.
    pub buffered_output: u8,
    /// Number of buffer underruns during the test.
    pub underruns: u8,
    /// Raw inter-arrival times in microseconds (before jitter buffer).
    /// Shows actual network jitter.
    pub raw_jitter_us: heapless::Vec<u32, 50>,
    /// Buffered inter-departure times in microseconds (after jitter buffer).
    /// Should be close to 20000us (20ms) if buffer is working.
    pub buffered_jitter_us: heapless::Vec<u32, 50>,
}

/// Results from the WebSocket speed test.
#[derive(Debug, Clone, Default)]
pub struct SpeedTestResults {
    /// Number of packets sent.
    pub sent: u8,
    /// Number of packets received.
    pub received: u8,
    /// Time to send all packets in milliseconds.
    pub send_time_ms: u32,
    /// Time to receive all responses in milliseconds.
    pub recv_time_ms: u32,
}

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
// Sync TLV Read/Write
// ============================================================================

/// Error type for TLV read operations.
#[derive(Debug)]
pub enum TlvReadError<E> {
    Io(E),
    InvalidType,
    TooLong,
}

/// Read a TLV packet from a sync reader.
/// Scans for sync word, then reads header and value.
fn read_tlv<T, R>(reader: &mut R) -> Result<Option<Tlv<T>>, TlvReadError<std::io::Error>>
where
    T: TryFrom<u16>,
    R: Read,
{
    // Scan for sync word, draining any garbage
    let mut matched = 0usize;
    while matched < SYNC_WORD.len() {
        let mut byte = [0u8; 1];
        match reader.read_exact(&mut byte) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
            Err(e) => return Err(TlvReadError::Io(e)),
        }

        if byte[0] == SYNC_WORD[matched] {
            matched += 1;
        } else {
            matched = 0;
            if byte[0] == SYNC_WORD[0] {
                matched = 1;
            }
        }
    }

    // Sync word found, now read the header
    let mut header = [0u8; HEADER_SIZE];
    match reader.read_exact(&mut header) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(TlvReadError::Io(e)),
    }

    // Decode header
    let raw_type = u16::from_be_bytes([header[0], header[1]]);
    let Ok(tlv_type) = T::try_from(raw_type) else {
        return Err(TlvReadError::InvalidType);
    };
    let length = u32::from_be_bytes([header[2], header[3], header[4], header[5]]) as usize;

    let mut value = heapless::Vec::<u8, MAX_VALUE_SIZE>::new();
    if value.resize(length, 0).is_err() {
        return Err(TlvReadError::TooLong);
    }

    match reader.read_exact(&mut value) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(TlvReadError::Io(e)),
    }

    Ok(Some(Tlv { tlv_type, value }))
}

/// Write a TLV packet to a sync writer.
fn write_tlv<T, W>(writer: &mut W, tlv_type: T, value: &[u8]) -> std::io::Result<()>
where
    T: Into<u16>,
    W: Write,
{
    let type_val: u16 = tlv_type.into();
    writer.write_all(&SYNC_WORD)?;

    let mut header = [0u8; HEADER_SIZE];
    header[0..2].copy_from_slice(&type_val.to_be_bytes());
    header[2..6].copy_from_slice(&(value.len() as u32).to_be_bytes());
    writer.write_all(&header)?;
    writer.write_all(value)?;
    writer.flush()?;
    Ok(())
}

// ============================================================================
// Tunnel Reader/Writer
// ============================================================================

/// A reader that extracts data from TLV packets received through MGMT.
///
/// Buffers incoming TLV values and exposes them as a continuous byte stream.
pub struct TunnelReader<'a, R> {
    tlv_type: MgmtToCtl,
    reader: &'a mut R,
    buffer: &'a mut heapless::Vec<u8, MAX_VALUE_SIZE>,
}

impl<'a, R> TunnelReader<'a, R> {
    fn new(
        tlv_type: MgmtToCtl,
        reader: &'a mut R,
        buffer: &'a mut heapless::Vec<u8, MAX_VALUE_SIZE>,
    ) -> Self {
        Self {
            tlv_type,
            reader,
            buffer,
        }
    }
}

impl<'a, R: Read> Read for TunnelReader<'a, R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        while self.buffer.is_empty() {
            let tlv: Tlv<MgmtToCtl> = read_tlv(self.reader)
                .map_err(|e| match e {
                    TlvReadError::Io(io) => io,
                    TlvReadError::InvalidType => {
                        std::io::Error::new(std::io::ErrorKind::InvalidData, "invalid TLV type")
                    }
                    TlvReadError::TooLong => {
                        std::io::Error::new(std::io::ErrorKind::InvalidData, "TLV too long")
                    }
                })?
                .ok_or_else(|| {
                    std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "unexpected EOF")
                })?;

            if tlv.tlv_type != self.tlv_type {
                continue;
            }
            self.buffer.extend_from_slice(&tlv.value).unwrap();
        }

        let to_copy = core::cmp::min(self.buffer.len(), buf.len());
        buf[..to_copy].copy_from_slice(&self.buffer[..to_copy]);
        // Drain from front by copying remaining bytes and truncating
        let remaining = self.buffer.len() - to_copy;
        for i in 0..remaining {
            self.buffer[i] = self.buffer[i + to_copy];
        }
        self.buffer.truncate(remaining);
        Ok(to_copy)
    }
}

/// A writer that wraps TLV packets for tunneling through MGMT.
pub struct TunnelWriter<'a, W> {
    tlv_type: CtlToMgmt,
    writer: &'a mut W,
}

impl<'a, W> TunnelWriter<'a, W> {
    fn new(tlv_type: CtlToMgmt, writer: &'a mut W) -> Self {
        Self { tlv_type, writer }
    }
}

impl<'a, W: Write> Write for TunnelWriter<'a, W> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let to_write = core::cmp::min(MAX_VALUE_SIZE, buf.len());
        write_tlv(self.writer, self.tlv_type, &buf[..to_write])?;
        Ok(to_write)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.writer.flush()
    }
}

// ============================================================================
// MGMT Reader/Writer
// ============================================================================

/// Encapsulates the read side of MGMT communication.
pub struct MgmtReader<R> {
    from_mgmt: R,
    ui_buffer: heapless::Vec<u8, MAX_VALUE_SIZE>,
    net_buffer: heapless::Vec<u8, MAX_VALUE_SIZE>,
}

impl<R: Read> Read for MgmtReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.from_mgmt.read(buf)
    }
}

impl<R: Read> MgmtReader<R> {
    fn new(from_mgmt: R) -> Self {
        Self {
            from_mgmt,
            ui_buffer: heapless::Vec::new(),
            net_buffer: heapless::Vec::new(),
        }
    }

    /// Drain any pending data from the input buffer.
    ///
    /// This clears internal buffers and reads/discards any pending data from the
    /// underlying reader. Useful before starting a new protocol that expects a
    /// clean slate (e.g., bootloader communication after reset).
    ///
    /// Note: This relies on the underlying reader having a reasonable timeout
    /// (e.g., 50-100ms) so that reads will return when no data is available.
    pub fn drain(&mut self) {
        // Clear internal TLV buffers
        self.ui_buffer.clear();
        self.net_buffer.clear();

        // Read and discard any pending data from the serial port
        // With a typical 50ms timeout, this will return quickly when empty
        let mut buf = [0u8; 256];
        loop {
            match self.from_mgmt.read(&mut buf) {
                Ok(0) => break,    // EOF or no data
                Ok(_) => continue, // Discard and keep reading
                Err(_) => break,   // Timeout or error - buffer is drained
            }
        }
    }

    /// Read a TLV from the MGMT connection.
    pub fn read_tlv(&mut self) -> Result<Option<Tlv<MgmtToCtl>>, TlvReadError<std::io::Error>> {
        read_tlv(&mut self.from_mgmt)
    }

    /// Get a reader for the UI tunnel.
    pub fn ui(&mut self) -> TunnelReader<'_, R> {
        TunnelReader::new(MgmtToCtl::FromUi, &mut self.from_mgmt, &mut self.ui_buffer)
    }

    /// Get a reader for the NET tunnel.
    pub fn net(&mut self) -> TunnelReader<'_, R> {
        TunnelReader::new(
            MgmtToCtl::FromNet,
            &mut self.from_mgmt,
            &mut self.net_buffer,
        )
    }

    /// Get a mutable reference to the underlying reader.
    ///
    /// This is useful for operations that need to modify the underlying reader,
    /// such as setting the timeout on a serial port.
    pub fn inner_mut(&mut self) -> &mut R {
        &mut self.from_mgmt
    }
}

/// Encapsulates the write side of MGMT communication.
pub struct MgmtWriter<W> {
    to_mgmt: W,
}

impl<W: Write> Write for MgmtWriter<W> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.to_mgmt.write(buf)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.to_mgmt.flush()
    }
}

impl<W: Write> MgmtWriter<W> {
    fn new(to_mgmt: W) -> Self {
        Self { to_mgmt }
    }

    /// Write a TLV to the MGMT connection.
    pub fn write_tlv(&mut self, tlv_type: CtlToMgmt, value: &[u8]) -> std::io::Result<()> {
        write_tlv(&mut self.to_mgmt, tlv_type, value)
    }

    /// Get a writer for the UI tunnel (TLV protocol).
    pub fn ui(&mut self) -> TunnelWriter<'_, W> {
        TunnelWriter::new(CtlToMgmt::ToUi, &mut self.to_mgmt)
    }

    /// Get a writer for the NET tunnel.
    pub fn net(&mut self) -> TunnelWriter<'_, W> {
        TunnelWriter::new(CtlToMgmt::ToNet, &mut self.to_mgmt)
    }

    /// Get a mutable reference to the underlying writer.
    ///
    /// This is useful for operations that need to modify the underlying writer,
    /// such as setting the baud rate on a serial port.
    pub fn inner_mut(&mut self) -> &mut W {
        &mut self.to_mgmt
    }
}

// ============================================================================
// App
// ============================================================================

pub struct App<R, W> {
    reader: MgmtReader<R>,
    writer: MgmtWriter<W>,
}

impl<R, W> App<R, W>
where
    W: Write,
    R: Read,
{
    pub fn new(from_mgmt: R, to_mgmt: W) -> Self {
        Self {
            reader: MgmtReader::new(from_mgmt),
            writer: MgmtWriter::new(to_mgmt),
        }
    }

    /// Get a mutable reference to the underlying reader.
    ///
    /// This is useful for operations that need to modify the underlying reader,
    /// such as setting the timeout on a serial port.
    pub fn reader_mut(&mut self) -> &mut MgmtReader<R> {
        &mut self.reader
    }

    /// Get a mutable reference to the underlying writer.
    ///
    /// This is useful for operations that need to modify the underlying writer,
    /// such as setting the baud rate on a serial port.
    pub fn writer_mut(&mut self) -> &mut MgmtWriter<W> {
        &mut self.writer
    }

    pub fn mgmt_ping(&mut self, data: &[u8]) {
        write_tlv(&mut self.writer, CtlToMgmt::Ping, data).unwrap();
        let tlv: Tlv<MgmtToCtl> = read_tlv(&mut self.reader).unwrap().unwrap();
        assert_eq!(tlv.tlv_type, MgmtToCtl::Pong);
        assert_eq!(&tlv.value, data);
    }

    /// Send a Hello handshake to detect if a valid device is connected.
    ///
    /// Sends a 4-byte challenge value and verifies the response is the challenge
    /// XOR'd with b"LINK". Returns true if the handshake succeeded.
    pub fn hello(&mut self, challenge: &[u8; 4]) -> bool {
        const MAGIC: &[u8; 4] = b"LINK";

        if write_tlv(&mut self.writer, CtlToMgmt::Hello, challenge).is_err() {
            return false;
        }

        let tlv: Tlv<MgmtToCtl> = match read_tlv(&mut self.reader) {
            Ok(Some(tlv)) => tlv,
            _ => return false,
        };

        if tlv.tlv_type != MgmtToCtl::Hello || tlv.value.len() != 4 {
            return false;
        }

        // Verify response is challenge XOR'd with MAGIC
        for i in 0..4 {
            if tlv.value[i] != (challenge[i] ^ MAGIC[i]) {
                return false;
            }
        }
        true
    }

    pub fn ui_ping(&mut self, data: &[u8]) {
        write_tlv(&mut self.writer.ui(), MgmtToUi::Ping, data).unwrap();
        let tlv: Tlv<UiToMgmt> = read_tlv(&mut self.reader.ui()).unwrap().unwrap();
        assert_eq!(tlv.tlv_type, UiToMgmt::Pong);
        assert_eq!(&tlv.value, data);
    }

    pub fn net_ping(&mut self, data: &[u8]) {
        write_tlv(&mut self.writer.net(), MgmtToNet::Ping, data).unwrap();
        let tlv: Tlv<NetToMgmt> = read_tlv(&mut self.reader.net()).unwrap().unwrap();
        assert_eq!(tlv.tlv_type, NetToMgmt::Pong);
        assert_eq!(&tlv.value, data);
    }

    pub fn ui_first_circular_ping(&mut self, data: &[u8]) {
        write_tlv(&mut self.writer.ui(), MgmtToUi::CircularPing, data).unwrap();
        let tlv: Tlv<NetToMgmt> = read_tlv(&mut self.reader.net()).unwrap().unwrap();
        assert_eq!(tlv.tlv_type, NetToMgmt::CircularPing);
        assert_eq!(&tlv.value, data);
    }

    pub fn net_first_circular_ping(&mut self, data: &[u8]) {
        write_tlv(&mut self.writer.net(), MgmtToNet::CircularPing, data).unwrap();
        let tlv: Tlv<UiToMgmt> = read_tlv(&mut self.reader.ui()).unwrap().unwrap();
        assert_eq!(tlv.tlv_type, UiToMgmt::CircularPing);
        assert_eq!(&tlv.value, data);
    }

    /// Get the version stored in UI chip EEPROM.
    pub fn get_version(&mut self) -> u32 {
        write_tlv(&mut self.writer.ui(), MgmtToUi::GetVersion, &[]).unwrap();
        let tlv: Tlv<UiToMgmt> = read_tlv(&mut self.reader.ui()).unwrap().unwrap();
        assert_eq!(tlv.tlv_type, UiToMgmt::Version);
        assert_eq!(tlv.value.len(), 4);
        u32::from_be_bytes([tlv.value[0], tlv.value[1], tlv.value[2], tlv.value[3]])
    }

    /// Set the version stored in UI chip EEPROM.
    pub fn set_version(&mut self, version: u32) {
        write_tlv(
            &mut self.writer.ui(),
            MgmtToUi::SetVersion,
            &version.to_be_bytes(),
        )
        .unwrap();
        let tlv: Tlv<UiToMgmt> = read_tlv(&mut self.reader.ui()).unwrap().unwrap();
        assert_eq!(tlv.tlv_type, UiToMgmt::Ack);
    }

    /// Get the SFrame key stored in UI chip EEPROM.
    pub fn get_sframe_key(&mut self) -> [u8; 16] {
        write_tlv(&mut self.writer.ui(), MgmtToUi::GetSFrameKey, &[]).unwrap();
        let tlv: Tlv<UiToMgmt> = read_tlv(&mut self.reader.ui()).unwrap().unwrap();
        assert_eq!(tlv.tlv_type, UiToMgmt::SFrameKey);
        assert_eq!(tlv.value.len(), 16);
        let mut key = [0u8; 16];
        key.copy_from_slice(&tlv.value);
        key
    }

    /// Set the SFrame key stored in UI chip EEPROM.
    pub fn set_sframe_key(&mut self, key: &[u8; 16]) {
        write_tlv(&mut self.writer.ui(), MgmtToUi::SetSFrameKey, key).unwrap();
        let tlv: Tlv<UiToMgmt> = read_tlv(&mut self.reader.ui()).unwrap().unwrap();
        assert_eq!(tlv.tlv_type, UiToMgmt::Ack);
    }

    /// Set UI chip loopback mode.
    /// When enabled, mic audio goes directly to speaker instead of to NET.
    pub fn ui_set_loopback(&mut self, enabled: bool) {
        write_tlv(
            &mut self.writer.ui(),
            MgmtToUi::SetLoopback,
            &[enabled as u8],
        )
        .unwrap();
        let tlv: Tlv<UiToMgmt> = read_tlv(&mut self.reader.ui()).unwrap().unwrap();
        assert_eq!(tlv.tlv_type, UiToMgmt::Ack);
    }

    /// Get UI chip loopback mode.
    pub fn ui_get_loopback(&mut self) -> bool {
        write_tlv(&mut self.writer.ui(), MgmtToUi::GetLoopback, &[]).unwrap();
        let tlv: Tlv<UiToMgmt> = read_tlv(&mut self.reader.ui()).unwrap().unwrap();
        assert_eq!(tlv.tlv_type, UiToMgmt::Loopback);
        tlv.value.first().copied().unwrap_or(0) != 0
    }

    /// Set NET chip loopback mode.
    /// When enabled, audio from UI goes back to UI through jitter buffer instead of to WebSocket.
    pub fn net_set_loopback(&mut self, enabled: bool) {
        write_tlv(
            &mut self.writer.net(),
            MgmtToNet::SetLoopback,
            &[enabled as u8],
        )
        .unwrap();
        let tlv: Tlv<NetToMgmt> = read_tlv(&mut self.reader.net()).unwrap().unwrap();
        assert_eq!(tlv.tlv_type, NetToMgmt::Ack);
    }

    /// Get NET chip loopback mode.
    pub fn net_get_loopback(&mut self) -> bool {
        write_tlv(&mut self.writer.net(), MgmtToNet::GetLoopback, &[]).unwrap();
        let tlv: Tlv<NetToMgmt> = read_tlv(&mut self.reader.net()).unwrap().unwrap();
        assert_eq!(tlv.tlv_type, NetToMgmt::Loopback);
        tlv.value.first().copied().unwrap_or(0) != 0
    }

    /// Add a WiFi SSID and password pair to NET chip storage.
    pub fn add_wifi_ssid(&mut self, ssid: &str, password: &str) {
        let wifi = WifiSsid {
            ssid: ssid.try_into().expect("SSID too long"),
            password: password.try_into().expect("Password too long"),
        };
        let mut buf = [0u8; 128];
        let serialized = postcard::to_slice(&wifi, &mut buf).expect("Serialization failed");
        write_tlv(&mut self.writer.net(), MgmtToNet::AddWifiSsid, serialized).unwrap();
        let tlv: Tlv<NetToMgmt> = read_tlv(&mut self.reader.net()).unwrap().unwrap();
        assert_eq!(tlv.tlv_type, NetToMgmt::Ack);
    }

    /// Get all WiFi SSIDs from NET chip storage.
    pub fn get_wifi_ssids(&mut self) -> heapless::Vec<WifiSsid, 8> {
        write_tlv(&mut self.writer.net(), MgmtToNet::GetWifiSsids, &[]).unwrap();
        let tlv: Tlv<NetToMgmt> = read_tlv(&mut self.reader.net()).unwrap().unwrap();
        assert_eq!(tlv.tlv_type, NetToMgmt::WifiSsids);
        postcard::from_bytes(&tlv.value).expect("Deserialization failed")
    }

    /// Clear all WiFi SSIDs from NET chip storage.
    pub fn clear_wifi_ssids(&mut self) {
        write_tlv(&mut self.writer.net(), MgmtToNet::ClearWifiSsids, &[]).unwrap();
        let tlv: Tlv<NetToMgmt> = read_tlv(&mut self.reader.net()).unwrap().unwrap();
        assert_eq!(tlv.tlv_type, NetToMgmt::Ack);
    }

    /// Get the relay URL from NET chip storage.
    pub fn get_relay_url(&mut self) -> heapless::String<128> {
        write_tlv(&mut self.writer.net(), MgmtToNet::GetRelayUrl, &[]).unwrap();
        let tlv: Tlv<NetToMgmt> = read_tlv(&mut self.reader.net()).unwrap().unwrap();
        assert_eq!(tlv.tlv_type, NetToMgmt::RelayUrl);
        let url_str = core::str::from_utf8(&tlv.value).expect("Invalid UTF-8");
        url_str.try_into().expect("URL too long")
    }

    /// Set the relay URL in NET chip storage.
    pub fn set_relay_url(&mut self, url: &str) {
        write_tlv(
            &mut self.writer.net(),
            MgmtToNet::SetRelayUrl,
            url.as_bytes(),
        )
        .unwrap();
        let tlv: Tlv<NetToMgmt> = read_tlv(&mut self.reader.net()).unwrap().unwrap();
        assert_eq!(tlv.tlv_type, NetToMgmt::Ack);
    }

    /// Send data over WebSocket and verify echo response.
    ///
    /// This sends data to the relay server via WebSocket and expects the same
    /// data back (assumes an echo server). Useful for testing WS connectivity.
    pub fn ws_ping(&mut self, data: &[u8]) {
        write_tlv(&mut self.writer.net(), MgmtToNet::WsSend, data).unwrap();
        let tlv: Tlv<NetToMgmt> = read_tlv(&mut self.reader.net()).unwrap().unwrap();
        assert_eq!(tlv.tlv_type, NetToMgmt::WsReceived);
        assert_eq!(&tlv.value, data);
    }

    /// Run WebSocket echo test to measure bidirectional throughput.
    ///
    /// This test:
    /// 1. Sends 50 packets (640 bytes each) at 20ms intervals (50 fps)
    /// 2. Expects the echo server to return each packet
    /// 3. Measures jitter before and after the jitter buffer
    ///
    /// Returns EchoTestResults with raw and buffered jitter measurements.
    pub fn ws_echo_test(&mut self) -> EchoTestResults {
        // Tunnel through MGMT to NET (like ws_ping does)
        write_tlv(&mut self.writer.net(), MgmtToNet::WsEchoTest, &[]).unwrap();

        // Wait for result from NET (tunneled through MGMT as FromNet)
        let tlv: Tlv<NetToMgmt> = read_tlv(&mut self.reader.net()).unwrap().unwrap();
        assert_eq!(tlv.tlv_type, NetToMgmt::WsEchoTestResult);

        // Parse result:
        // - sent (1 byte)
        // - received (1 byte)
        // - buffered_output (1 byte)
        // - underruns (1 byte)
        // - raw_jitter_count (1 byte)
        // - raw_jitter_us (4 bytes each)
        // - buffered_jitter_count (1 byte)
        // - buffered_jitter_us (4 bytes each)
        let sent = tlv.value.get(0).copied().unwrap_or(0);
        let received = tlv.value.get(1).copied().unwrap_or(0);
        let buffered_output = tlv.value.get(2).copied().unwrap_or(0);
        let underruns = tlv.value.get(3).copied().unwrap_or(0);
        let raw_count = tlv.value.get(4).copied().unwrap_or(0) as usize;

        let mut offset = 5;
        let mut raw_jitter_us: heapless::Vec<u32, 50> = heapless::Vec::new();
        for _ in 0..raw_count {
            if offset + 4 <= tlv.value.len() {
                let time_us = u32::from_le_bytes([
                    tlv.value[offset],
                    tlv.value[offset + 1],
                    tlv.value[offset + 2],
                    tlv.value[offset + 3],
                ]);
                let _ = raw_jitter_us.push(time_us);
                offset += 4;
            }
        }

        let buffered_count = tlv.value.get(offset).copied().unwrap_or(0) as usize;
        offset += 1;

        let mut buffered_jitter_us: heapless::Vec<u32, 50> = heapless::Vec::new();
        for _ in 0..buffered_count {
            if offset + 4 <= tlv.value.len() {
                let time_us = u32::from_le_bytes([
                    tlv.value[offset],
                    tlv.value[offset + 1],
                    tlv.value[offset + 2],
                    tlv.value[offset + 3],
                ]);
                let _ = buffered_jitter_us.push(time_us);
                offset += 4;
            }
        }

        EchoTestResults {
            sent,
            received,
            buffered_output,
            underruns,
            raw_jitter_us,
            buffered_jitter_us,
        }
    }

    /// Run a WebSocket speed test.
    ///
    /// This sends 50 packets as fast as possible (no delay between sends),
    /// then waits up to 2 seconds to receive responses.
    ///
    /// Returns SpeedTestResults with timing information.
    pub fn ws_speed_test(&mut self) -> SpeedTestResults {
        // Tunnel through MGMT to NET
        write_tlv(&mut self.writer.net(), MgmtToNet::WsSpeedTest, &[]).unwrap();

        // Wait for result from NET (tunneled through MGMT as FromNet)
        let tlv: Tlv<NetToMgmt> = read_tlv(&mut self.reader.net()).unwrap().unwrap();
        assert_eq!(tlv.tlv_type, NetToMgmt::WsSpeedTestResult);

        // Parse result: sent (1), received (1), send_time_ms (4), recv_time_ms (4)
        let sent = tlv.value.get(0).copied().unwrap_or(0);
        let received = tlv.value.get(1).copied().unwrap_or(0);
        let send_time_ms = if tlv.value.len() >= 6 {
            u32::from_le_bytes([tlv.value[2], tlv.value[3], tlv.value[4], tlv.value[5]])
        } else {
            0
        };
        let recv_time_ms = if tlv.value.len() >= 10 {
            u32::from_le_bytes([tlv.value[6], tlv.value[7], tlv.value[8], tlv.value[9]])
        } else {
            0
        };

        SpeedTestResults {
            sent,
            received,
            send_time_ms,
            recv_time_ms,
        }
    }

    /// Get bootloader information from the MGMT chip.
    ///
    /// This assumes the MGMT chip is already in bootloader mode and the serial
    /// connection is configured correctly (even parity, 115200 baud).
    ///
    /// Returns bootloader version, chip ID, supported commands, and optionally
    /// a sample of flash memory if read protection is not enabled.
    pub fn get_mgmt_bootloader_info(
        &mut self,
    ) -> Result<MgmtBootloaderInfo, stm::Error<std::io::Error>> {
        // Drain any stale data from previous communication before starting bootloader protocol
        self.reader.drain();

        let mut bl = Bootloader::new(&mut self.reader, &mut self.writer);

        // Initialize communication (sends 0x7F for auto-baud detection)
        bl.init()?;

        // Get bootloader info
        let info = bl.get()?;

        // Get chip ID
        let chip_id = bl.get_id()?;

        // Try to read a small amount of memory from the start of flash
        let mut flash_sample = [0u8; 32];
        let flash_result = bl.read_memory(0x0800_0000, &mut flash_sample);
        let flash_sample = if flash_result.is_ok() {
            Some(flash_sample)
        } else {
            None // Read protection may be enabled
        };

        // Reset MGMT chip back to normal operation by jumping to user firmware
        bl.go(0x0800_0000)?;

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
    /// This assumes the MGMT chip is already in bootloader mode (requires
    /// manual BOOT0 pin manipulation) and the serial connection is configured
    /// correctly (even parity, 115200 baud).
    ///
    /// This method:
    /// 1. Initializes bootloader communication
    /// 2. Performs a global erase of the flash
    /// 3. Writes the firmware in 256-byte chunks
    /// 4. Verifies the written data by reading it back
    /// 5. Jumps to the new firmware
    ///
    /// The progress callback is called with (phase, bytes_processed, total_bytes).
    pub fn flash_mgmt<F>(
        &mut self,
        firmware: &[u8],
        mut progress: F,
    ) -> Result<(), FlashError<std::io::Error>>
    where
        F: FnMut(FlashPhase, usize, usize),
    {
        // Drain any stale data from previous communication (e.g., hello response)
        // before starting bootloader protocol
        self.reader.drain();

        let mut bl = Bootloader::new(&mut self.reader, &mut self.writer);

        // Initialize communication
        bl.init()?;

        // Erase pages needed for firmware (STM32F072CB has 2KB pages)
        // Erase page-by-page for progress feedback
        const PAGE_SIZE: usize = 2048;
        let pages_needed = (firmware.len() + PAGE_SIZE - 1) / PAGE_SIZE;
        let pages_needed = pages_needed.max(1); // At least 1 page

        for page in 0..pages_needed {
            progress(FlashPhase::Erasing, page, pages_needed);
            // Try extended erase first, fall back to legacy
            let page_num = page as u16;
            if bl.extended_erase(Some(&[page_num]), None).is_err() {
                bl.erase(Some(&[page as u8]))?;
            }
        }
        progress(FlashPhase::Erasing, pages_needed, pages_needed);

        // Write firmware in 256-byte chunks
        let total = firmware.len();
        let mut written = 0;
        let base_address: u32 = 0x0800_0000;

        for chunk in firmware.chunks(256) {
            let address = base_address + written as u32;
            bl.write_memory(address, chunk)?;
            written += chunk.len();
            progress(FlashPhase::Writing, written, total);
        }

        // Verify by reading back
        let mut verified = 0;
        let mut read_buf = [0u8; 256];

        for chunk in firmware.chunks(256) {
            let address = base_address + verified as u32;
            let len = bl.read_memory(address, &mut read_buf[..chunk.len()])?;
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
        bl.go(0x0800_0000)?;

        Ok(())
    }

    /// Reset the UI chip into bootloader mode.
    ///
    /// Sends a command to MGMT which toggles the BOOT0 and RST pins
    /// to put the UI chip into bootloader mode.
    pub fn reset_ui_to_bootloader(&mut self) {
        write_tlv(&mut self.writer, CtlToMgmt::ResetUiToBootloader, &[]).unwrap();
        let tlv: Tlv<MgmtToCtl> = read_tlv(&mut self.reader).unwrap().unwrap();
        assert_eq!(tlv.tlv_type, MgmtToCtl::Ack);
    }

    /// Reset the UI chip into user mode (normal operation).
    ///
    /// Sends a command to MGMT which toggles the BOOT0 and RST pins
    /// to put the UI chip back into normal user mode.
    pub fn reset_ui_to_user(&mut self) {
        write_tlv(&mut self.writer, CtlToMgmt::ResetUiToUser, &[]).unwrap();
        let tlv: Tlv<MgmtToCtl> = read_tlv(&mut self.reader).unwrap().unwrap();
        assert_eq!(tlv.tlv_type, MgmtToCtl::Ack);
    }

    /// Hold the UI chip in reset.
    ///
    /// Sends a command to MGMT to assert the RST pin low, keeping the
    /// UI chip in reset until released.
    pub fn hold_ui_reset(&mut self) {
        write_tlv(&mut self.writer, CtlToMgmt::HoldUiReset, &[]).unwrap();
        let tlv: Tlv<MgmtToCtl> = read_tlv(&mut self.reader).unwrap().unwrap();
        assert_eq!(tlv.tlv_type, MgmtToCtl::Ack);
    }

    /// Reset the NET chip into bootloader mode.
    ///
    /// Sends a command to MGMT which toggles the BOOT0 and RST pins
    /// to put the NET chip into bootloader mode.
    pub fn reset_net_to_bootloader(&mut self) {
        eprintln!("[debug] Sending ResetNetToBootloader command to MGMT...");
        write_tlv(&mut self.writer, CtlToMgmt::ResetNetToBootloader, &[]).unwrap();
        // Read TLVs, skipping any FromNet (boot messages from NET chip) until we get the Ack
        for i in 0..100 {
            eprintln!("[trace] Waiting for Ack (attempt {})", i);
            let tlv: Tlv<MgmtToCtl> = read_tlv(&mut self.reader).unwrap().unwrap();
            eprintln!("[trace] Received TLV: {:?}", tlv.tlv_type);
            match tlv.tlv_type {
                MgmtToCtl::Ack => {
                    eprintln!("[debug] Received Ack from MGMT");
                    return;
                }
                MgmtToCtl::FromNet => {
                    eprintln!("[trace] Skipping FromNet TLV ({} bytes)", tlv.value.len());
                    continue;
                }
                other => panic!("unexpected TLV type: {:?}", other),
            }
        }
        panic!("gave up waiting for Ack after discarding 100 FromNet TLVs");
    }

    /// Reset the NET chip into user mode (normal operation).
    ///
    /// Sends a command to MGMT which toggles the BOOT0 and RST pins
    /// to put the NET chip back into normal user mode.
    pub fn reset_net_to_user(&mut self) {
        write_tlv(&mut self.writer, CtlToMgmt::ResetNetToUser, &[]).unwrap();
        // Read TLVs, skipping any FromNet (boot messages from NET chip) until we get the Ack
        for _ in 0..100 {
            let tlv: Tlv<MgmtToCtl> = read_tlv(&mut self.reader).unwrap().unwrap();
            match tlv.tlv_type {
                MgmtToCtl::Ack => return,
                MgmtToCtl::FromNet => continue,
                other => panic!("unexpected TLV type: {:?}", other),
            }
        }
        panic!("gave up waiting for Ack after discarding 100 FromNet TLVs");
    }

    /// Set the NET UART baud rate on the MGMT chip.
    ///
    /// This changes the baud rate between MGMT and NET chips.
    /// The change takes effect immediately after MGMT processes the command.
    pub fn set_net_baud_rate(&mut self, baud_rate: u32) {
        write_tlv(
            &mut self.writer,
            CtlToMgmt::SetNetBaudRate,
            &baud_rate.to_le_bytes(),
        )
        .unwrap();
        let tlv: Tlv<MgmtToCtl> = read_tlv(&mut self.reader).unwrap().unwrap();
        assert_eq!(tlv.tlv_type, MgmtToCtl::Ack);
    }

    /// Set the CTL UART baud rate on the MGMT chip.
    ///
    /// This changes the baud rate between CTL and MGMT.
    /// IMPORTANT: The ACK is sent at the old baud rate before the change takes effect.
    /// After calling this, the caller must change their own serial port baud rate
    /// to match before continuing communication.
    pub fn set_ctl_baud_rate(&mut self, baud_rate: u32) {
        write_tlv(
            &mut self.writer,
            CtlToMgmt::SetCtlBaudRate,
            &baud_rate.to_le_bytes(),
        )
        .unwrap();
        // Read ACK at current baud rate (before MGMT switches)
        let tlv: Tlv<MgmtToCtl> = read_tlv(&mut self.reader).unwrap().unwrap();
        assert_eq!(tlv.tlv_type, MgmtToCtl::Ack);
    }

    /// Get bootloader information from the UI chip.
    ///
    /// This method:
    /// 1. Resets the UI chip into bootloader mode
    /// 2. Queries bootloader information via the tunneled UI connection
    /// 3. Resets the UI chip back to user mode
    ///
    /// The `delay_ms` parameter should be a function that sleeps for the given
    /// number of milliseconds. For std, use `|ms| std::thread::sleep(Duration::from_millis(ms))`.
    ///
    /// Returns bootloader version, chip ID, supported commands, and optionally
    /// a sample of flash memory if read protection is not enabled.
    pub fn get_ui_bootloader_info<D>(
        &mut self,
        delay_ms: D,
    ) -> Result<MgmtBootloaderInfo, stm::Error<std::io::Error>>
    where
        D: FnOnce(u64),
    {
        // Reset UI chip into bootloader mode
        self.reset_ui_to_bootloader();

        // Wait for bootloader to be ready
        delay_ms(1000);

        // Query bootloader info, capturing any error
        let result = self.query_ui_bootloader();

        // Always reset UI chip back to user mode
        self.reset_ui_to_user();

        result
    }

    /// Helper to query the UI bootloader. Separated so borrows are released before reset.
    fn query_ui_bootloader(&mut self) -> Result<MgmtBootloaderInfo, stm::Error<std::io::Error>> {
        // Create a bootloader client using the tunneled UI connection
        let mut ui_reader = self.reader.ui();
        let mut ui_writer = self.writer.ui();
        let mut bl = Bootloader::new(&mut ui_reader, &mut ui_writer);

        // Initialize communication (sends 0x7F for auto-baud detection)
        bl.init()?;

        // Get bootloader info
        let info = bl.get()?;

        // Get chip ID
        let chip_id = bl.get_id()?;

        // Try to read a small amount of memory from the start of flash
        let mut flash_sample = [0u8; 32];
        let flash_sample = match bl.read_memory(0x0800_0000, &mut flash_sample) {
            Ok(_) => Some(flash_sample),
            Err(_) => None, // Read protection may be enabled
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
    /// 2. Performs a mass erase of the flash
    /// 3. Writes the firmware in 256-byte chunks
    /// 4. Verifies the written data by reading it back
    /// 5. Resets the UI chip back to user mode
    ///
    /// The `delay_ms` parameter should be a function that sleeps for the given
    /// number of milliseconds.
    ///
    /// The progress callback is called with (phase, bytes_processed, total_bytes).
    pub fn flash_ui<F, D>(
        &mut self,
        firmware: &[u8],
        delay_ms: D,
        mut progress: F,
    ) -> Result<(), FlashError<std::io::Error>>
    where
        F: FnMut(FlashPhase, usize, usize),
        D: FnOnce(u64),
    {
        // Reset UI chip into bootloader mode
        self.reset_ui_to_bootloader();

        // Wait for bootloader to be ready
        delay_ms(100);

        // Flash the firmware
        let result = self.do_flash_ui(firmware, &mut progress);

        // Always reset UI chip back to user mode
        self.reset_ui_to_user();

        result
    }

    /// Helper to flash the UI chip. Separated so borrows are released before reset.
    fn do_flash_ui<F>(
        &mut self,
        firmware: &[u8],
        progress: &mut F,
    ) -> Result<(), FlashError<std::io::Error>>
    where
        F: FnMut(FlashPhase, usize, usize),
    {
        let mut ui_reader = self.reader.ui();
        let mut ui_writer = self.writer.ui();
        let mut bl = Bootloader::new(&mut ui_reader, &mut ui_writer);

        // Initialize communication
        bl.init()?;

        // Erase sectors needed for firmware (STM32F405RG has variable sector sizes)
        // Sectors 0-3: 16KB, Sector 4: 64KB, Sectors 5-11: 128KB
        let sectors_needed = sectors_for_size_f405(firmware.len());

        for sector in 0..sectors_needed {
            progress(FlashPhase::Erasing, sector, sectors_needed);
            bl.extended_erase(Some(&[sector as u16]), None)?;
        }
        progress(FlashPhase::Erasing, sectors_needed, sectors_needed);

        // Write firmware in 256-byte chunks
        let total = firmware.len();
        let mut written = 0;
        let base_address: u32 = 0x0800_0000;

        for chunk in firmware.chunks(256) {
            let address = base_address + written as u32;
            bl.write_memory(address, chunk)?;
            written += chunk.len();
            progress(FlashPhase::Writing, written, total);
        }

        // Verify by reading back
        let mut verified = 0;
        let mut read_buf = [0u8; 256];

        for chunk in firmware.chunks(256) {
            let address = base_address + verified as u32;
            let len = bl.read_memory(address, &mut read_buf[..chunk.len()])?;
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

        Ok(())
    }
}

// =============================================================================
// NET chip (ESP32) support via espflash
// =============================================================================

/// Serial port wrapper for the TLV tunnel to NET chip.
///
/// DTR/RTS signals are mapped directly to BOOT/RST pins:
/// - DTR HIGH → BOOT LOW (bootloader mode), DTR LOW → BOOT HIGH (normal)
/// - RTS HIGH → RST LOW (chip in reset), RTS LOW → RST HIGH (chip running)
struct TunnelSerialPort<'a, R, W> {
    reader: &'a mut MgmtReader<R>,
    writer: &'a mut MgmtWriter<W>,
    read_buffer: Vec<u8>,
    timeout: Duration,
    baud_rate: u32,
}

impl<'a, R, W> TunnelSerialPort<'a, R, W>
where
    R: Read + Send,
    W: Write + Send,
{
    fn new(reader: &'a mut MgmtReader<R>, writer: &'a mut MgmtWriter<W>) -> Self {
        TunnelSerialPort {
            reader,
            writer,
            read_buffer: Vec::new(),
            timeout: Duration::from_secs(3),
            baud_rate: 115_200,
        }
    }
}

impl<R, W> TunnelSerialPort<'_, R, W>
where
    R: Read + Send + SetTimeout,
    W: Write + Send,
{
    /// Propagate timeout to underlying serial port.
    fn propagate_timeout(&mut self, timeout: Duration) -> std::io::Result<()> {
        self.reader.inner_mut().set_timeout(timeout)
    }
}

impl<R, W> io::Read for TunnelSerialPort<'_, R, W>
where
    R: Read + Send,
    W: Write + Send,
{
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if !self.read_buffer.is_empty() {
            let to_copy = buf.len().min(self.read_buffer.len());
            buf[..to_copy].copy_from_slice(&self.read_buffer[..to_copy]);
            self.read_buffer.drain(..to_copy);
            return Ok(to_copy);
        }
        let mut net_reader = self.reader.net();
        net_reader.read(buf)
    }
}

impl<R, W> io::Write for TunnelSerialPort<'_, R, W>
where
    R: Read + Send,
    W: Write + Send,
{
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let mut net_writer = self.writer.net();
        net_writer.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        let mut net_writer = self.writer.net();
        net_writer.flush()
    }
}

impl<R, W> SerialPort for TunnelSerialPort<'_, R, W>
where
    R: Read + Send + SetTimeout,
    W: Write + Send,
{
    fn name(&self) -> Option<String> {
        Some("tunnel".to_string())
    }
    fn baud_rate(&self) -> serialport::Result<u32> {
        Ok(self.baud_rate)
    }
    fn data_bits(&self) -> serialport::Result<DataBits> {
        Ok(DataBits::Eight)
    }
    fn flow_control(&self) -> serialport::Result<FlowControl> {
        Ok(FlowControl::None)
    }
    fn parity(&self) -> serialport::Result<Parity> {
        Ok(Parity::None)
    }
    fn stop_bits(&self) -> serialport::Result<StopBits> {
        Ok(StopBits::One)
    }
    fn timeout(&self) -> Duration {
        self.timeout
    }
    fn set_baud_rate(&mut self, baud_rate: u32) -> serialport::Result<()> {
        self.baud_rate = baud_rate;
        Ok(())
    }
    fn set_data_bits(&mut self, _: DataBits) -> serialport::Result<()> {
        Ok(())
    }
    fn set_flow_control(&mut self, _: FlowControl) -> serialport::Result<()> {
        Ok(())
    }
    fn set_parity(&mut self, _: Parity) -> serialport::Result<()> {
        Ok(())
    }
    fn set_stop_bits(&mut self, _: StopBits) -> serialport::Result<()> {
        Ok(())
    }
    fn set_timeout(&mut self, timeout: Duration) -> serialport::Result<()> {
        self.timeout = timeout;
        // Propagate to underlying serial port so reads use the correct timeout
        self.propagate_timeout(timeout)
            .map_err(|e| serialport::Error::new(serialport::ErrorKind::Io(e.kind()), e.to_string()))
    }
    fn write_request_to_send(&mut self, level: bool) -> serialport::Result<()> {
        let rst = !level; // RTS HIGH = RST LOW
        self.writer
            .write_tlv(CtlToMgmt::SetNetRst, &[rst as u8])
            .map_err(|e| serialport::Error::new(serialport::ErrorKind::Io(e.kind()), e.to_string()))
    }
    fn write_data_terminal_ready(&mut self, level: bool) -> serialport::Result<()> {
        let boot = !level; // DTR HIGH = BOOT LOW
        self.writer
            .write_tlv(CtlToMgmt::SetNetBoot, &[boot as u8])
            .map_err(|e| serialport::Error::new(serialport::ErrorKind::Io(e.kind()), e.to_string()))
    }
    fn read_clear_to_send(&mut self) -> serialport::Result<bool> {
        Ok(true)
    }
    fn read_data_set_ready(&mut self) -> serialport::Result<bool> {
        Ok(true)
    }
    fn read_ring_indicator(&mut self) -> serialport::Result<bool> {
        Ok(false)
    }
    fn read_carrier_detect(&mut self) -> serialport::Result<bool> {
        Ok(true)
    }
    fn bytes_to_read(&self) -> serialport::Result<u32> {
        Ok(self.read_buffer.len() as u32)
    }
    fn bytes_to_write(&self) -> serialport::Result<u32> {
        Ok(0)
    }
    fn clear(&self, _: ClearBuffer) -> serialport::Result<()> {
        Ok(())
    }
    fn try_clone(&self) -> serialport::Result<Box<dyn SerialPort>> {
        Err(serialport::Error::new(
            serialport::ErrorKind::InvalidInput,
            "cannot clone",
        ))
    }
    fn set_break(&self) -> serialport::Result<()> {
        Ok(())
    }
    fn clear_break(&self) -> serialport::Result<()> {
        Ok(())
    }
}

/// Trait for types that support setting a read timeout.
pub trait SetTimeout {
    fn set_timeout(&mut self, timeout: Duration) -> std::io::Result<()>;
}

// Implement for BufReader wrapping SerialPort
impl SetTimeout for std::io::BufReader<Box<dyn serialport::SerialPort>> {
    fn set_timeout(&mut self, timeout: Duration) -> std::io::Result<()> {
        self.get_mut()
            .set_timeout(timeout)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e.to_string()))
    }
}

// NET chip operations (ESP32)
impl<R, W> App<R, W>
where
    R: Read + Send + SetTimeout,
    W: Write + Send,
{
    /// Flash firmware to the NET chip (ESP32).
    ///
    /// The `firmware` parameter should be an ELF file - espflash converts it
    /// to IDF bootloader format. Progress is reported via the `ProgressCallbacks` trait.
    ///
    /// If `partition_table` is provided, it should be a path to a CSV or binary
    /// partition table file. Otherwise, the default partition table is used.
    pub fn flash_net(
        &mut self,
        elf_data: &[u8],
        partition_table: Option<&Path>,
        progress: &mut dyn ProgressCallbacks,
    ) -> Result<(), EspflashError> {
        let port = TunnelSerialPort::new(&mut self.reader, &mut self.writer);
        let port_info = UsbPortInfo {
            vid: 0x303A,
            pid: 0x1002,
            serial_number: Some("MGMT_BRIDGE".to_string()),
            manufacturer: Some("Link".to_string()),
            product: Some("MGMT Bridge".to_string()),
        };

        let connection = Connection::new(
            port,
            port_info,
            ResetAfterOperation::HardReset,
            ResetBeforeOperation::DefaultReset,
            115_200,
        );

        println!("About to connect");

        // Pass explicit 115200 baud rate to prevent espflash from changing it
        let mut flasher = Flasher::connect(connection, false, false, true, None, Some(115_200))
            .map_err(|e| EspflashError::Espflash(format!("{:?}", e)))?;

        println!("About to flash");

        // Actually flash
        let info = flasher
            .device_info()
            .map_err(|e| EspflashError::Espflash(format!("device_info: {:?}", e)))?;
        let chip = flasher.chip();

        let flash_settings = FlashSettings::new(None, Some(info.flash_size), None);
        let flash_data = FlashData::new(flash_settings, 0, None, chip, info.crystal_frequency);

        let image_format =
            IdfBootloaderFormat::new(elf_data, &flash_data, partition_table, None, None, None)
                .map_err(|e| EspflashError::Espflash(format!("IdfBootloaderFormat: {:?}", e)))?;

        flasher
            .load_image_to_flash(progress, image_format.into())
            .map_err(|e| EspflashError::Espflash(format!("{:?}", e)))?;

        flasher
            .connection()
            .reset()
            .map_err(|e| EspflashError::Espflash(format!("reset: {:?}", e)))?;

        Ok(())
    }

    /// Get NET chip bootloader info.
    ///
    /// Returns detailed device information including chip type, revision,
    /// flash size, features, MAC address, and security info.
    pub fn get_net_bootloader_info(&mut self) -> Result<EspflashDeviceInfo, EspflashError> {
        let port = TunnelSerialPort::new(&mut self.reader, &mut self.writer);
        let port_info = UsbPortInfo {
            vid: 0,
            pid: 0,
            serial_number: None,
            manufacturer: None,
            product: None,
        };

        let connection = Connection::new(
            port,
            port_info,
            ResetAfterOperation::NoReset,
            ResetBeforeOperation::DefaultReset,
            115_200,
        );

        let mut flasher =
            Flasher::connect(connection, false, false, false, Some(Chip::Esp32s3), None)
                .map_err(|e| EspflashError::Espflash(format!("{:?}", e)))?;

        let device_info = flasher
            .device_info()
            .map_err(|e| EspflashError::Espflash(format!("device_info: {:?}", e)))?;

        let security_info = flasher
            .security_info()
            .map_err(|e| EspflashError::Espflash(format!("security_info: {:?}", e)))?;

        Ok(EspflashDeviceInfo {
            device_info,
            security_info,
        })
    }

    /// Erase the NET chip's entire flash.
    pub fn erase_net(&mut self) -> Result<(), EspflashError> {
        let port = TunnelSerialPort::new(&mut self.reader, &mut self.writer);
        let port_info = UsbPortInfo {
            vid: 0x303A,
            pid: 0x1002,
            serial_number: Some("MGMT_BRIDGE".to_string()),
            manufacturer: Some("Link".to_string()),
            product: Some("MGMT Bridge".to_string()),
        };

        let connection = Connection::new(
            port,
            port_info,
            ResetAfterOperation::HardReset,
            ResetBeforeOperation::DefaultReset,
            115_200,
        );

        let mut flasher = Flasher::connect(connection, false, false, true, None, Some(115_200))
            .map_err(|e| EspflashError::Espflash(format!("{:?}", e)))?;

        flasher
            .erase_flash()
            .map_err(|e| EspflashError::Espflash(format!("erase_flash: {:?}", e)))?;

        flasher
            .connection()
            .reset()
            .map_err(|e| EspflashError::Espflash(format!("reset: {:?}", e)))?;

        Ok(())
    }

    // =========================================================================
    // MoQ commands
    // =========================================================================

    /// Get MoQ relay URL from NET chip.
    pub fn get_moq_relay_url(&mut self) -> heapless::String<128> {
        write_tlv(&mut self.writer.net(), MgmtToNet::GetMoqRelayUrl, &[]).unwrap();
        let tlv: Tlv<NetToMgmt> = read_tlv(&mut self.reader.net()).unwrap().unwrap();
        assert_eq!(tlv.tlv_type, NetToMgmt::MoqRelayUrl);
        let url_str = core::str::from_utf8(&tlv.value).expect("Invalid UTF-8");
        url_str.try_into().expect("URL too long")
    }

    /// Set MoQ relay URL on NET chip.
    pub fn set_moq_relay_url(&mut self, url: &str) {
        write_tlv(
            &mut self.writer.net(),
            MgmtToNet::SetMoqRelayUrl,
            url.as_bytes(),
        )
        .unwrap();
        let tlv: Tlv<NetToMgmt> = read_tlv(&mut self.reader.net()).unwrap().unwrap();
        assert_eq!(tlv.tlv_type, NetToMgmt::Ack);
    }

    /// Get benchmark FPS from NET chip.
    pub fn get_benchmark_fps(&mut self) -> u32 {
        write_tlv(&mut self.writer.net(), MgmtToNet::GetBenchmarkFps, &[]).unwrap();
        let tlv: Tlv<NetToMgmt> = read_tlv(&mut self.reader.net()).unwrap().unwrap();
        assert_eq!(tlv.tlv_type, NetToMgmt::BenchmarkFps);
        if tlv.value.len() >= 4 {
            u32::from_le_bytes([tlv.value[0], tlv.value[1], tlv.value[2], tlv.value[3]])
        } else {
            0
        }
    }

    /// Set benchmark FPS on NET chip.
    pub fn set_benchmark_fps(&mut self, fps: u32) {
        write_tlv(
            &mut self.writer.net(),
            MgmtToNet::SetBenchmarkFps,
            &fps.to_le_bytes(),
        )
        .unwrap();
        let tlv: Tlv<NetToMgmt> = read_tlv(&mut self.reader.net()).unwrap().unwrap();
        assert_eq!(tlv.tlv_type, NetToMgmt::Ack);
    }

    /// Get benchmark payload size from NET chip.
    pub fn get_benchmark_payload_size(&mut self) -> u32 {
        write_tlv(
            &mut self.writer.net(),
            MgmtToNet::GetBenchmarkPayloadSize,
            &[],
        )
        .unwrap();
        let tlv: Tlv<NetToMgmt> = read_tlv(&mut self.reader.net()).unwrap().unwrap();
        assert_eq!(tlv.tlv_type, NetToMgmt::BenchmarkPayloadSize);
        if tlv.value.len() >= 4 {
            u32::from_le_bytes([tlv.value[0], tlv.value[1], tlv.value[2], tlv.value[3]])
        } else {
            0
        }
    }

    /// Set benchmark payload size on NET chip.
    pub fn set_benchmark_payload_size(&mut self, size: u32) {
        write_tlv(
            &mut self.writer.net(),
            MgmtToNet::SetBenchmarkPayloadSize,
            &size.to_le_bytes(),
        )
        .unwrap();
        let tlv: Tlv<NetToMgmt> = read_tlv(&mut self.reader.net()).unwrap().unwrap();
        assert_eq!(tlv.tlv_type, NetToMgmt::Ack);
    }

    /// Run clock mode on NET chip - subscribe to clock track and log times.
    pub fn run_clock(&mut self) -> Result<(), heapless::String<64>> {
        write_tlv(&mut self.writer.net(), MgmtToNet::RunClock, &[]).unwrap();
        let tlv: Tlv<NetToMgmt> = read_tlv(&mut self.reader.net()).unwrap().unwrap();
        match tlv.tlv_type {
            NetToMgmt::ModeStarted => Ok(()),
            NetToMgmt::Error => {
                let err = core::str::from_utf8(&tlv.value).unwrap_or("unknown error");
                Err(err.try_into().unwrap_or_default())
            }
            _ => Err("unexpected response".try_into().unwrap_or_default()),
        }
    }

    /// Run benchmark mode on NET chip - publish frames at configured FPS.
    pub fn run_benchmark(&mut self) -> Result<(), heapless::String<64>> {
        write_tlv(&mut self.writer.net(), MgmtToNet::RunBenchmark, &[]).unwrap();
        let tlv: Tlv<NetToMgmt> = read_tlv(&mut self.reader.net()).unwrap().unwrap();
        match tlv.tlv_type {
            NetToMgmt::ModeStarted => Ok(()),
            NetToMgmt::Error => {
                let err = core::str::from_utf8(&tlv.value).unwrap_or("unknown error");
                Err(err.try_into().unwrap_or_default())
            }
            _ => Err("unexpected response".try_into().unwrap_or_default()),
        }
    }

    /// Stop current running mode on NET chip.
    pub fn stop_mode(&mut self) -> Result<(), heapless::String<64>> {
        write_tlv(&mut self.writer.net(), MgmtToNet::StopMode, &[]).unwrap();
        let tlv: Tlv<NetToMgmt> = read_tlv(&mut self.reader.net()).unwrap().unwrap();
        match tlv.tlv_type {
            NetToMgmt::ModeStopped => Ok(()),
            NetToMgmt::Error => {
                let err = core::str::from_utf8(&tlv.value).unwrap_or("unknown error");
                Err(err.try_into().unwrap_or_default())
            }
            _ => Err("unexpected response".try_into().unwrap_or_default()),
        }
    }

    /// Send a chat message via MoQ.
    pub fn send_chat_message(&mut self, message: &str) -> Result<(), heapless::String<64>> {
        write_tlv(
            &mut self.writer.net(),
            MgmtToNet::SendChatMessage,
            message.as_bytes(),
        )
        .unwrap();
        let tlv: Tlv<NetToMgmt> = read_tlv(&mut self.reader.net()).unwrap().unwrap();
        match tlv.tlv_type {
            NetToMgmt::ChatMessageSent => Ok(()),
            NetToMgmt::Error => {
                let err = core::str::from_utf8(&tlv.value).unwrap_or("unknown error");
                Err(err.try_into().unwrap_or_default())
            }
            _ => Err("unexpected response".try_into().unwrap_or_default()),
        }
    }
}

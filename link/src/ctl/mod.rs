//! CTL (Controller) chip - the host computer interface.
//!
//! This module requires `std` for synchronous I/O operations.

extern crate alloc;

pub mod stm;

use crate::shared::{
    CtlToMgmt, HEADER_SIZE, LoopbackMode, MAX_CHANNELS, MAX_VALUE_SIZE, MgmtToCtl, MgmtToNet,
    MgmtToUi, NetLoopback, NetToMgmt, SYNC_WORD, Tlv, UiToMgmt, WifiSsid,
};
pub use crate::shared::ChannelConfig;
use espflash::connection::{Connection, ResetAfterOperation, ResetBeforeOperation};
use espflash::flasher::{FlashData, FlashSettings, Flasher};
use espflash::image_format::idf::IdfBootloaderFormat;
use espflash::target::Chip;
use serialport::{ClearBuffer, DataBits, FlowControl, Parity, SerialPort, StopBits, UsbPortInfo};
use std::io::{self, BufRead, Read, Write};
use std::path::Path;
use std::time::Duration;
use stm::Bootloader;

// ============================================================================
// BufferedPort - buffered reading for serial ports
// ============================================================================

/// A wrapper that provides buffered reading for a serial port.
///
/// This struct provides buffered reading while passing writes directly through.
/// Serial ports already handle write buffering, so we only need read buffering
/// to allow peeking/parsing without losing data.
pub struct BufferedPort<P> {
    port: P,
    read_buf: Vec<u8>,
    read_pos: usize,
    read_cap: usize,
}

impl<P> BufferedPort<P> {
    const DEFAULT_BUF_SIZE: usize = 8192;

    /// Create a new BufferedPort wrapping the given serial port.
    pub fn new(port: P) -> Self {
        Self::with_capacity(Self::DEFAULT_BUF_SIZE, port)
    }

    /// Create a new BufferedPort with specified read buffer capacity.
    pub fn with_capacity(read_capacity: usize, port: P) -> Self {
        Self {
            port,
            read_buf: vec![0; read_capacity],
            read_pos: 0,
            read_cap: 0,
        }
    }

    /// Get a reference to the underlying port.
    pub fn get_ref(&self) -> &P {
        &self.port
    }

    /// Get a mutable reference to the underlying port.
    pub fn get_mut(&mut self) -> &mut P {
        &mut self.port
    }

    /// Consume the BufferedPort and return the underlying port.
    pub fn into_inner(self) -> P {
        self.port
    }
}

impl<P: Read> BufferedPort<P> {
    fn fill_buf_internal(&mut self) -> io::Result<&[u8]> {
        if self.read_pos >= self.read_cap {
            self.read_cap = self.port.read(&mut self.read_buf)?;
            self.read_pos = 0;
        }
        Ok(&self.read_buf[self.read_pos..self.read_cap])
    }
}

impl<P: Read> Read for BufferedPort<P> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        // If buffer is empty, fill it
        if self.read_pos >= self.read_cap {
            // For large reads, bypass the buffer
            if buf.len() >= self.read_buf.len() {
                return self.port.read(buf);
            }
            self.read_cap = self.port.read(&mut self.read_buf)?;
            self.read_pos = 0;
        }
        // Copy from buffer to output
        let available = self.read_cap - self.read_pos;
        let to_copy = available.min(buf.len());
        buf[..to_copy].copy_from_slice(&self.read_buf[self.read_pos..self.read_pos + to_copy]);
        self.read_pos += to_copy;
        Ok(to_copy)
    }
}

impl<P: Read> BufRead for BufferedPort<P> {
    fn fill_buf(&mut self) -> io::Result<&[u8]> {
        self.fill_buf_internal()
    }

    fn consume(&mut self, amt: usize) {
        self.read_pos = (self.read_pos + amt).min(self.read_cap);
    }
}

impl<P: Write> Write for BufferedPort<P> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        // For simplicity, write directly to port (no write buffering needed for serial)
        // The underlying serial port already handles write buffering
        self.port.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.port.flush()
    }
}

// Forward SerialPort methods when available
impl<P: SerialPort> BufferedPort<P> {
    /// Set the timeout for read operations.
    pub fn set_timeout(&mut self, timeout: Duration) -> serialport::Result<()> {
        self.port.set_timeout(timeout)
    }

    /// Get the current timeout.
    pub fn timeout(&self) -> Duration {
        self.port.timeout()
    }

    /// Set the baud rate.
    pub fn set_baud_rate(&mut self, baud_rate: u32) -> serialport::Result<()> {
        self.port.set_baud_rate(baud_rate)
    }

    /// Get the current baud rate.
    pub fn baud_rate(&self) -> serialport::Result<u32> {
        self.port.baud_rate()
    }
}

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

/// Stack usage information from the UI chip.
#[derive(Debug, Clone, Default)]
pub struct StackInfoResult {
    /// Stack base address (highest address, start of stack memory).
    pub stack_base: u32,
    /// Stack top address (lowest address, end of stack memory).
    pub stack_top: u32,
    /// Total stack size in bytes.
    pub stack_size: u32,
    /// Stack usage (bytes from top to high-water mark).
    pub stack_used: u32,
}

/// Jitter buffer statistics from the NET chip.
#[derive(Debug, Clone, Default)]
pub struct JitterStatsResult {
    /// Total frames received.
    pub received: u32,
    /// Total frames output.
    pub output: u32,
    /// Number of underruns (had to output silence).
    pub underruns: u32,
    /// Number of overruns (had to drop frames).
    pub overruns: u32,
    /// Current buffer level.
    pub level: u16,
    /// Current state (0=Buffering, 1=Playing).
    pub state: u8,
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

/// Errors from CTL operations.
#[derive(Debug, thiserror::Error)]
pub enum CtlError {
    /// I/O error during communication.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    /// Received an invalid TLV type.
    #[error("invalid TLV type")]
    InvalidType,

    /// TLV value too long.
    #[error("TLV value too long")]
    TooLong,

    /// Unexpected end of stream.
    #[error("unexpected end of stream")]
    UnexpectedEof,

    /// Received unexpected TLV type (expected vs actual).
    #[error("unexpected response: expected {expected}, got {actual}")]
    UnexpectedResponse {
        expected: &'static str,
        actual: String,
    },

    /// Data mismatch (e.g., ping/pong data doesn't match).
    #[error("data mismatch")]
    DataMismatch,

    /// Invalid response length.
    #[error("invalid response length: expected {expected}, got {actual}")]
    InvalidLength { expected: usize, actual: usize },

    /// Device returned an error.
    #[error("device error: {0}")]
    DeviceError(String),

    /// Invalid UTF-8 in response.
    #[error("invalid UTF-8 in response")]
    InvalidUtf8,

    /// Invalid data format (deserialization failed).
    #[error("invalid data format")]
    InvalidData,

    /// Timeout waiting for response.
    #[error("timeout")]
    Timeout,
}

impl<E: Into<std::io::Error>> From<TlvReadError<E>> for CtlError {
    fn from(e: TlvReadError<E>) -> Self {
        match e {
            TlvReadError::Io(io) => CtlError::Io(io.into()),
            TlvReadError::InvalidType => CtlError::InvalidType,
            TlvReadError::TooLong => CtlError::TooLong,
        }
    }
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

/// Read a TLV from the UI tunnel, silently skipping any Log messages.
///
/// This function reads TLVs from the UI tunnel and silently discards any
/// `UiToMgmt::Log` messages. It continues reading until it receives
/// a non-Log TLV, which it returns.
///
/// To see Log messages, use the `ui monitor` command.
fn read_tlv_ui<R: Read>(
    reader: &mut TunnelReader<'_, R>,
) -> Result<Option<Tlv<UiToMgmt>>, TlvReadError<std::io::Error>> {
    loop {
        let tlv: Tlv<UiToMgmt> = match read_tlv(reader)? {
            Some(t) => t,
            None => return Ok(None),
        };

        if tlv.tlv_type == UiToMgmt::Log {
            // Silently skip log messages - use `ui monitor` to see them
            continue;
        }

        return Ok(Some(tlv));
    }
}

/// Write a TLV packet to a sync writer.
///
/// This function buffers the entire TLV before writing to ensure atomic delivery
/// through tunnel writers that wrap each write call.
fn write_tlv<T, W>(writer: &mut W, tlv_type: T, value: &[u8]) -> std::io::Result<()>
where
    T: Into<u16>,
    W: Write,
{
    let type_val: u16 = tlv_type.into();

    // Buffer the entire TLV packet before writing
    // This ensures TunnelWriter sends it as a single unit
    let total_len = SYNC_WORD.len() + HEADER_SIZE + value.len();
    let mut buf = vec![0u8; total_len];

    buf[..4].copy_from_slice(&SYNC_WORD);
    buf[4..6].copy_from_slice(&type_val.to_be_bytes());
    buf[6..10].copy_from_slice(&(value.len() as u32).to_be_bytes());
    buf[10..].copy_from_slice(value);

    writer.write_all(&buf)?;
    writer.flush()?;
    Ok(())
}

// ============================================================================
// Tunnel Reader/Writer
// ============================================================================

use std::sync::{Arc, Mutex, MutexGuard};

/// A shared reference to a port that can be cloned and is thread-safe.
/// Used to allow both reader and writer access to the same underlying port.
pub type SharedPort<P> = Arc<Mutex<P>>;

/// A reader that extracts data from TLV packets received through MGMT.
///
/// Buffers incoming TLV values and exposes them as a continuous byte stream.
pub struct TunnelReader<'a, P> {
    tlv_type: MgmtToCtl,
    port: &'a SharedPort<P>,
    buffer: &'a mut heapless::Vec<u8, MAX_VALUE_SIZE>,
}

impl<'a, P> TunnelReader<'a, P> {
    fn new(
        tlv_type: MgmtToCtl,
        port: &'a SharedPort<P>,
        buffer: &'a mut heapless::Vec<u8, MAX_VALUE_SIZE>,
    ) -> Self {
        Self {
            tlv_type,
            port,
            buffer,
        }
    }
}

impl<P: Read> Read for TunnelReader<'_, P> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        while self.buffer.is_empty() {
            let tlv: Tlv<MgmtToCtl> = read_tlv(&mut *self.port.lock().unwrap())
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
pub struct TunnelWriter<'a, P> {
    tlv_type: CtlToMgmt,
    port: &'a SharedPort<P>,
}

impl<'a, P> TunnelWriter<'a, P> {
    fn new(tlv_type: CtlToMgmt, port: &'a SharedPort<P>) -> Self {
        Self { tlv_type, port }
    }
}

impl<P: Write> Write for TunnelWriter<'_, P> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let to_write = core::cmp::min(MAX_VALUE_SIZE, buf.len());
        write_tlv(&mut *self.port.lock().unwrap(), self.tlv_type, &buf[..to_write])?;
        Ok(to_write)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.port.lock().unwrap().flush()
    }
}

/// A combined reader/writer for tunnel operations.
///
/// This is used for bootloader communication through the UI tunnel, where
/// we need a type that implements both Read and Write.
pub struct TunnelPort<'a, P> {
    read_tlv_type: MgmtToCtl,
    write_tlv_type: CtlToMgmt,
    port: &'a SharedPort<P>,
    buffer: heapless::Vec<u8, MAX_VALUE_SIZE>,
}

impl<'a, P> TunnelPort<'a, P> {
    /// Create a new TunnelPort for the UI tunnel.
    pub fn new_ui(port: &'a SharedPort<P>) -> Self {
        Self {
            read_tlv_type: MgmtToCtl::FromUi,
            write_tlv_type: CtlToMgmt::ToUi,
            port,
            buffer: heapless::Vec::new(),
        }
    }
}

impl<P: Read> Read for TunnelPort<'_, P> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        while self.buffer.is_empty() {
            let tlv: Tlv<MgmtToCtl> = read_tlv(&mut *self.port.lock().unwrap())
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

            if tlv.tlv_type != self.read_tlv_type {
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

impl<P: Write> Write for TunnelPort<'_, P> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let to_write = core::cmp::min(MAX_VALUE_SIZE, buf.len());
        write_tlv(&mut *self.port.lock().unwrap(), self.write_tlv_type, &buf[..to_write])?;
        Ok(to_write)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.port.lock().unwrap().flush()
    }
}

// ============================================================================
// App
// ============================================================================

/// The main application struct for communicating with the MGMT chip.
///
/// `App` wraps a serial port and provides methods for communicating with
/// MGMT, UI, and NET chips via the TLV protocol.
pub struct App<P> {
    port: SharedPort<P>,
    ui_buffer: heapless::Vec<u8, MAX_VALUE_SIZE>,
    net_buffer: heapless::Vec<u8, MAX_VALUE_SIZE>,
}

impl<P> App<P>
where
    P: Read + Write,
{
    /// Create a new App wrapping the given serial port.
    pub fn new(port: P) -> Self {
        Self {
            port: Arc::new(Mutex::new(port)),
            ui_buffer: heapless::Vec::new(),
            net_buffer: heapless::Vec::new(),
        }
    }

    /// Drain any pending data from the input buffer.
    ///
    /// This clears internal buffers and reads/discards any pending data from the
    /// underlying port. Useful before starting a new protocol that expects a
    /// clean slate (e.g., bootloader communication after reset).
    ///
    /// Note: This relies on the underlying port having a reasonable timeout
    /// (e.g., 50-100ms) so that reads will return when no data is available.
    pub fn drain(&mut self) {
        // Clear internal TLV buffers
        self.ui_buffer.clear();
        self.net_buffer.clear();

        // Read and discard any pending data from the serial port
        // With a typical 50ms timeout, this will return quickly when empty
        let mut buf = [0u8; 256];
        loop {
            match self.port.lock().unwrap().read(&mut buf) {
                Ok(0) => break,    // EOF or no data
                Ok(_) => continue, // Discard and keep reading
                Err(_) => break,   // Timeout or error - buffer is drained
            }
        }
    }

    /// Read a TLV from the MGMT connection.
    pub fn read_tlv(&mut self) -> Result<Option<Tlv<MgmtToCtl>>, TlvReadError<std::io::Error>> {
        read_tlv(&mut *self.port.lock().unwrap())
    }

    /// Write a TLV to the MGMT connection.
    pub fn write_tlv(&mut self, tlv_type: CtlToMgmt, value: &[u8]) -> std::io::Result<()> {
        write_tlv(&mut *self.port.lock().unwrap(), tlv_type, value)
    }

    /// Get a reader for the UI tunnel.
    pub fn ui_reader(&mut self) -> TunnelReader<'_, P> {
        TunnelReader::new(MgmtToCtl::FromUi, &self.port, &mut self.ui_buffer)
    }

    /// Get a reader for the NET tunnel.
    pub fn net_reader(&mut self) -> TunnelReader<'_, P> {
        TunnelReader::new(MgmtToCtl::FromNet, &self.port, &mut self.net_buffer)
    }

    /// Get a writer for the UI tunnel (TLV protocol).
    pub fn ui_writer(&self) -> TunnelWriter<'_, P> {
        TunnelWriter::new(CtlToMgmt::ToUi, &self.port)
    }

    /// Get a writer for the NET tunnel.
    pub fn net_writer(&self) -> TunnelWriter<'_, P> {
        TunnelWriter::new(CtlToMgmt::ToNet, &self.port)
    }

    /// Get a mutable reference to the underlying port.
    ///
    /// This is useful for operations that need to modify the underlying port,
    /// such as setting the timeout or baud rate on a serial port.
    ///
    /// # Panics
    /// Panics if the port is currently locked elsewhere.
    pub fn port_mut(&self) -> MutexGuard<'_, P> {
        self.port.lock().unwrap()
    }

    /// Get the shared port reference for passing to other APIs.
    pub fn shared_port(&self) -> SharedPort<P> {
        self.port.clone()
    }

    /// Read a Log message from the UI chip.
    ///
    /// This uses the TunnelReader which buffers FromUi TLV values and handles
    /// batching/splitting correctly. Non-FromUi TLVs (like FromNet) are skipped
    /// by the TunnelReader.
    ///
    /// Returns `Ok(Some(message))` if a Log TLV was received,
    /// `Ok(None)` if timeout or non-Log TLV was received,
    /// or an error if reading failed.
    pub fn read_ui_log(&mut self) -> Result<Option<String>, TlvReadError<std::io::Error>> {
        // Use TunnelReader which handles buffering and skips non-FromUi TLVs
        let tlv: Tlv<UiToMgmt> = match read_tlv(&mut self.ui_reader())? {
            Some(t) => t,
            None => return Ok(None),
        };

        if tlv.tlv_type == UiToMgmt::Log {
            match core::str::from_utf8(&tlv.value) {
                Ok(msg) => Ok(Some(msg.to_string())),
                Err(_) => Ok(Some(format!("<invalid utf8: {:?}>", tlv.value.as_slice()))),
            }
        } else {
            // Non-log UI TLV - discard it
            Ok(None)
        }
    }

    /// Ping the MGMT chip directly.
    ///
    /// Skips any FromNet/FromUi TLVs that may be pending before the Pong.
    pub fn mgmt_ping(&mut self, data: &[u8]) -> Result<(), CtlError> {
        write_tlv(&mut *self.port.lock().unwrap(), CtlToMgmt::Ping, data)?;
        loop {
            let tlv: Tlv<MgmtToCtl> = read_tlv(&mut *self.port.lock().unwrap())?.ok_or(CtlError::UnexpectedEof)?;
            match tlv.tlv_type {
                MgmtToCtl::FromUi | MgmtToCtl::FromNet => continue, // Skip tunneled messages
                MgmtToCtl::Pong => {
                    if tlv.value.as_slice() != data {
                        return Err(CtlError::DataMismatch);
                    }
                    return Ok(());
                }
                other => {
                    return Err(CtlError::UnexpectedResponse {
                        expected: "Pong",
                        actual: format!("{:?}", other),
                    });
                }
            }
        }
    }

    /// Get MGMT chip stack usage information.
    pub fn mgmt_get_stack_info(&mut self) -> Result<StackInfoResult, CtlError> {
        write_tlv(&mut *self.port.lock().unwrap(), CtlToMgmt::GetStackInfo, &[])?;
        loop {
            let tlv: Tlv<MgmtToCtl> = read_tlv(&mut *self.port.lock().unwrap())?.ok_or(CtlError::UnexpectedEof)?;
            match tlv.tlv_type {
                MgmtToCtl::FromUi | MgmtToCtl::FromNet => continue, // Skip tunneled messages
                MgmtToCtl::StackInfo => {
                    if tlv.value.len() != 16 {
                        return Err(CtlError::InvalidLength {
                            expected: 16,
                            actual: tlv.value.len(),
                        });
                    }
                    return Ok(StackInfoResult {
                        stack_base: u32::from_le_bytes([tlv.value[0], tlv.value[1], tlv.value[2], tlv.value[3]]),
                        stack_top: u32::from_le_bytes([tlv.value[4], tlv.value[5], tlv.value[6], tlv.value[7]]),
                        stack_size: u32::from_le_bytes([tlv.value[8], tlv.value[9], tlv.value[10], tlv.value[11]]),
                        stack_used: u32::from_le_bytes([tlv.value[12], tlv.value[13], tlv.value[14], tlv.value[15]]),
                    });
                }
                other => {
                    return Err(CtlError::UnexpectedResponse {
                        expected: "StackInfo",
                        actual: format!("{:?}", other),
                    });
                }
            }
        }
    }

    /// Repaint the MGMT chip stack for future measurement.
    pub fn mgmt_repaint_stack(&mut self) -> Result<(), CtlError> {
        write_tlv(&mut *self.port.lock().unwrap(), CtlToMgmt::RepaintStack, &[])?;
        loop {
            let tlv: Tlv<MgmtToCtl> = read_tlv(&mut *self.port.lock().unwrap())?.ok_or(CtlError::UnexpectedEof)?;
            match tlv.tlv_type {
                MgmtToCtl::FromUi | MgmtToCtl::FromNet => continue, // Skip tunneled messages
                MgmtToCtl::Ack => return Ok(()),
                other => {
                    return Err(CtlError::UnexpectedResponse {
                        expected: "Ack",
                        actual: format!("{:?}", other),
                    });
                }
            }
        }
    }

    /// Send a Hello handshake to detect if a valid device is connected.
    ///
    /// Sends a 4-byte challenge value and verifies the response is the challenge
    /// XOR'd with b"LINK". Returns true if the handshake succeeded.
    pub fn hello(&mut self, challenge: &[u8; 4]) -> bool {
        const MAGIC: &[u8; 4] = b"LINK";

        if write_tlv(&mut *self.port.lock().unwrap(), CtlToMgmt::Hello, challenge).is_err() {
            return false;
        }

        let tlv: Tlv<MgmtToCtl> = match read_tlv(&mut *self.port.lock().unwrap()) {
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

    /// Ping the UI chip through the MGMT tunnel.
    pub fn ui_ping(&mut self, data: &[u8]) -> Result<(), CtlError> {
        write_tlv(&mut self.ui_writer(), MgmtToUi::Ping, data)?;
        let tlv: Tlv<UiToMgmt> =
            read_tlv_ui(&mut self.ui_reader())?.ok_or(CtlError::UnexpectedEof)?;
        if tlv.tlv_type != UiToMgmt::Pong {
            return Err(CtlError::UnexpectedResponse {
                expected: "Pong",
                actual: format!("{:?}", tlv.tlv_type),
            });
        }
        if tlv.value.as_slice() != data {
            return Err(CtlError::DataMismatch);
        }
        Ok(())
    }

    /// Ping the NET chip through the MGMT tunnel.
    pub fn net_ping(&mut self, data: &[u8]) -> Result<(), CtlError> {
        write_tlv(&mut self.net_writer(), MgmtToNet::Ping, data)?;
        let tlv: Tlv<NetToMgmt> =
            read_tlv(&mut self.net_reader())?.ok_or(CtlError::UnexpectedEof)?;
        if tlv.tlv_type != NetToMgmt::Pong {
            return Err(CtlError::UnexpectedResponse {
                expected: "Pong",
                actual: format!("{:?}", tlv.tlv_type),
            });
        }
        if tlv.value.as_slice() != data {
            return Err(CtlError::DataMismatch);
        }
        Ok(())
    }

    /// Send a circular ping starting from UI (UI -> NET -> MGMT -> CTL).
    pub fn ui_first_circular_ping(&mut self, data: &[u8]) -> Result<(), CtlError> {
        write_tlv(&mut self.ui_writer(), MgmtToUi::CircularPing, data)?;
        let tlv: Tlv<NetToMgmt> =
            read_tlv(&mut self.net_reader())?.ok_or(CtlError::UnexpectedEof)?;
        if tlv.tlv_type != NetToMgmt::CircularPing {
            return Err(CtlError::UnexpectedResponse {
                expected: "CircularPing",
                actual: format!("{:?}", tlv.tlv_type),
            });
        }
        if tlv.value.as_slice() != data {
            return Err(CtlError::DataMismatch);
        }
        Ok(())
    }

    /// Send a circular ping starting from NET (NET -> UI -> MGMT -> CTL).
    pub fn net_first_circular_ping(&mut self, data: &[u8]) -> Result<(), CtlError> {
        write_tlv(&mut self.net_writer(), MgmtToNet::CircularPing, data)?;
        let tlv: Tlv<UiToMgmt> =
            read_tlv_ui(&mut self.ui_reader())?.ok_or(CtlError::UnexpectedEof)?;
        if tlv.tlv_type != UiToMgmt::CircularPing {
            return Err(CtlError::UnexpectedResponse {
                expected: "CircularPing",
                actual: format!("{:?}", tlv.tlv_type),
            });
        }
        if tlv.value.as_slice() != data {
            return Err(CtlError::DataMismatch);
        }
        Ok(())
    }

    /// Get the version stored in UI chip EEPROM.
    pub fn get_version(&mut self) -> Result<u32, CtlError> {
        write_tlv(&mut self.ui_writer(), MgmtToUi::GetVersion, &[])?;
        let tlv: Tlv<UiToMgmt> =
            read_tlv_ui(&mut self.ui_reader())?.ok_or(CtlError::UnexpectedEof)?;
        if tlv.tlv_type != UiToMgmt::Version {
            return Err(CtlError::UnexpectedResponse {
                expected: "Version",
                actual: format!("{:?}", tlv.tlv_type),
            });
        }
        if tlv.value.len() != 4 {
            return Err(CtlError::InvalidLength {
                expected: 4,
                actual: tlv.value.len(),
            });
        }
        Ok(u32::from_be_bytes([
            tlv.value[0],
            tlv.value[1],
            tlv.value[2],
            tlv.value[3],
        ]))
    }

    /// Set the version stored in UI chip EEPROM.
    pub fn set_version(&mut self, version: u32) -> Result<(), CtlError> {
        write_tlv(
            &mut self.ui_writer(),
            MgmtToUi::SetVersion,
            &version.to_be_bytes(),
        )?;
        let tlv: Tlv<UiToMgmt> =
            read_tlv_ui(&mut self.ui_reader())?.ok_or(CtlError::UnexpectedEof)?;
        if tlv.tlv_type != UiToMgmt::Ack {
            return Err(CtlError::UnexpectedResponse {
                expected: "Ack",
                actual: format!("{:?}", tlv.tlv_type),
            });
        }
        Ok(())
    }

    /// Get the SFrame key stored in UI chip EEPROM.
    pub fn get_sframe_key(&mut self) -> Result<[u8; 16], CtlError> {
        write_tlv(&mut self.ui_writer(), MgmtToUi::GetSFrameKey, &[])?;
        let tlv: Tlv<UiToMgmt> =
            read_tlv_ui(&mut self.ui_reader())?.ok_or(CtlError::UnexpectedEof)?;
        if tlv.tlv_type != UiToMgmt::SFrameKey {
            return Err(CtlError::UnexpectedResponse {
                expected: "SFrameKey",
                actual: format!("{:?}", tlv.tlv_type),
            });
        }
        if tlv.value.len() != 16 {
            return Err(CtlError::InvalidLength {
                expected: 16,
                actual: tlv.value.len(),
            });
        }
        let mut key = [0u8; 16];
        key.copy_from_slice(&tlv.value);
        Ok(key)
    }

    /// Set the SFrame key stored in UI chip EEPROM.
    pub fn set_sframe_key(&mut self, key: &[u8; 16]) -> Result<(), CtlError> {
        write_tlv(&mut self.ui_writer(), MgmtToUi::SetSFrameKey, key)?;
        let tlv: Tlv<UiToMgmt> =
            read_tlv_ui(&mut self.ui_reader())?.ok_or(CtlError::UnexpectedEof)?;
        if tlv.tlv_type != UiToMgmt::Ack {
            return Err(CtlError::UnexpectedResponse {
                expected: "Ack",
                actual: format!("{:?}", tlv.tlv_type),
            });
        }
        Ok(())
    }

    /// Set UI chip loopback mode.
    pub fn ui_set_loopback(&mut self, mode: LoopbackMode) -> Result<(), CtlError> {
        write_tlv(
            &mut self.ui_writer(),
            MgmtToUi::SetLoopback,
            &[mode as u8],
        )?;
        let tlv: Tlv<UiToMgmt> =
            read_tlv_ui(&mut self.ui_reader())?.ok_or(CtlError::UnexpectedEof)?;
        if tlv.tlv_type != UiToMgmt::Ack {
            return Err(CtlError::UnexpectedResponse {
                expected: "Ack",
                actual: format!("{:?}", tlv.tlv_type),
            });
        }
        Ok(())
    }

    /// Get UI chip loopback mode.
    pub fn ui_get_loopback(&mut self) -> Result<LoopbackMode, CtlError> {
        write_tlv(&mut self.ui_writer(), MgmtToUi::GetLoopback, &[])?;
        let tlv: Tlv<UiToMgmt> =
            read_tlv_ui(&mut self.ui_reader())?.ok_or(CtlError::UnexpectedEof)?;
        if tlv.tlv_type != UiToMgmt::Loopback {
            return Err(CtlError::UnexpectedResponse {
                expected: "Loopback",
                actual: format!("{:?}", tlv.tlv_type),
            });
        }
        let mode_byte = tlv.value.first().copied().unwrap_or(0);
        Ok(LoopbackMode::try_from(mode_byte).unwrap_or(LoopbackMode::Off))
    }

    /// Get UI chip stack usage information.
    pub fn ui_get_stack_info(&mut self) -> Result<StackInfoResult, CtlError> {
        write_tlv(&mut self.ui_writer(), MgmtToUi::GetStackInfo, &[])?;
        let tlv: Tlv<UiToMgmt> =
            read_tlv_ui(&mut self.ui_reader())?.ok_or(CtlError::UnexpectedEof)?;
        if tlv.tlv_type == UiToMgmt::Error {
            let msg = core::str::from_utf8(&tlv.value).unwrap_or("unknown error");
            return Err(CtlError::DeviceError(msg.to_string()));
        }
        if tlv.tlv_type != UiToMgmt::StackInfo {
            return Err(CtlError::UnexpectedResponse {
                expected: "StackInfo",
                actual: format!("{:?}", tlv.tlv_type),
            });
        }
        if tlv.value.len() != 16 {
            return Err(CtlError::InvalidLength {
                expected: 16,
                actual: tlv.value.len(),
            });
        }
        Ok(StackInfoResult {
            stack_base: u32::from_le_bytes([tlv.value[0], tlv.value[1], tlv.value[2], tlv.value[3]]),
            stack_top: u32::from_le_bytes([tlv.value[4], tlv.value[5], tlv.value[6], tlv.value[7]]),
            stack_size: u32::from_le_bytes([tlv.value[8], tlv.value[9], tlv.value[10], tlv.value[11]]),
            stack_used: u32::from_le_bytes([tlv.value[12], tlv.value[13], tlv.value[14], tlv.value[15]]),
        })
    }

    /// Repaint the UI chip stack for future measurement.
    pub fn ui_repaint_stack(&mut self) -> Result<(), CtlError> {
        write_tlv(&mut self.ui_writer(), MgmtToUi::RepaintStack, &[])?;
        let tlv: Tlv<UiToMgmt> =
            read_tlv_ui(&mut self.ui_reader())?.ok_or(CtlError::UnexpectedEof)?;
        if tlv.tlv_type == UiToMgmt::Error {
            let msg = core::str::from_utf8(&tlv.value).unwrap_or("unknown error");
            return Err(CtlError::DeviceError(msg.to_string()));
        }
        if tlv.tlv_type != UiToMgmt::Ack {
            return Err(CtlError::UnexpectedResponse {
                expected: "Ack",
                actual: format!("{:?}", tlv.tlv_type),
            });
        }
        Ok(())
    }

    /// Set NET chip loopback mode.
    /// - Off: Normal PTT operation, filter self-echo
    /// - Raw: Local bypass, audio directly back to UI (no MoQ)
    /// - Moq: MoQ loopback, hear own audio via relay
    pub fn net_set_loopback(&mut self, mode: NetLoopback) -> Result<(), CtlError> {
        write_tlv(
            &mut self.net_writer(),
            MgmtToNet::SetLoopback,
            &[mode as u8],
        )?;
        let tlv: Tlv<NetToMgmt> =
            read_tlv(&mut self.net_reader())?.ok_or(CtlError::UnexpectedEof)?;
        if tlv.tlv_type != NetToMgmt::Ack {
            return Err(CtlError::UnexpectedResponse {
                expected: "Ack",
                actual: format!("{:?}", tlv.tlv_type),
            });
        }
        Ok(())
    }

    /// Get NET chip loopback mode.
    pub fn net_get_loopback(&mut self) -> Result<NetLoopback, CtlError> {
        write_tlv(&mut self.net_writer(), MgmtToNet::GetLoopback, &[])?;
        let tlv: Tlv<NetToMgmt> =
            read_tlv(&mut self.net_reader())?.ok_or(CtlError::UnexpectedEof)?;
        if tlv.tlv_type != NetToMgmt::Loopback {
            return Err(CtlError::UnexpectedResponse {
                expected: "Loopback",
                actual: format!("{:?}", tlv.tlv_type),
            });
        }
        let mode_byte = tlv.value.first().copied().unwrap_or(0);
        Ok(NetLoopback::try_from(mode_byte).unwrap_or(NetLoopback::Off))
    }

    /// Add a WiFi SSID and password pair to NET chip storage.
    pub fn add_wifi_ssid(&mut self, ssid: &str, password: &str) -> Result<(), CtlError> {
        let wifi = WifiSsid {
            ssid: ssid.try_into().map_err(|_| CtlError::TooLong)?,
            password: password.try_into().map_err(|_| CtlError::TooLong)?,
        };
        let mut buf = [0u8; 128];
        let serialized = postcard::to_slice(&wifi, &mut buf).map_err(|_| CtlError::TooLong)?;
        write_tlv(&mut self.net_writer(), MgmtToNet::AddWifiSsid, serialized)?;
        let tlv: Tlv<NetToMgmt> =
            read_tlv(&mut self.net_reader())?.ok_or(CtlError::UnexpectedEof)?;
        if tlv.tlv_type != NetToMgmt::Ack {
            return Err(CtlError::UnexpectedResponse {
                expected: "Ack",
                actual: format!("{:?}", tlv.tlv_type),
            });
        }
        Ok(())
    }

    /// Get all WiFi SSIDs from NET chip storage.
    pub fn get_wifi_ssids(&mut self) -> Result<heapless::Vec<WifiSsid, 8>, CtlError> {
        write_tlv(&mut self.net_writer(), MgmtToNet::GetWifiSsids, &[])?;
        let tlv: Tlv<NetToMgmt> =
            read_tlv(&mut self.net_reader())?.ok_or(CtlError::UnexpectedEof)?;
        if tlv.tlv_type != NetToMgmt::WifiSsids {
            return Err(CtlError::UnexpectedResponse {
                expected: "WifiSsids",
                actual: format!("{:?}", tlv.tlv_type),
            });
        }
        postcard::from_bytes(&tlv.value).map_err(|_| CtlError::InvalidUtf8)
    }

    /// Clear all WiFi SSIDs from NET chip storage.
    pub fn clear_wifi_ssids(&mut self) -> Result<(), CtlError> {
        write_tlv(&mut self.net_writer(), MgmtToNet::ClearWifiSsids, &[])?;
        let tlv: Tlv<NetToMgmt> =
            read_tlv(&mut self.net_reader())?.ok_or(CtlError::UnexpectedEof)?;
        if tlv.tlv_type != NetToMgmt::Ack {
            return Err(CtlError::UnexpectedResponse {
                expected: "Ack",
                actual: format!("{:?}", tlv.tlv_type),
            });
        }
        Ok(())
    }

    /// Get the relay URL from NET chip storage.
    pub fn get_relay_url(&mut self) -> Result<heapless::String<128>, CtlError> {
        write_tlv(&mut self.net_writer(), MgmtToNet::GetRelayUrl, &[])?;
        let tlv: Tlv<NetToMgmt> =
            read_tlv(&mut self.net_reader())?.ok_or(CtlError::UnexpectedEof)?;
        if tlv.tlv_type != NetToMgmt::RelayUrl {
            return Err(CtlError::UnexpectedResponse {
                expected: "RelayUrl",
                actual: format!("{:?}", tlv.tlv_type),
            });
        }
        let url_str = core::str::from_utf8(&tlv.value).map_err(|_| CtlError::InvalidUtf8)?;
        url_str.try_into().map_err(|_| CtlError::TooLong)
    }

    /// Set the relay URL in NET chip storage.
    pub fn set_relay_url(&mut self, url: &str) -> Result<(), CtlError> {
        write_tlv(
            &mut self.net_writer(),
            MgmtToNet::SetRelayUrl,
            url.as_bytes(),
        )?;
        let tlv: Tlv<NetToMgmt> =
            read_tlv(&mut self.net_reader())?.ok_or(CtlError::UnexpectedEof)?;
        if tlv.tlv_type != NetToMgmt::Ack {
            return Err(CtlError::UnexpectedResponse {
                expected: "Ack",
                actual: format!("{:?}", tlv.tlv_type),
            });
        }
        Ok(())
    }

    /// Get configuration for a specific channel.
    ///
    /// Returns the channel configuration, or a default (disabled) config
    /// if the channel hasn't been configured.
    pub fn get_channel_config(&mut self, channel_id: u8) -> Result<ChannelConfig, CtlError> {
        write_tlv(
            &mut self.net_writer(),
            MgmtToNet::GetChannelConfig,
            &[channel_id],
        )?;
        let tlv: Tlv<NetToMgmt> =
            read_tlv(&mut self.net_reader())?.ok_or(CtlError::UnexpectedEof)?;
        if tlv.tlv_type != NetToMgmt::ChannelConfig {
            return Err(CtlError::UnexpectedResponse {
                expected: "ChannelConfig",
                actual: format!("{:?}", tlv.tlv_type),
            });
        }
        postcard::from_bytes(&tlv.value).map_err(|_| CtlError::InvalidData)
    }

    /// Set configuration for a channel.
    ///
    /// Replaces existing config for that channel_id or adds new one.
    pub fn set_channel_config(&mut self, config: &ChannelConfig) -> Result<(), CtlError> {
        let mut buf = [0u8; 256];
        let serialized = postcard::to_slice(config, &mut buf).map_err(|_| CtlError::TooLong)?;
        write_tlv(&mut self.net_writer(), MgmtToNet::SetChannelConfig, serialized)?;
        let tlv: Tlv<NetToMgmt> =
            read_tlv(&mut self.net_reader())?.ok_or(CtlError::UnexpectedEof)?;
        if tlv.tlv_type != NetToMgmt::Ack {
            return Err(CtlError::UnexpectedResponse {
                expected: "Ack",
                actual: format!("{:?}", tlv.tlv_type),
            });
        }
        Ok(())
    }

    /// Get all channel configurations.
    pub fn get_all_channel_configs(
        &mut self,
    ) -> Result<heapless::Vec<ChannelConfig, MAX_CHANNELS>, CtlError> {
        write_tlv(&mut self.net_writer(), MgmtToNet::GetAllChannelConfigs, &[])?;
        let tlv: Tlv<NetToMgmt> =
            read_tlv(&mut self.net_reader())?.ok_or(CtlError::UnexpectedEof)?;
        if tlv.tlv_type != NetToMgmt::AllChannelConfigs {
            return Err(CtlError::UnexpectedResponse {
                expected: "AllChannelConfigs",
                actual: format!("{:?}", tlv.tlv_type),
            });
        }
        postcard::from_bytes(&tlv.value).map_err(|_| CtlError::InvalidData)
    }

    /// Clear all channel configurations.
    pub fn clear_channel_configs(&mut self) -> Result<(), CtlError> {
        write_tlv(&mut self.net_writer(), MgmtToNet::ClearChannelConfigs, &[])?;
        let tlv: Tlv<NetToMgmt> =
            read_tlv(&mut self.net_reader())?.ok_or(CtlError::UnexpectedEof)?;
        if tlv.tlv_type != NetToMgmt::Ack {
            return Err(CtlError::UnexpectedResponse {
                expected: "Ack",
                actual: format!("{:?}", tlv.tlv_type),
            });
        }
        Ok(())
    }

    /// Get jitter buffer statistics for a channel.
    ///
    /// Only available when the NET chip is built with the `audio-buffer` feature.
    pub fn get_jitter_stats(&mut self, channel_id: u8) -> Result<JitterStatsResult, CtlError> {
        write_tlv(
            &mut self.net_writer(),
            MgmtToNet::GetJitterStats,
            &[channel_id],
        )?;
        let tlv: Tlv<NetToMgmt> =
            read_tlv(&mut self.net_reader())?.ok_or(CtlError::UnexpectedEof)?;
        if tlv.tlv_type != NetToMgmt::JitterStats {
            return Err(CtlError::UnexpectedResponse {
                expected: "JitterStats",
                actual: format!("{:?}", tlv.tlv_type),
            });
        }
        // Parse: received(4) + output(4) + underruns(4) + overruns(4) + level(2) + state(1) = 19 bytes
        if tlv.value.len() < 19 {
            return Err(CtlError::InvalidData);
        }
        let received = u32::from_le_bytes([
            tlv.value[0],
            tlv.value[1],
            tlv.value[2],
            tlv.value[3],
        ]);
        let output = u32::from_le_bytes([
            tlv.value[4],
            tlv.value[5],
            tlv.value[6],
            tlv.value[7],
        ]);
        let underruns = u32::from_le_bytes([
            tlv.value[8],
            tlv.value[9],
            tlv.value[10],
            tlv.value[11],
        ]);
        let overruns = u32::from_le_bytes([
            tlv.value[12],
            tlv.value[13],
            tlv.value[14],
            tlv.value[15],
        ]);
        let level = u16::from_le_bytes([tlv.value[16], tlv.value[17]]);
        let state = tlv.value[18];
        Ok(JitterStatsResult {
            received,
            output,
            underruns,
            overruns,
            level,
            state,
        })
    }

    /// Send data over WebSocket and verify echo response.
    ///
    /// This sends data to the relay server via WebSocket and expects the same
    /// data back (assumes an echo server). Useful for testing WS connectivity.
    pub fn ws_ping(&mut self, data: &[u8]) -> Result<(), CtlError> {
        write_tlv(&mut self.net_writer(), MgmtToNet::WsSend, data)?;
        let tlv: Tlv<NetToMgmt> =
            read_tlv(&mut self.net_reader())?.ok_or(CtlError::UnexpectedEof)?;
        if tlv.tlv_type != NetToMgmt::WsReceived {
            return Err(CtlError::UnexpectedResponse {
                expected: "WsReceived",
                actual: format!("{:?}", tlv.tlv_type),
            });
        }
        if tlv.value.as_slice() != data {
            return Err(CtlError::DataMismatch);
        }
        Ok(())
    }

    /// Run WebSocket echo test to measure bidirectional throughput.
    ///
    /// This test:
    /// 1. Sends 50 packets (160 bytes each) at 20ms intervals (50 fps)
    /// 2. Expects the echo server to return each packet
    /// 3. Measures jitter before and after the jitter buffer
    ///
    /// Returns EchoTestResults with raw and buffered jitter measurements.
    pub fn ws_echo_test(&mut self) -> Result<EchoTestResults, CtlError> {
        // Tunnel through MGMT to NET (like ws_ping does)
        write_tlv(&mut self.net_writer(), MgmtToNet::WsEchoTest, &[])?;

        // Wait for result from NET (tunneled through MGMT as FromNet)
        let tlv: Tlv<NetToMgmt> =
            read_tlv(&mut self.net_reader())?.ok_or(CtlError::UnexpectedEof)?;
        if tlv.tlv_type != NetToMgmt::WsEchoTestResult {
            return Err(CtlError::UnexpectedResponse {
                expected: "WsEchoTestResult",
                actual: format!("{:?}", tlv.tlv_type),
            });
        }

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

        Ok(EchoTestResults {
            sent,
            received,
            buffered_output,
            underruns,
            raw_jitter_us,
            buffered_jitter_us,
        })
    }

    /// Run a WebSocket speed test.
    ///
    /// This sends 50 packets as fast as possible (no delay between sends),
    /// then waits up to 2 seconds to receive responses.
    ///
    /// Returns SpeedTestResults with timing information.
    pub fn ws_speed_test(&mut self) -> Result<SpeedTestResults, CtlError> {
        // Tunnel through MGMT to NET
        write_tlv(&mut self.net_writer(), MgmtToNet::WsSpeedTest, &[])?;

        // Wait for result from NET (tunneled through MGMT as FromNet)
        let tlv: Tlv<NetToMgmt> =
            read_tlv(&mut self.net_reader())?.ok_or(CtlError::UnexpectedEof)?;
        if tlv.tlv_type != NetToMgmt::WsSpeedTestResult {
            return Err(CtlError::UnexpectedResponse {
                expected: "WsSpeedTestResult",
                actual: format!("{:?}", tlv.tlv_type),
            });
        }

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

        Ok(SpeedTestResults {
            sent,
            received,
            send_time_ms,
            recv_time_ms,
        })
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
        self.drain();

        let mut port_guard = self.port.lock().unwrap();
        let mut bl = Bootloader::new(&mut *port_guard);

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
        self.drain();

        let mut port_guard = self.port.lock().unwrap();
        let mut bl = Bootloader::new(&mut *port_guard);

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
    pub fn reset_ui_to_bootloader(&mut self) -> Result<(), CtlError> {
        write_tlv(&mut *self.port.lock().unwrap(), CtlToMgmt::ResetUiToBootloader, &[])?;
        let tlv: Tlv<MgmtToCtl> = read_tlv(&mut *self.port.lock().unwrap())?.ok_or(CtlError::UnexpectedEof)?;
        if tlv.tlv_type != MgmtToCtl::Ack {
            return Err(CtlError::UnexpectedResponse {
                expected: "Ack",
                actual: format!("{:?}", tlv.tlv_type),
            });
        }
        Ok(())
    }

    /// Reset the UI chip into user mode (normal operation).
    ///
    /// Sends a command to MGMT which toggles the BOOT0 and RST pins
    /// to put the UI chip back into normal user mode.
    pub fn reset_ui_to_user(&mut self) -> Result<(), CtlError> {
        write_tlv(&mut *self.port.lock().unwrap(), CtlToMgmt::ResetUiToUser, &[])?;
        let tlv: Tlv<MgmtToCtl> = read_tlv(&mut *self.port.lock().unwrap())?.ok_or(CtlError::UnexpectedEof)?;
        if tlv.tlv_type != MgmtToCtl::Ack {
            return Err(CtlError::UnexpectedResponse {
                expected: "Ack",
                actual: format!("{:?}", tlv.tlv_type),
            });
        }
        Ok(())
    }

    /// Hold the UI chip in reset.
    ///
    /// Sends a command to MGMT to assert the RST pin low, keeping the
    /// UI chip in reset until released.
    pub fn hold_ui_reset(&mut self) -> Result<(), CtlError> {
        write_tlv(&mut *self.port.lock().unwrap(), CtlToMgmt::HoldUiReset, &[])?;
        let tlv: Tlv<MgmtToCtl> = read_tlv(&mut *self.port.lock().unwrap())?.ok_or(CtlError::UnexpectedEof)?;
        if tlv.tlv_type != MgmtToCtl::Ack {
            return Err(CtlError::UnexpectedResponse {
                expected: "Ack",
                actual: format!("{:?}", tlv.tlv_type),
            });
        }
        Ok(())
    }

    /// Hold the NET chip in reset.
    ///
    /// Sends a command to MGMT to assert the RST pin low, keeping the
    /// NET chip in reset until released.
    pub fn hold_net_reset(&mut self) -> Result<(), CtlError> {
        write_tlv(&mut *self.port.lock().unwrap(), CtlToMgmt::HoldNetReset, &[])?;
        let tlv: Tlv<MgmtToCtl> = read_tlv(&mut *self.port.lock().unwrap())?.ok_or(CtlError::UnexpectedEof)?;
        if tlv.tlv_type != MgmtToCtl::Ack {
            return Err(CtlError::UnexpectedResponse {
                expected: "Ack",
                actual: format!("{:?}", tlv.tlv_type),
            });
        }
        Ok(())
    }

    /// Reset the NET chip into bootloader mode.
    ///
    /// Sends a command to MGMT which toggles the BOOT0 and RST pins
    /// to put the NET chip into bootloader mode.
    pub fn reset_net_to_bootloader(&mut self) -> Result<(), CtlError> {
        eprintln!("[debug] Sending ResetNetToBootloader command to MGMT...");
        write_tlv(&mut *self.port.lock().unwrap(), CtlToMgmt::ResetNetToBootloader, &[])?;
        // Read TLVs, skipping any FromNet (boot messages from NET chip) until we get the Ack
        for i in 0..100 {
            eprintln!("[trace] Waiting for Ack (attempt {})", i);
            let tlv: Tlv<MgmtToCtl> = read_tlv(&mut *self.port.lock().unwrap())?.ok_or(CtlError::UnexpectedEof)?;
            eprintln!("[trace] Received TLV: {:?}", tlv.tlv_type);
            match tlv.tlv_type {
                MgmtToCtl::Ack => {
                    eprintln!("[debug] Received Ack from MGMT");
                    return Ok(());
                }
                MgmtToCtl::FromNet => {
                    eprintln!("[trace] Skipping FromNet TLV ({} bytes)", tlv.value.len());
                    continue;
                }
                other => {
                    return Err(CtlError::UnexpectedResponse {
                        expected: "Ack",
                        actual: format!("{:?}", other),
                    });
                }
            }
        }
        Err(CtlError::Timeout)
    }

    /// Reset the NET chip into user mode (normal operation).
    ///
    /// Sends a command to MGMT which toggles the BOOT0 and RST pins
    /// to put the NET chip back into normal user mode.
    pub fn reset_net_to_user(&mut self) -> Result<(), CtlError> {
        write_tlv(&mut *self.port.lock().unwrap(), CtlToMgmt::ResetNetToUser, &[])?;
        // Read TLVs, skipping any FromNet (boot messages from NET chip) until we get the Ack
        for _ in 0..100 {
            let tlv: Tlv<MgmtToCtl> = read_tlv(&mut *self.port.lock().unwrap())?.ok_or(CtlError::UnexpectedEof)?;
            match tlv.tlv_type {
                MgmtToCtl::Ack => return Ok(()),
                MgmtToCtl::FromNet => continue,
                other => {
                    return Err(CtlError::UnexpectedResponse {
                        expected: "Ack",
                        actual: format!("{:?}", other),
                    });
                }
            }
        }
        Err(CtlError::Timeout)
    }

    /// Set the NET UART baud rate on the MGMT chip.
    ///
    /// This changes the baud rate between MGMT and NET chips.
    /// The change takes effect immediately after MGMT processes the command.
    pub fn set_net_baud_rate(&mut self, baud_rate: u32) -> Result<(), CtlError> {
        write_tlv(
            &mut *self.port.lock().unwrap(),
            CtlToMgmt::SetNetBaudRate,
            &baud_rate.to_le_bytes(),
        )?;
        loop {
            let tlv: Tlv<MgmtToCtl> = read_tlv(&mut *self.port.lock().unwrap())?.ok_or(CtlError::UnexpectedEof)?;
            match tlv.tlv_type {
                MgmtToCtl::Ack => return Ok(()),
                MgmtToCtl::FromNet | MgmtToCtl::FromUi => continue,
                other => {
                    return Err(CtlError::UnexpectedResponse {
                        expected: "Ack",
                        actual: format!("{:?}", other),
                    });
                }
            }
        }
    }

    /// Set the CTL UART baud rate on the MGMT chip.
    ///
    /// This changes the baud rate between CTL and MGMT.
    /// IMPORTANT: The ACK is sent at the old baud rate before the change takes effect.
    /// After calling this, the caller must change their own serial port baud rate
    /// to match before continuing communication.
    pub fn set_ctl_baud_rate(&mut self, baud_rate: u32) -> Result<(), CtlError> {
        write_tlv(
            &mut *self.port.lock().unwrap(),
            CtlToMgmt::SetCtlBaudRate,
            &baud_rate.to_le_bytes(),
        )?;
        // Read ACK at current baud rate (before MGMT switches)
        loop {
            let tlv: Tlv<MgmtToCtl> = read_tlv(&mut *self.port.lock().unwrap())?.ok_or(CtlError::UnexpectedEof)?;
            match tlv.tlv_type {
                MgmtToCtl::Ack => return Ok(()),
                MgmtToCtl::FromNet | MgmtToCtl::FromUi => continue,
                other => {
                    return Err(CtlError::UnexpectedResponse {
                        expected: "Ack",
                        actual: format!("{:?}", other),
                    });
                }
            }
        }
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
        let _ = self.reset_ui_to_bootloader();

        // Wait for bootloader to be ready
        delay_ms(1000);

        // Query bootloader info, capturing any error
        let result = self.query_ui_bootloader();

        // Always reset UI chip back to user mode
        let _ = self.reset_ui_to_user();

        result
    }

    /// Helper to query the UI bootloader. Separated so borrows are released before reset.
    fn query_ui_bootloader(&mut self) -> Result<MgmtBootloaderInfo, stm::Error<std::io::Error>> {
        // Create a bootloader client using the tunneled UI connection
        let mut ui_tunnel = TunnelPort::new_ui(&self.port);
        let mut bl = Bootloader::new(&mut ui_tunnel);

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
        verify: bool,
        mut progress: F,
    ) -> Result<(), FlashError<std::io::Error>>
    where
        F: FnMut(FlashPhase, usize, usize),
        D: FnOnce(u64),
    {
        // Reset UI chip into bootloader mode
        let _ = self.reset_ui_to_bootloader();

        // Wait for bootloader to be ready
        delay_ms(100);

        // Flash the firmware
        let result = self.do_flash_ui(firmware, verify, &mut progress);

        // Always reset UI chip back to user mode
        let _ = self.reset_ui_to_user();

        result
    }

    /// Helper to flash the UI chip. Separated so borrows are released before reset.
    fn do_flash_ui<F>(
        &mut self,
        firmware: &[u8],
        verify: bool,
        progress: &mut F,
    ) -> Result<(), FlashError<std::io::Error>>
    where
        F: FnMut(FlashPhase, usize, usize),
    {
        let mut ui_tunnel = TunnelPort::new_ui(&self.port);
        let mut bl = Bootloader::new(&mut ui_tunnel);

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

        // Verify by reading back (optional)
        if verify {
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
struct TunnelSerialPort<'a, P> {
    port: &'a SharedPort<P>,
    net_buffer: &'a mut heapless::Vec<u8, MAX_VALUE_SIZE>,
    read_buffer: Vec<u8>,
    timeout: Duration,
    baud_rate: u32,
}

impl<'a, P> TunnelSerialPort<'a, P>
where
    P: Read + Write + Send,
{
    fn new(port: &'a SharedPort<P>, net_buffer: &'a mut heapless::Vec<u8, MAX_VALUE_SIZE>, baud_rate: u32) -> Self {
        TunnelSerialPort {
            port,
            net_buffer,
            read_buffer: Vec::new(),
            timeout: Duration::from_secs(3),
            baud_rate,
        }
    }

    fn net_reader(&mut self) -> TunnelReader<'_, P> {
        TunnelReader::new(MgmtToCtl::FromNet, self.port, self.net_buffer)
    }

    fn net_writer(&self) -> TunnelWriter<'_, P> {
        TunnelWriter::new(CtlToMgmt::ToNet, self.port)
    }
}

impl<P> TunnelSerialPort<'_, P>
where
    P: Read + Write + Send + SetTimeout + SetBaudRate,
{
    /// Propagate timeout to underlying serial port.
    fn propagate_timeout(&mut self, timeout: Duration) -> std::io::Result<()> {
        self.port.lock().unwrap().set_timeout(timeout)
    }

    /// Change the baud rate on both CTL-MGMT and MGMT-NET links.
    ///
    /// This sends commands to MGMT to change both UART baud rates,
    /// then updates the local serial port to match.
    fn change_baud_rate(&mut self, baud_rate: u32) -> std::io::Result<()> {
        println!(
            "TunnelSerialPort: changing baud rate from {} to {}",
            self.baud_rate, baud_rate
        );
        let baud_bytes = baud_rate.to_le_bytes();

        // 1. Send SetNetBaudRate to change MGMT-NET link
        //    ACK comes back at current CTL-MGMT rate
        write_tlv(&mut *self.port.lock().unwrap(), CtlToMgmt::SetNetBaudRate, &baud_bytes)?;

        // Wait for ACK from MGMT
        loop {
            match read_tlv::<MgmtToCtl, _>(&mut *self.port.lock().unwrap()) {
                Ok(Some(tlv)) if tlv.tlv_type == MgmtToCtl::Ack => break,
                Ok(Some(_)) => continue, // Ignore other messages
                Ok(None) => {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::TimedOut,
                        "timeout waiting for SetNetBaudRate ACK",
                    ));
                }
                Err(e) => {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::Other,
                        format!("{:?}", e),
                    ));
                }
            }
        }

        // 2. Send SetCtlBaudRate to change CTL-MGMT link
        //    ACK comes back at OLD rate, then MGMT switches
        write_tlv(&mut *self.port.lock().unwrap(), CtlToMgmt::SetCtlBaudRate, &baud_bytes)?;

        // Wait for ACK from MGMT
        loop {
            match read_tlv::<MgmtToCtl, _>(&mut *self.port.lock().unwrap()) {
                Ok(Some(tlv)) if tlv.tlv_type == MgmtToCtl::Ack => break,
                Ok(Some(_)) => continue, // Ignore other messages
                Ok(None) => {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::TimedOut,
                        "timeout waiting for SetCtlBaudRate ACK",
                    ));
                }
                Err(e) => {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::Other,
                        format!("{:?}", e),
                    ));
                }
            }
        }

        // Small delay for MGMT to complete the baud rate switch
        std::thread::sleep(Duration::from_millis(10));

        // 3. Update local serial port baud rate
        self.port.lock().unwrap().set_baud_rate(baud_rate)?;

        self.baud_rate = baud_rate;
        println!("TunnelSerialPort: baud rate changed to {}", baud_rate);
        Ok(())
    }
}

impl<P> io::Read for TunnelSerialPort<'_, P>
where
    P: Read + Write + Send,
{
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if !self.read_buffer.is_empty() {
            let to_copy = buf.len().min(self.read_buffer.len());
            buf[..to_copy].copy_from_slice(&self.read_buffer[..to_copy]);
            self.read_buffer.drain(..to_copy);
            return Ok(to_copy);
        }
        let mut net_reader = self.net_reader();
        net_reader.read(buf)
    }
}

impl<P> io::Write for TunnelSerialPort<'_, P>
where
    P: Read + Write + Send,
{
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let mut net_writer = self.net_writer();
        net_writer.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        let net_writer = self.net_writer();
        net_writer.port.lock().unwrap().flush()
    }
}

impl<P> SerialPort for TunnelSerialPort<'_, P>
where
    P: Read + Write + Send + SetTimeout + SetBaudRate,
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
        // Actually change the baud rate on both CTL-MGMT and MGMT-NET links
        self.change_baud_rate(baud_rate)
            .map_err(|e| serialport::Error::new(serialport::ErrorKind::Io(e.kind()), e.to_string()))
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
        write_tlv(&mut *self.port.lock().unwrap(), CtlToMgmt::SetNetRst, &[rst as u8])
            .map_err(|e| serialport::Error::new(serialport::ErrorKind::Io(e.kind()), e.to_string()))
    }
    fn write_data_terminal_ready(&mut self, level: bool) -> serialport::Result<()> {
        let boot = !level; // DTR HIGH = BOOT LOW
        write_tlv(&mut *self.port.lock().unwrap(), CtlToMgmt::SetNetBoot, &[boot as u8])
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

/// Trait for types that support setting the baud rate.
pub trait SetBaudRate {
    fn set_baud_rate(&mut self, baud_rate: u32) -> std::io::Result<()>;
}

// Implement SetTimeout for BufferedPort wrapping Box<dyn SerialPort>
impl SetTimeout for BufferedPort<Box<dyn SerialPort>> {
    fn set_timeout(&mut self, timeout: Duration) -> std::io::Result<()> {
        self.port
            .set_timeout(timeout)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e.to_string()))
    }
}

// Implement SetBaudRate for BufferedPort wrapping Box<dyn SerialPort>
impl SetBaudRate for BufferedPort<Box<dyn SerialPort>> {
    fn set_baud_rate(&mut self, baud_rate: u32) -> std::io::Result<()> {
        self.port
            .set_baud_rate(baud_rate)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e.to_string()))
    }
}

// NET chip operations (ESP32)
impl<P> App<P>
where
    P: Read + Write + Send + SetTimeout + SetBaudRate,
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
        const INITIAL_BAUD: u32 = 115_200;

        let port = TunnelSerialPort::new(&self.port, &mut self.net_buffer, INITIAL_BAUD);
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
            INITIAL_BAUD,
        );

        println!("About to connect");

        // Allow espflash to negotiate higher baud rate (up to 460800)
        let mut flasher = Flasher::connect(connection, false, false, true, None, Some(460_800))
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

        // Drop flasher to release borrows on self.reader/self.writer
        drop(flasher);

        // Restore baud rate to initial value
        // espflash may have changed it to a higher rate during flashing
        println!("Restoring baud rate to {}", INITIAL_BAUD);
        self.restore_baud_rate(INITIAL_BAUD)
            .map_err(|e| EspflashError::Espflash(format!("restore baud rate: {:?}", e)))?;

        Ok(())
    }

    /// Restore baud rate on both CTL-MGMT and MGMT-NET links.
    fn restore_baud_rate(&mut self, baud_rate: u32) -> Result<(), CtlError> {
        // Change MGMT-NET baud rate
        self.set_net_baud_rate(baud_rate)?;

        // Change CTL-MGMT baud rate (ACK comes at old rate, then MGMT switches)
        self.set_ctl_baud_rate(baud_rate)?;

        // Small delay for MGMT to complete the baud rate switch
        std::thread::sleep(Duration::from_millis(10));

        // Update local serial port baud rate
        self.port.lock().unwrap().set_baud_rate(baud_rate)?;

        println!("Baud rate restored to {}", baud_rate);
        Ok(())
    }

    /// Get NET chip bootloader info.
    ///
    /// Returns detailed device information including chip type, revision,
    /// flash size, features, MAC address, and security info.
    pub fn get_net_bootloader_info(&mut self) -> Result<EspflashDeviceInfo, EspflashError> {
        let port = TunnelSerialPort::new(&self.port, &mut self.net_buffer, 115_200);
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
        let port = TunnelSerialPort::new(&self.port, &mut self.net_buffer, 115_200);
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
    /// Send a chat message via MoQ.
    pub fn send_chat_message(&mut self, message: &str) -> Result<(), CtlError> {
        write_tlv(
            &mut self.net_writer(),
            MgmtToNet::SendChatMessage,
            message.as_bytes(),
        )?;
        let tlv: Tlv<NetToMgmt> =
            read_tlv(&mut self.net_reader())?.ok_or(CtlError::UnexpectedEof)?;
        match tlv.tlv_type {
            NetToMgmt::ChatMessageSent => Ok(()),
            NetToMgmt::Error => {
                let err = core::str::from_utf8(&tlv.value).unwrap_or("unknown error");
                Err(CtlError::DeviceError(err.to_string()))
            }
            other => Err(CtlError::UnexpectedResponse {
                expected: "ChatMessageSent",
                actual: format!("{:?}", other),
            }),
        }
    }
}

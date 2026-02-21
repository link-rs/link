//! Core CTL functionality, async-first and generic over port type.
//!
//! This module provides `CtlCore<P>`, which implements all CTL operations
//! as async methods, working with any type implementing `CtlPort`.

extern crate alloc;

use alloc::format;
use alloc::string::String;

use crate::shared::{
    ChannelConfig, CtlToMgmt, CtlToNet, CtlToUi, JitterStatsInfo, MgmtToCtl, NetLoopbackMode,
    NetToCtl, StackInfo, Tlv, UiLoopbackMode, UiToCtl, WifiSsid, HEADER_SIZE, MAX_VALUE_SIZE,
    SYNC_WORD,
};
use crate::shared::protocol_config::retries::MAX_TLV_SKIP;
use crate::shared::timing::bootloader::HELLO_TIMEOUT_MS;
use crate::shared::timing::reset::ESP32_RESET_HOLD_MS;
use crate::shared::tlv::{buffer, tunnel};

use super::port::CtlPort;

// ============================================================================
// Error Types
// ============================================================================

/// Errors from CTL operations.
#[derive(Debug)]
pub enum CtlError {
    /// Port I/O error (formatted as string for cross-platform compatibility).
    Port(String),

    /// Received an invalid TLV type.
    InvalidType(u16),

    /// TLV value too long.
    TooLong,

    /// Unexpected end of stream.
    UnexpectedEof,

    /// Received unexpected TLV type (expected vs actual).
    UnexpectedResponse {
        expected: &'static str,
        actual: String,
    },

    /// Data mismatch (e.g., ping/pong data doesn't match).
    DataMismatch,

    /// Invalid response length.
    InvalidLength { expected: usize, actual: usize },

    /// Device returned an error.
    DeviceError(String),

    /// Invalid UTF-8 in response.
    InvalidUtf8,

    /// Invalid data format (deserialization failed).
    InvalidData,

    /// Timeout waiting for response.
    Timeout,
}

impl core::fmt::Display for CtlError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            CtlError::Port(e) => write!(f, "port error: {}", e),
            CtlError::InvalidType(t) => write!(f, "invalid TLV type: 0x{:04x}", t),
            CtlError::TooLong => write!(f, "TLV value too long"),
            CtlError::UnexpectedEof => write!(f, "unexpected end of stream"),
            CtlError::UnexpectedResponse { expected, actual } => {
                write!(
                    f,
                    "unexpected response: expected {}, got {}",
                    expected, actual
                )
            }
            CtlError::DataMismatch => write!(f, "data mismatch"),
            CtlError::InvalidLength { expected, actual } => {
                write!(
                    f,
                    "invalid response length: expected {}, got {}",
                    expected, actual
                )
            }
            CtlError::DeviceError(e) => write!(f, "device error: {}", e),
            CtlError::InvalidUtf8 => write!(f, "invalid UTF-8 in response"),
            CtlError::InvalidData => write!(f, "invalid data format"),
            CtlError::Timeout => write!(f, "timeout"),
        }
    }
}

impl CtlError {
    /// Returns true if this error represents a timeout.
    pub fn is_timeout(&self) -> bool {
        match self {
            CtlError::Timeout => true,
            CtlError::Port(msg) => msg.contains("TimedOut") || msg.contains("timeout"),
            _ => false,
        }
    }
}

#[cfg(feature = "std")]
impl std::error::Error for CtlError {}

// ============================================================================
// CtlCore
// ============================================================================

/// The core CTL controller, generic over port type.
///
/// This struct provides async methods for communicating with MGMT, UI, and NET
/// chips via the TLV protocol. It works with any type implementing `CtlPort`.
pub struct CtlCore<P: CtlPort> {
    port: Option<P>,
    ui_buffer: Vec<u8>,
    net_buffer: Vec<u8>,
}

impl<P: CtlPort> CtlCore<P> {
    /// Create a new CtlCore wrapping the given port.
    pub fn new(port: P) -> Self {
        Self {
            port: Some(port),
            ui_buffer: Vec::new(),
            net_buffer: Vec::new(),
        }
    }

    /// Get a reference to the underlying port.
    ///
    /// # Panics
    /// Panics if the port has been taken via `take_port()`.
    pub fn port_ref(&self) -> &P {
        self.port.as_ref().expect("port has been taken")
    }

    /// Get a mutable reference to the underlying port.
    ///
    /// # Panics
    /// Panics if the port has been taken via `take_port()`.
    pub fn port_mut(&mut self) -> &mut P {
        self.port.as_mut().expect("port has been taken")
    }

    /// Consume the CtlCore and return the underlying port.
    ///
    /// # Panics
    /// Panics if the port has been taken via `take_port()`.
    pub fn into_inner(self) -> P {
        self.port.expect("port has been taken")
    }

    /// Temporarily take the port out for exclusive use (e.g., for flashing).
    ///
    /// The port must be returned via `put_port()` before using any other CtlCore methods.
    ///
    /// # Panics
    /// Panics if the port has already been taken.
    pub fn take_port(&mut self) -> P {
        self.port.take().expect("port has already been taken")
    }

    /// Return a previously taken port.
    ///
    /// # Panics
    /// Panics if a port is already present (wasn't taken, or was returned twice).
    pub fn put_port(&mut self, port: P) {
        if self.port.is_some() {
            panic!("port is already present");
        }
        self.port = Some(port);
    }

    // ========================================================================
    // Low-level TLV operations
    // ========================================================================

    /// Drain any pending data from buffers.
    ///
    /// This clears both the internal TLV buffers and the port's read buffer.
    pub fn drain(&mut self) {
        self.ui_buffer.clear();
        self.net_buffer.clear();
        self.port_mut().clear_buffer();
    }

    /// Read a TLV from the port, scanning for sync word.
    async fn read_tlv<T: TryFrom<u16>>(&mut self) -> Result<Option<Tlv<T>>, CtlError> {
        // Scan for sync word byte-by-byte
        let mut matched = 0usize;
        let mut buf = [0u8; 1];
        while matched < SYNC_WORD.len() {
            let n = match self.port_mut().read(&mut buf).await {
                Ok(n) => n,
                Err(e) if P::is_timeout(&e) => return Ok(None),
                Err(e) => return Err(CtlError::Port(format!("{:?}", e))),
            };
            if n == 0 {
                return Ok(None); // EOF
            }

            if buf[0] == SYNC_WORD[matched] {
                matched += 1;
            } else {
                matched = 0;
                if buf[0] == SYNC_WORD[0] {
                    matched = 1;
                }
            }
        }

        // Read header
        let mut header = [0u8; HEADER_SIZE];
        self.port_mut()
            .read_exact(&mut header)
            .await
            .map_err(|e| CtlError::Port(format!("{:?}", e)))?;

        // Decode header
        let raw_type = u16::from_be_bytes([header[0], header[1]]);
        let length = u32::from_be_bytes([header[2], header[3], header[4], header[5]]) as usize;

        // Check type first
        let Ok(tlv_type) = T::try_from(raw_type) else {
            return Err(CtlError::InvalidType(raw_type));
        };

        // Read value
        let mut value = heapless::Vec::<u8, MAX_VALUE_SIZE>::new();
        if value.resize(length, 0).is_err() {
            return Err(CtlError::TooLong);
        }

        self.port_mut()
            .read_exact(&mut value)
            .await
            .map_err(|e| CtlError::Port(format!("{:?}", e)))?;

        Ok(Some(Tlv { tlv_type, value }))
    }

    /// Write a TLV to the port with sync word prefix.
    async fn write_tlv<T: Into<u16> + Copy>(
        &mut self,
        tlv_type: T,
        value: &[u8],
    ) -> Result<(), CtlError> {
        let type_val: u16 = tlv_type.into();

        // Build the complete packet
        let total_len = SYNC_WORD.len() + HEADER_SIZE + value.len();
        let mut buf = Vec::new();
        let _ = buf.extend_from_slice(&SYNC_WORD);
        let _ = buf.extend_from_slice(&type_val.to_be_bytes());
        let _ = buf.extend_from_slice(&(value.len() as u32).to_be_bytes());
        let _ = buf.extend_from_slice(value);

        debug_assert_eq!(buf.len(), total_len);

        self.port_mut()
            .write_all(&buf)
            .await
            .map_err(|e| CtlError::Port(format!("{:?}", e)))?;
        self.port_mut()
            .flush()
            .await
            .map_err(|e| CtlError::Port(format!("{:?}", e)))?;

        Ok(())
    }

    /// Write a tunneled TLV to UI through MGMT.
    async fn write_tlv_ui(&mut self, tlv_type: CtlToUi, value: &[u8]) -> Result<(), CtlError> {
        let inner = tunnel::encode_nested(tlv_type, value);
        self.write_tlv(CtlToMgmt::ToUi, &inner).await
    }

    /// Write a tunneled TLV to NET through MGMT.
    async fn write_tlv_net(&mut self, tlv_type: CtlToNet, value: &[u8]) -> Result<(), CtlError> {
        let inner = tunnel::encode_nested(tlv_type, value);
        self.write_tlv(CtlToMgmt::ToNet, &inner).await
    }

    /// Read a TLV from MGMT, skipping tunneled messages.
    ///
    /// Tunneled data (FromUi/FromNet) is appended to stream buffers for later parsing.
    /// Limited to MAX_TLV_SKIP tunneled messages to prevent infinite looping when
    /// NET/UI data arrives faster than it can be processed (especially in WASM).
    async fn read_tlv_mgmt(&mut self) -> Result<Tlv<MgmtToCtl>, CtlError> {
        for _ in 0..MAX_TLV_SKIP {
            let tlv = self
                .read_tlv::<MgmtToCtl>()
                .await?
                .ok_or(CtlError::UnexpectedEof)?;
            match tlv.tlv_type {
                MgmtToCtl::FromUi => {
                    // Append to UI stream buffer
                    let _ = self.ui_buffer.extend_from_slice(&tlv.value);
                    continue;
                }
                MgmtToCtl::FromNet => {
                    // Append to NET stream buffer
                    let _ = self.net_buffer.extend_from_slice(&tlv.value);
                    continue;
                }
                _ => return Ok(tlv),
            }
        }
        Err(CtlError::Timeout)
    }

    /// Read a TLV from UI tunnel stream.
    ///
    /// The UI tunnel data is treated as a byte stream - TLVs may span multiple
    /// FromUi messages. This method scans for sync words and parses complete TLVs.
    async fn read_tlv_ui(&mut self) -> Result<Tlv<UiToCtl>, CtlError> {
        loop {
            // Try to parse a TLV from the buffer
            if let Some(tlv) = Self::try_parse_tlv_from_buffer::<UiToCtl>(&mut self.ui_buffer)? {
                return Ok(tlv);
            }

            // Need more data - read from wire
            let tlv = self
                .read_tlv::<MgmtToCtl>()
                .await?
                .ok_or(CtlError::UnexpectedEof)?;
            match tlv.tlv_type {
                MgmtToCtl::FromUi => {
                    // Append to UI stream buffer
                    let _ = self.ui_buffer.extend_from_slice(&tlv.value);
                }
                MgmtToCtl::FromNet => {
                    // Append to NET stream buffer
                    let _ = self.net_buffer.extend_from_slice(&tlv.value);
                }
                _ => {
                    // Skip MGMT-level messages (Pong, StackInfo, etc.)
                }
            }
        }
    }

    /// Read a TLV from UI tunnel, skipping Log messages.
    async fn read_tlv_ui_skip_log(&mut self) -> Result<Tlv<UiToCtl>, CtlError> {
        loop {
            let tlv = self.read_tlv_ui().await?;
            if tlv.tlv_type != UiToCtl::Log {
                return Ok(tlv);
            }
            // Skip log messages
        }
    }

    /// Read a TLV from NET tunnel stream.
    ///
    /// The NET tunnel data is treated as a byte stream - TLVs may span multiple
    /// FromNet messages, and non-TLV data (like raw log output) is discarded.
    async fn read_tlv_net(&mut self) -> Result<Tlv<NetToCtl>, CtlError> {
        loop {
            // Try to parse a TLV from the buffer
            if let Some(tlv) = Self::try_parse_tlv_from_buffer::<NetToCtl>(&mut self.net_buffer)? {
                return Ok(tlv);
            }

            // Need more data - read from wire
            let tlv = self
                .read_tlv::<MgmtToCtl>()
                .await?
                .ok_or(CtlError::UnexpectedEof)?;
            match tlv.tlv_type {
                MgmtToCtl::FromNet => {
                    let _ = self.net_buffer.extend_from_slice(&tlv.value);
                }
                MgmtToCtl::FromUi => {
                    let _ = self.ui_buffer.extend_from_slice(&tlv.value);
                }
                _ => {
                    // Skip MGMT-level messages (Pong, StackInfo, etc.)
                }
            }
        }
    }

    /// Try to parse a TLV from a stream buffer.
    ///
    /// Scans for sync word, discarding any non-TLV data (like raw log output).
    /// Returns `Ok(Some(tlv))` if a complete TLV was parsed and consumed from the buffer.
    /// Returns `Ok(None)` if more data is needed.
    /// Returns `Err` on parse errors (invalid type, etc.).
    fn try_parse_tlv_from_buffer<T: TryFrom<u16>>(
        buffer: &mut Vec<u8>,
    ) -> Result<Option<Tlv<T>>, CtlError> {
        // Try to parse using shared buffer utilities
        match buffer::try_parse_from_buffer(buffer) {
            Ok(Some((tlv, consumed))) => {
                // Consume the parsed TLV from buffer
                buffer.copy_within(consumed.., 0);
                buffer.truncate(buffer.len() - consumed);
                Ok(Some(tlv))
            }
            Ok(None) => {
                // No complete TLV yet - check if we should discard garbage
                if let Some(sync_pos) = buffer::find_sync_word(buffer) {
                    // Sync word found - discard garbage before it
                    if sync_pos > 0 {
                        buffer.copy_within(sync_pos.., 0);
                        buffer.truncate(buffer.len() - sync_pos);
                    }
                } else {
                    // No sync word - keep last 3 bytes (partial sync match possible)
                    let keep = buffer.len().min(SYNC_WORD.len() - 1);
                    let start = buffer.len() - keep;
                    buffer.copy_within(start.., 0);
                    buffer.truncate(keep);
                }
                Ok(None)
            }
            Err(buffer::ParseError::InvalidType(raw_type)) => {
                Err(CtlError::InvalidType(raw_type))
            }
            Err(buffer::ParseError::TooLong) => {
                // Invalid length - discard one byte and retry next time
                buffer.copy_within(1.., 0);
                buffer.truncate(buffer.len() - 1);
                Ok(None)
            }
        }
    }

    // ========================================================================
    // MGMT operations
    // ========================================================================

    /// Initialize DTR/RTS to known good state after opening the serial port.
    ///
    /// On EV16 hardware: DTR→NRST (high=reset), RTS→BOOT0 (high=bootloader).
    /// Setting both low ensures the MGMT chip is running in normal mode.
    /// Waits 100ms for the chip to stabilize after deasserting reset.
    pub async fn init_port<D, F>(&mut self, delay_ms: D)
    where
        D: Fn(u64) -> F,
        F: core::future::Future<Output = ()>,
    {
        let _ = self.port_mut().write_dtr(false).await;
        let _ = self.port_mut().write_rts(false).await;
        delay_ms(100).await;
    }

    /// Send Hello handshake to detect if a valid device is connected.
    ///
    /// Returns true if the device responds correctly with challenge XOR'd with b"LINK".
    ///
    /// Uses `read_tlv()` which scans for sync words byte-by-byte, making it robust
    /// against misaligned data (e.g. NET boot spam that arrives before the first TLV).
    pub async fn hello(&mut self, challenge: &[u8; 4]) -> bool {
        const MAGIC: &[u8; 4] = b"LINK";

        let expected_value: [u8; 4] = [
            challenge[0] ^ MAGIC[0],
            challenge[1] ^ MAGIC[1],
            challenge[2] ^ MAGIC[2],
            challenge[3] ^ MAGIC[3],
        ];

        // Send the Hello request
        if self.write_tlv(CtlToMgmt::Hello, challenge).await.is_err() {
            return false;
        }

        // Read TLV frames using sync word scanning, skipping non-Hello ones
        for _ in 0..MAX_TLV_SKIP {
            match self.read_tlv::<MgmtToCtl>().await {
                Ok(Some(tlv)) => {
                    if tlv.tlv_type == MgmtToCtl::Hello && tlv.value.len() == 4 {
                        return tlv.value.as_slice() == expected_value;
                    }
                    // Skip non-Hello TLVs (e.g. FromNet boot spam)
                }
                Ok(None) | Err(_) => return false,
            }
        }

        false // Too many non-Hello TLVs
    }

    /// Wait for MGMT to be ready by repeatedly trying hello() with short timeouts.
    ///
    /// This is useful after resetting MGMT (e.g., after a baud rate change that
    /// reopens the serial port). Instead of blindly waiting, we actively probe
    /// until MGMT responds or we hit the retry limit.
    ///
    /// Returns true if MGMT responded, false if all retries exhausted.
    pub async fn wait_for_mgmt_ready(&mut self, max_attempts: usize) -> bool
    where
        P: crate::ctl::SetTimeout,
    {
        // Save the original timeout so we can restore it when done.
        let original_timeout = self.port.as_ref().map(|p| p.timeout());

        // Set short timeout for hello attempts
        if let Some(port) = &mut self.port {
            let _ = port.set_timeout(std::time::Duration::from_millis(HELLO_TIMEOUT_MS));
        }

        for _attempt in 1..=max_attempts {
            let challenge = [0x12, 0x34, 0x56, 0x78];

            if self.hello(&challenge).await {
                // Success! Restore original timeout and return
                if let (Some(port), Some(timeout)) = (&mut self.port, original_timeout) {
                    let _ = port.set_timeout(timeout);
                }
                return true;
            }

            // hello() already has timeout built in, no need for additional delay
        }

        // All attempts failed, restore original timeout
        if let (Some(port), Some(timeout)) = (&mut self.port, original_timeout) {
            let _ = port.set_timeout(timeout);
        }
        false
    }

    /// Wait for the NET chip to be ready by sending a single ping with a long timeout.
    ///
    /// The NET chip (ESP32-S3) takes several seconds to boot after MGMT releases
    /// it from reset. If WiFi credentials are stored, it may block during WiFi
    /// connection. This method sends one ping and waits for a response.
    ///
    /// Returns `true` if NET responded, `false` if timeout expired.
    pub async fn wait_for_net_ready(&mut self, timeout_secs: u64) -> bool
    where
        P: crate::ctl::SetTimeout,
    {
        let original_timeout = self.port.as_ref().map(|p| p.timeout());

        if let Some(port) = &mut self.port {
            let _ = port.set_timeout(std::time::Duration::from_secs(timeout_secs));
        }

        self.net_buffer.clear();
        let result = self.net_ping(b"ready").await.is_ok();

        if let (Some(port), Some(timeout)) = (&mut self.port, original_timeout) {
            let _ = port.set_timeout(timeout);
        }
        result
    }

    /// Ping the MGMT chip.
    pub async fn mgmt_ping(&mut self, data: &[u8]) -> Result<(), CtlError> {
        self.write_tlv(CtlToMgmt::Ping, data).await?;
        let tlv = self.read_tlv_mgmt().await?;
        if tlv.tlv_type != MgmtToCtl::Pong {
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

    /// Get MGMT chip stack usage information.
    pub async fn mgmt_get_stack_info(&mut self) -> Result<StackInfo, CtlError> {
        self.write_tlv(CtlToMgmt::GetStackInfo, &[]).await?;
        let tlv = self.read_tlv_mgmt().await?;
        if tlv.tlv_type != MgmtToCtl::StackInfo {
            return Err(CtlError::UnexpectedResponse {
                expected: "StackInfo",
                actual: format!("{:?}", tlv.tlv_type),
            });
        }
        StackInfo::from_bytes(&tlv.value).ok_or(CtlError::InvalidData)
    }

    /// Repaint the MGMT chip stack for future measurement.
    pub async fn mgmt_repaint_stack(&mut self) -> Result<(), CtlError> {
        self.write_tlv(CtlToMgmt::RepaintStack, &[]).await?;
        let tlv = self.read_tlv_mgmt().await?;
        if tlv.tlv_type != MgmtToCtl::Ack {
            return Err(CtlError::UnexpectedResponse {
                expected: "Ack",
                actual: format!("{:?}", tlv.tlv_type),
            });
        }
        Ok(())
    }

    /// Set the UI UART baud rate on the MGMT chip.
    ///
    /// This changes the baud rate between MGMT and UI chips.
    /// The MGMT chip changes both TX and RX baud rates unilaterally.
    pub async fn set_ui_baud_rate(&mut self, baud_rate: u32) -> Result<(), CtlError> {
        self.write_tlv(CtlToMgmt::SetUiBaudRate, &baud_rate.to_be_bytes())
            .await?;
        let tlv = self.read_tlv_mgmt().await?;
        if tlv.tlv_type != MgmtToCtl::Ack {
            return Err(CtlError::UnexpectedResponse {
                expected: "Ack",
                actual: format!("{:?}", tlv.tlv_type),
            });
        }
        Ok(())
    }

    /// Write a raw TLV to the MGMT connection.
    pub async fn write_tlv_raw(
        &mut self,
        tlv_type: CtlToMgmt,
        value: &[u8],
    ) -> Result<(), CtlError> {
        self.write_tlv(tlv_type, value).await
    }

    /// Read a raw TLV from the MGMT connection.
    pub async fn read_tlv_raw(&mut self) -> Result<Option<Tlv<MgmtToCtl>>, CtlError> {
        self.read_tlv().await
    }

    // ========================================================================
    // UI operations
    // ========================================================================

    /// Ping the UI chip through the MGMT tunnel.
    pub async fn ui_ping(&mut self, data: &[u8]) -> Result<(), CtlError> {
        self.write_tlv_ui(CtlToUi::Ping, data).await?;
        let tlv = self.read_tlv_ui_skip_log().await?;
        if tlv.tlv_type != UiToCtl::Pong {
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

    /// Get the version stored in UI chip EEPROM.
    pub async fn get_version(&mut self) -> Result<u32, CtlError> {
        self.write_tlv_ui(CtlToUi::GetVersion, &[]).await?;
        let tlv = self.read_tlv_ui_skip_log().await?;
        if tlv.tlv_type != UiToCtl::Version {
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
    pub async fn set_version(&mut self, version: u32) -> Result<(), CtlError> {
        self.write_tlv_ui(CtlToUi::SetVersion, &version.to_be_bytes())
            .await?;
        let tlv = self.read_tlv_ui_skip_log().await?;
        if tlv.tlv_type != UiToCtl::Ack {
            return Err(CtlError::UnexpectedResponse {
                expected: "Ack",
                actual: format!("{:?}", tlv.tlv_type),
            });
        }
        Ok(())
    }

    /// Get the SFrame key stored in UI chip EEPROM.
    pub async fn get_sframe_key(&mut self) -> Result<[u8; 16], CtlError> {
        self.write_tlv_ui(CtlToUi::GetSFrameKey, &[]).await?;
        let tlv = self.read_tlv_ui_skip_log().await?;
        if tlv.tlv_type != UiToCtl::SFrameKey {
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
    ///
    /// The key must be exactly 16 bytes. Returns `InvalidLength` if not.
    pub async fn set_sframe_key(&mut self, key: &[u8]) -> Result<(), CtlError> {
        if key.len() != 16 {
            return Err(CtlError::InvalidLength {
                expected: 16,
                actual: key.len(),
            });
        }
        self.write_tlv_ui(CtlToUi::SetSFrameKey, key).await?;
        let tlv = self.read_tlv_ui_skip_log().await?;
        if tlv.tlv_type != UiToCtl::Ack {
            return Err(CtlError::UnexpectedResponse {
                expected: "Ack",
                actual: format!("{:?}", tlv.tlv_type),
            });
        }
        Ok(())
    }

    /// Set UI chip loopback mode.
    pub async fn ui_set_loopback(&mut self, mode: UiLoopbackMode) -> Result<(), CtlError> {
        self.write_tlv_ui(CtlToUi::SetLoopback, &[mode as u8])
            .await?;
        let tlv = self.read_tlv_ui_skip_log().await?;
        if tlv.tlv_type != UiToCtl::Ack {
            return Err(CtlError::UnexpectedResponse {
                expected: "Ack",
                actual: format!("{:?}", tlv.tlv_type),
            });
        }
        Ok(())
    }

    /// Get UI chip loopback mode.
    pub async fn ui_get_loopback(&mut self) -> Result<UiLoopbackMode, CtlError> {
        self.write_tlv_ui(CtlToUi::GetLoopback, &[]).await?;
        let tlv = self.read_tlv_ui_skip_log().await?;
        if tlv.tlv_type != UiToCtl::Loopback {
            return Err(CtlError::UnexpectedResponse {
                expected: "Loopback",
                actual: format!("{:?}", tlv.tlv_type),
            });
        }
        let mode_byte = tlv.value.first().copied().unwrap_or(0);
        Ok(UiLoopbackMode::try_from(mode_byte).unwrap_or(UiLoopbackMode::Off))
    }

    /// Get UI chip stack usage information.
    pub async fn ui_get_stack_info(&mut self) -> Result<StackInfo, CtlError> {
        self.write_tlv_ui(CtlToUi::GetStackInfo, &[]).await?;
        let tlv = self.read_tlv_ui_skip_log().await?;
        if tlv.tlv_type == UiToCtl::Error {
            let msg = core::str::from_utf8(&tlv.value).unwrap_or("unknown error");
            return Err(CtlError::DeviceError(msg.into()));
        }
        if tlv.tlv_type != UiToCtl::StackInfo {
            return Err(CtlError::UnexpectedResponse {
                expected: "StackInfo",
                actual: format!("{:?}", tlv.tlv_type),
            });
        }
        StackInfo::from_bytes(&tlv.value).ok_or(CtlError::InvalidData)
    }

    /// Repaint the UI chip stack for future measurement.
    pub async fn ui_repaint_stack(&mut self) -> Result<(), CtlError> {
        self.write_tlv_ui(CtlToUi::RepaintStack, &[]).await?;
        let tlv = self.read_tlv_ui_skip_log().await?;
        if tlv.tlv_type == UiToCtl::Error {
            let msg = core::str::from_utf8(&tlv.value).unwrap_or("unknown error");
            return Err(CtlError::DeviceError(msg.into()));
        }
        if tlv.tlv_type != UiToCtl::Ack {
            return Err(CtlError::UnexpectedResponse {
                expected: "Ack",
                actual: format!("{:?}", tlv.tlv_type),
            });
        }
        Ok(())
    }

    /// Set UI chip BOOT0 pin directly.
    pub async fn set_ui_boot0(&mut self, value: crate::shared::PinValue) -> Result<(), CtlError> {
        use crate::shared::Pin;
        self.write_tlv(CtlToMgmt::SetPin, &[Pin::UiBoot0 as u8, value as u8]).await?;
        let tlv = self.read_tlv_mgmt().await?;
        if tlv.tlv_type != MgmtToCtl::Ack {
            return Err(CtlError::UnexpectedResponse {
                expected: "Ack",
                actual: format!("{:?}", tlv.tlv_type),
            });
        }
        Ok(())
    }

    /// Set UI chip BOOT1 pin directly.
    pub async fn set_ui_boot1(&mut self, value: crate::shared::PinValue) -> Result<(), CtlError> {
        use crate::shared::Pin;
        self.write_tlv(CtlToMgmt::SetPin, &[Pin::UiBoot1 as u8, value as u8]).await?;
        let tlv = self.read_tlv_mgmt().await?;
        if tlv.tlv_type != MgmtToCtl::Ack {
            return Err(CtlError::UnexpectedResponse {
                expected: "Ack",
                actual: format!("{:?}", tlv.tlv_type),
            });
        }
        Ok(())
    }

    /// Set UI chip RST pin directly.
    pub async fn set_ui_rst(&mut self, value: crate::shared::PinValue) -> Result<(), CtlError> {
        use crate::shared::Pin;
        self.write_tlv(CtlToMgmt::SetPin, &[Pin::UiRst as u8, value as u8]).await?;
        let tlv = self.read_tlv_mgmt().await?;
        if tlv.tlv_type != MgmtToCtl::Ack {
            return Err(CtlError::UnexpectedResponse {
                expected: "Ack",
                actual: format!("{:?}", tlv.tlv_type),
            });
        }
        Ok(())
    }

    /// Reset the UI chip into bootloader mode using pin control.
    ///
    /// Includes a delay between RST transitions so the STM32 reliably
    /// samples the BOOT pins.
    pub async fn reset_ui_to_bootloader<D, F>(&mut self, delay_ms: D) -> Result<(), CtlError>
    where
        D: Fn(u64) -> F,
        F: core::future::Future<Output = ()>,
    {
        use crate::shared::PinValue;
        use crate::shared::timing::reset::STM32_SIGNAL_TRANSITION_MS;
        // BOOT0=1, BOOT1=0, then RST cycle
        self.set_ui_boot0(PinValue::High).await?;
        self.set_ui_boot1(PinValue::Low).await?;
        self.set_ui_rst(PinValue::Low).await?;
        delay_ms(STM32_SIGNAL_TRANSITION_MS).await;
        self.set_ui_rst(PinValue::High).await
    }

    /// Reset the UI chip into user mode using pin control.
    ///
    /// Includes a delay between RST transitions so the STM32 reliably
    /// samples the BOOT pins.
    pub async fn reset_ui_to_user<D, F>(&mut self, delay_ms: D) -> Result<(), CtlError>
    where
        D: Fn(u64) -> F,
        F: core::future::Future<Output = ()>,
    {
        use crate::shared::PinValue;
        use crate::shared::timing::reset::STM32_SIGNAL_TRANSITION_MS;
        // BOOT0=0, BOOT1=1, then RST cycle
        self.set_ui_boot0(PinValue::Low).await?;
        self.set_ui_boot1(PinValue::High).await?;
        self.set_ui_rst(PinValue::Low).await?;
        delay_ms(STM32_SIGNAL_TRANSITION_MS).await;
        self.set_ui_rst(PinValue::High).await
    }

    /// Hold the UI chip in reset.
    pub async fn hold_ui_reset(&mut self) -> Result<(), CtlError> {
        use crate::shared::PinValue;
        self.set_ui_rst(PinValue::Low).await
    }

    /// Read a log message from the UI chip.
    ///
    /// Returns `Ok(Some(message))` if a Log TLV was received,
    /// `Ok(None)` if a non-Log TLV was received (which is discarded),
    /// or an error if reading failed.
    pub async fn read_ui_log(&mut self) -> Result<Option<String>, CtlError> {
        let tlv = self.read_tlv_ui().await?;
        if tlv.tlv_type == UiToCtl::Log {
            match core::str::from_utf8(&tlv.value) {
                Ok(msg) => Ok(Some(msg.into())),
                Err(_) => Ok(Some(format!("<invalid utf8: {:?}>", tlv.value.as_slice()))),
            }
        } else {
            // Non-log UI TLV - discard it
            Ok(None)
        }
    }

    /// Try to read a log message from the UI chip (non-blocking/timeout-aware).
    ///
    /// Returns `Ok(Some(message))` if a Log TLV was received,
    /// `Ok(None)` if timeout/no data, or if a non-Log TLV was received,
    /// or an error for real I/O failures.
    ///
    /// Use this for polling scenarios where you expect timeouts.
    pub async fn try_read_ui_log(&mut self) -> Result<Option<String>, CtlError> {
        // First, try to parse a TLV from the existing buffer
        if let Some(tlv) = Self::try_parse_tlv_from_buffer::<UiToCtl>(&mut self.ui_buffer)? {
            if tlv.tlv_type == UiToCtl::Log {
                match core::str::from_utf8(&tlv.value) {
                    Ok(msg) => return Ok(Some(msg.into())),
                    Err(_) => {
                        return Ok(Some(format!(
                            "<invalid utf8: {:?}>",
                            tlv.value.as_slice()
                        )))
                    }
                }
            }
            // Non-log TLV, discard it and check for more data
        }

        // Need more data - try to read from wire (returns None on timeout)
        let Some(tlv) = self.read_tlv::<MgmtToCtl>().await? else {
            return Ok(None); // Timeout/no data
        };

        // Append data to appropriate buffer
        match tlv.tlv_type {
            MgmtToCtl::FromUi => {
                let _ = self.ui_buffer.extend_from_slice(&tlv.value);
            }
            MgmtToCtl::FromNet => {
                let _ = self.net_buffer.extend_from_slice(&tlv.value);
            }
            _ => {
                // Skip MGMT-level messages
            }
        }

        // Return None to indicate we got data but not a complete Log TLV yet
        // (caller will call us again in the next iteration)
        Ok(None)
    }

    // ========================================================================
    // NET operations
    // ========================================================================

    /// Ping the NET chip through the MGMT tunnel.
    pub async fn net_ping(&mut self, data: &[u8]) -> Result<(), CtlError> {
        self.net_buffer.clear();
        self.write_tlv_net(CtlToNet::Ping, data).await?;
        let tlv = self.read_tlv_net().await?;
        if tlv.tlv_type != NetToCtl::Pong {
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

    /// Set NET chip loopback mode.
    pub async fn net_set_loopback(&mut self, mode: NetLoopbackMode) -> Result<(), CtlError> {
        self.write_tlv_net(CtlToNet::SetLoopback, &[mode as u8])
            .await?;
        let tlv = self.read_tlv_net().await?;
        if tlv.tlv_type != NetToCtl::Ack {
            return Err(CtlError::UnexpectedResponse {
                expected: "Ack",
                actual: format!("{:?}", tlv.tlv_type),
            });
        }
        Ok(())
    }

    /// Get NET chip loopback mode.
    pub async fn net_get_loopback(&mut self) -> Result<NetLoopbackMode, CtlError> {
        self.write_tlv_net(CtlToNet::GetLoopback, &[]).await?;
        let tlv = self.read_tlv_net().await?;
        if tlv.tlv_type == NetToCtl::Error {
            let msg = core::str::from_utf8(&tlv.value).unwrap_or("<invalid utf8>");
            return Err(CtlError::DeviceError(format!("GetLoopback: {}", msg)));
        }
        if tlv.tlv_type != NetToCtl::Loopback {
            return Err(CtlError::UnexpectedResponse {
                expected: "Loopback",
                actual: format!("{:?}", tlv.tlv_type),
            });
        }
        let mode_byte = tlv.value.first().copied().unwrap_or(0);
        Ok(NetLoopbackMode::try_from(mode_byte).unwrap_or(NetLoopbackMode::Off))
    }

    /// Add a WiFi SSID and password pair to NET chip storage.
    pub async fn add_wifi_ssid(&mut self, ssid: &str, password: &str) -> Result<(), CtlError> {
        let wifi = WifiSsid {
            ssid: ssid.try_into().map_err(|_| CtlError::TooLong)?,
            password: password.try_into().map_err(|_| CtlError::TooLong)?,
        };
        let mut buf = [0u8; 128];
        let serialized = postcard::to_slice(&wifi, &mut buf).map_err(|_| CtlError::TooLong)?;
        self.write_tlv_net(CtlToNet::AddWifiSsid, serialized)
            .await?;
        let tlv = self.read_tlv_net().await?;
        if tlv.tlv_type != NetToCtl::Ack {
            return Err(CtlError::UnexpectedResponse {
                expected: "Ack",
                actual: format!("{:?}", tlv.tlv_type),
            });
        }
        Ok(())
    }

    /// Get all WiFi SSIDs from NET chip storage.
    pub async fn get_wifi_ssids(&mut self) -> Result<Vec<WifiSsid>, CtlError> {
        self.write_tlv_net(CtlToNet::GetWifiSsids, &[]).await?;
        let tlv = self.read_tlv_net().await?;
        if tlv.tlv_type != NetToCtl::WifiSsids {
            return Err(CtlError::UnexpectedResponse {
                expected: "WifiSsids",
                actual: format!("{:?}", tlv.tlv_type),
            });
        }
        postcard::from_bytes(&tlv.value).map_err(|_| CtlError::InvalidUtf8)
    }

    /// Clear all WiFi SSIDs from NET chip storage.
    pub async fn clear_wifi_ssids(&mut self) -> Result<(), CtlError> {
        self.write_tlv_net(CtlToNet::ClearWifiSsids, &[]).await?;
        let tlv = self.read_tlv_net().await?;
        if tlv.tlv_type != NetToCtl::Ack {
            return Err(CtlError::UnexpectedResponse {
                expected: "Ack",
                actual: format!("{:?}", tlv.tlv_type),
            });
        }
        Ok(())
    }

    /// Get the relay URL from NET chip storage.
    pub async fn get_relay_url(&mut self) -> Result<String, CtlError> {
        self.write_tlv_net(CtlToNet::GetRelayUrl, &[]).await?;
        let tlv = self.read_tlv_net().await?;
        if tlv.tlv_type != NetToCtl::RelayUrl {
            return Err(CtlError::UnexpectedResponse {
                expected: "RelayUrl",
                actual: format!("{:?}", tlv.tlv_type),
            });
        }
        let url_str = core::str::from_utf8(&tlv.value).map_err(|_| CtlError::InvalidUtf8)?;
        url_str.try_into().map_err(|_| CtlError::TooLong)
    }

    /// Set the relay URL in NET chip storage.
    pub async fn set_relay_url(&mut self, url: &str) -> Result<(), CtlError> {
        self.write_tlv_net(CtlToNet::SetRelayUrl, url.as_bytes())
            .await?;
        let tlv = self.read_tlv_net().await?;
        if tlv.tlv_type != NetToCtl::Ack {
            return Err(CtlError::UnexpectedResponse {
                expected: "Ack",
                actual: format!("{:?}", tlv.tlv_type),
            });
        }
        Ok(())
    }

    /// Get configuration for a specific channel.
    pub async fn get_channel_config(&mut self, channel_id: u8) -> Result<ChannelConfig, CtlError> {
        self.write_tlv_net(CtlToNet::GetChannelConfig, &[channel_id])
            .await?;
        let tlv = self.read_tlv_net().await?;
        if tlv.tlv_type != NetToCtl::ChannelConfig {
            return Err(CtlError::UnexpectedResponse {
                expected: "ChannelConfig",
                actual: format!("{:?}", tlv.tlv_type),
            });
        }
        postcard::from_bytes(&tlv.value).map_err(|_| CtlError::InvalidData)
    }

    /// Set configuration for a channel.
    pub async fn set_channel_config(&mut self, config: &ChannelConfig) -> Result<(), CtlError> {
        let mut buf = [0u8; 256];
        let serialized = postcard::to_slice(config, &mut buf).map_err(|_| CtlError::TooLong)?;
        self.write_tlv_net(CtlToNet::SetChannelConfig, serialized)
            .await?;
        let tlv = self.read_tlv_net().await?;
        if tlv.tlv_type != NetToCtl::Ack {
            return Err(CtlError::UnexpectedResponse {
                expected: "Ack",
                actual: format!("{:?}", tlv.tlv_type),
            });
        }
        Ok(())
    }

    /// Clear all channel configurations.
    pub async fn clear_channel_configs(&mut self) -> Result<(), CtlError> {
        self.write_tlv_net(CtlToNet::ClearChannelConfigs, &[])
            .await?;
        let tlv = self.read_tlv_net().await?;
        if tlv.tlv_type != NetToCtl::Ack {
            return Err(CtlError::UnexpectedResponse {
                expected: "Ack",
                actual: format!("{:?}", tlv.tlv_type),
            });
        }
        Ok(())
    }

    /// Get jitter buffer statistics for a channel.
    pub async fn get_jitter_stats(
        &mut self,
        channel_id: u8,
    ) -> Result<JitterStatsInfo, CtlError> {
        self.write_tlv_net(CtlToNet::GetJitterStats, &[channel_id])
            .await?;
        let tlv = self.read_tlv_net().await?;
        if tlv.tlv_type != NetToCtl::JitterStats {
            return Err(CtlError::UnexpectedResponse {
                expected: "JitterStats",
                actual: format!("{:?}", tlv.tlv_type),
            });
        }
        let stats = JitterStatsInfo::from_bytes(&tlv.value).ok_or(CtlError::InvalidData)?;
        Ok(stats)
    }

    /// Set NET chip BOOT pin directly.
    pub async fn set_net_boot(&mut self, value: crate::shared::PinValue) -> Result<(), CtlError> {
        use crate::shared::Pin;
        self.write_tlv(CtlToMgmt::SetPin, &[Pin::NetBoot as u8, value as u8]).await?;
        let tlv = self.read_tlv_mgmt().await?;
        if tlv.tlv_type != MgmtToCtl::Ack {
            return Err(CtlError::UnexpectedResponse {
                expected: "Ack",
                actual: format!("{:?}", tlv.tlv_type),
            });
        }
        Ok(())
    }

    /// Set NET chip RST pin directly.
    pub async fn set_net_rst(&mut self, value: crate::shared::PinValue) -> Result<(), CtlError> {
        use crate::shared::Pin;
        self.write_tlv(CtlToMgmt::SetPin, &[Pin::NetRst as u8, value as u8]).await?;
        let tlv = self.read_tlv_mgmt().await?;
        if tlv.tlv_type != MgmtToCtl::Ack {
            return Err(CtlError::UnexpectedResponse {
                expected: "Ack",
                actual: format!("{:?}", tlv.tlv_type),
            });
        }
        Ok(())
    }

    /// Reset the NET chip into bootloader mode using pin control.
    ///
    /// Includes 10ms delays between RST transitions to match the ESP32's
    /// reset timing requirements (same delays as the old MGMT-side handler).
    pub async fn reset_net_to_bootloader<D, F>(&mut self, delay_ms: D) -> Result<(), CtlError>
    where
        D: Fn(u64) -> F,
        F: core::future::Future<Output = ()>,
    {
        use crate::shared::PinValue;
        // First power cycle (clean slate)
        self.set_net_rst(PinValue::Low).await?;
        delay_ms(ESP32_RESET_HOLD_MS).await;
        self.set_net_rst(PinValue::High).await?;
        // Set BOOT low for bootloader mode
        self.set_net_boot(PinValue::Low).await?;
        // Second power cycle - ESP32 samples BOOT when RST goes high
        self.set_net_rst(PinValue::Low).await?;
        delay_ms(ESP32_RESET_HOLD_MS).await;
        self.set_net_rst(PinValue::High).await
    }

    /// Reset the NET chip into user mode using pin control.
    ///
    /// Includes a 10ms delay between RST transitions.
    pub async fn reset_net_to_user<D, F>(&mut self, delay_ms: D) -> Result<(), CtlError>
    where
        D: Fn(u64) -> F,
        F: core::future::Future<Output = ()>,
    {
        use crate::shared::PinValue;
        self.set_net_boot(PinValue::High).await?;
        self.set_net_rst(PinValue::Low).await?;
        delay_ms(ESP32_RESET_HOLD_MS).await;
        self.set_net_rst(PinValue::High).await
    }

    /// Hold the NET chip in reset.
    pub async fn hold_net_reset(&mut self) -> Result<(), CtlError> {
        use crate::shared::PinValue;
        self.set_net_rst(PinValue::Low).await
    }

    // ========================================================================
    // Circular ping
    // ========================================================================

    /// Send a circular ping starting from UI (UI -> NET -> MGMT -> CTL).
    pub async fn ui_first_circular_ping(&mut self, data: &[u8]) -> Result<(), CtlError> {
        self.write_tlv_ui(CtlToUi::CircularPing, data).await?;
        let tlv = self.read_tlv_net().await?;
        if tlv.tlv_type != NetToCtl::CircularPing {
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
    pub async fn net_first_circular_ping(&mut self, data: &[u8]) -> Result<(), CtlError> {
        self.write_tlv_net(CtlToNet::CircularPing, data).await?;
        let tlv = self.read_tlv_ui_skip_log().await?;
        if tlv.tlv_type != UiToCtl::CircularPing {
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
}

/// Escape non-ASCII bytes as `\xAB` while passing ASCII through.
pub fn escape_non_ascii(bytes: &[u8]) -> String {
    let mut out = String::new();
    for &b in bytes {
        if b.is_ascii() && !b.is_ascii_control() {
            out.push(b as char);
        } else if b == b'\n' {
            out.push('\n');
        } else if b == b'\r' {
            out.push('\r');
        } else if b == b'\t' {
            out.push('\t');
        } else {
            out.push_str(&format!("\\x{:02X}", b));
        }
    }
    out
}

//! CTL (Controller) chip - the host computer interface.
//!
//! This module is `no_std` compatible but requires the `alloc` crate for
//! compression support.

extern crate alloc;

pub mod esp;
pub mod stm;

use crate::shared::{
    CtlToMgmt, MAX_VALUE_SIZE, MgmtToCtl, MgmtToNet, MgmtToUi, NetToMgmt, ReadTlv, Tlv, UiToMgmt,
    WifiSsid, WriteTlv,
};
use core::future::Future;
use embedded_io_async::{ErrorType, Read, Write};
pub use esp::ChipType as NetChipType;
use esp::{Bootloader as EspBootloader, SecurityInfo};
use stm::Bootloader;

/// Maximum size for verification error data (matches write chunk size).
const VERIFY_CHUNK_SIZE: usize = 256;

/// Maximum size for ESP32 boot message line buffer.
const LINE_BUFFER_SIZE: usize = 128;

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

/// Information retrieved from the NET chip (ESP32) when it's in bootloader mode.
#[derive(Debug, Clone)]
pub struct NetBootloaderInfo {
    /// Security information from the ESP32 (includes chip type detection).
    pub security_info: SecurityInfo,
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

/// Errors that can occur during NET (ESP32) flash operations.
#[derive(Debug)]
pub enum NetFlashError<E> {
    /// ESP32 bootloader protocol error.
    Bootloader(esp::Error<E>),
    /// MD5 verification failed - flash contents don't match uploaded data.
    VerifyFailed {
        address: u32,
        size: u32,
        expected: [u8; 16],
        actual: [u8; 16],
    },
    /// Compression failed.
    CompressionFailed,
}

impl<E> From<esp::Error<E>> for NetFlashError<E> {
    fn from(e: esp::Error<E>) -> Self {
        NetFlashError::Bootloader(e)
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

/// A reader that extracts data from TLV packets received through MGMT.
///
/// Buffers incoming TLV values and exposes them as a continuous byte stream
/// via the `Read` trait. Also implements `ReadTlv` via the blanket impl.
struct TunnelReader<'a, R> {
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

impl<'a, R> ErrorType for TunnelReader<'a, R>
where
    R: Read,
{
    type Error = <R as ErrorType>::Error;
}

impl<'a, R> Read for TunnelReader<'a, R>
where
    R: ReadTlv<MgmtToCtl> + Read,
{
    async fn read(&mut self, buf: &mut [u8]) -> Result<usize, Self::Error> {
        while self.buffer.is_empty() {
            let tlv = self.reader.read_tlv().await.unwrap().unwrap();
            if tlv.tlv_type != self.tlv_type {
                continue;
            }
            // heapless extend_from_slice returns Result, unwrap since we know capacity is sufficient
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
///
/// Encodes the inner TLV first, then sends it as the value of an outer
/// tunnel TLV. Implements `WriteTlv` directly (not via the blanket impl).
struct TunnelWriter<'a, W> {
    tlv_type: CtlToMgmt,
    writer: &'a mut W,
}

impl<'a, W> TunnelWriter<'a, W> {
    fn new(tlv_type: CtlToMgmt, writer: &'a mut W) -> Self {
        Self { tlv_type, writer }
    }
}

impl<'a, W> ErrorType for TunnelWriter<'a, W>
where
    W: Write,
{
    type Error = <W as ErrorType>::Error;
}

impl<'a, W> Write for TunnelWriter<'a, W>
where
    W: Write,
{
    async fn write(&mut self, buf: &[u8]) -> Result<usize, Self::Error> {
        let to_write = core::cmp::min(MAX_VALUE_SIZE, buf.len());
        self.writer
            .write_tlv(self.tlv_type, &buf[..to_write])
            .await?;
        Ok(to_write)
    }

    async fn flush(&mut self) -> Result<(), Self::Error> {
        self.writer.flush().await
    }
}

/// Encapsulates the read side of MGMT communication.
///
/// Provides typed readers for UI and NET tunnels that can be borrowed
/// independently from the write side.
struct MgmtReader<R> {
    from_mgmt: R,
    ui_buffer: heapless::Vec<u8, MAX_VALUE_SIZE>,
    net_buffer: heapless::Vec<u8, MAX_VALUE_SIZE>,
}

impl<R> ErrorType for MgmtReader<R>
where
    R: Read,
{
    type Error = <R as ErrorType>::Error;
}

impl<R> Read for MgmtReader<R>
where
    R: Read,
{
    async fn read(&mut self, buf: &mut [u8]) -> Result<usize, Self::Error> {
        self.from_mgmt.read(buf).await
    }
}

impl<R> MgmtReader<R>
where
    R: Read,
{
    fn new(from_mgmt: R) -> Self {
        Self {
            from_mgmt,
            ui_buffer: heapless::Vec::new(),
            net_buffer: heapless::Vec::new(),
        }
    }

    /// Get a reader for the UI tunnel.
    fn ui(&mut self) -> TunnelReader<'_, R> {
        TunnelReader::new(MgmtToCtl::FromUi, &mut self.from_mgmt, &mut self.ui_buffer)
    }

    /// Get a reader for the NET tunnel.
    fn net(&mut self) -> TunnelReader<'_, R> {
        TunnelReader::new(
            MgmtToCtl::FromNet,
            &mut self.from_mgmt,
            &mut self.net_buffer,
        )
    }
}

/// Encapsulates the write side of MGMT communication.
///
/// Provides typed writers for UI and NET tunnels that can be borrowed
/// independently from the read side.
struct MgmtWriter<W> {
    to_mgmt: W,
}

impl<W> ErrorType for MgmtWriter<W>
where
    W: Write,
{
    type Error = <W as ErrorType>::Error;
}

impl<W> Write for MgmtWriter<W>
where
    W: Write,
{
    async fn write(&mut self, buf: &[u8]) -> Result<usize, Self::Error> {
        self.to_mgmt.write(buf).await
    }

    async fn flush(&mut self) -> Result<(), Self::Error> {
        self.to_mgmt.flush().await
    }
}

impl<W> MgmtWriter<W>
where
    W: Write,
{
    fn new(to_mgmt: W) -> Self {
        Self { to_mgmt }
    }

    /// Get a writer for the UI tunnel (TLV protocol).
    fn ui(&mut self) -> TunnelWriter<'_, W> {
        TunnelWriter::new(CtlToMgmt::ToUi, &mut self.to_mgmt)
    }

    /// Get a writer for the NET tunnel.
    fn net(&mut self) -> TunnelWriter<'_, W> {
        TunnelWriter::new(CtlToMgmt::ToNet, &mut self.to_mgmt)
    }
}

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

    pub async fn mgmt_ping(&mut self, data: &[u8]) {
        self.writer.must_write_tlv(CtlToMgmt::Ping, data).await;
        let tlv: Tlv<MgmtToCtl> = self.reader.must_read_tlv().await;
        assert_eq!(tlv.tlv_type, MgmtToCtl::Pong);
        assert_eq!(&tlv.value, data);
    }

    /// Send a Hello handshake to detect if a valid device is connected.
    ///
    /// Sends a 4-byte challenge value and verifies the response is the challenge
    /// XOR'd with b"LINK". Returns true if the handshake succeeded.
    pub async fn hello(&mut self, challenge: &[u8; 4]) -> bool {
        const MAGIC: &[u8; 4] = b"LINK";

        self.writer
            .must_write_tlv(CtlToMgmt::Hello, challenge)
            .await;
        let tlv: Tlv<MgmtToCtl> = self.reader.must_read_tlv().await;

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

    pub async fn ui_ping(&mut self, data: &[u8]) {
        self.writer.ui().must_write_tlv(MgmtToUi::Ping, data).await;
        let tlv: Tlv<UiToMgmt> = self.reader.ui().must_read_tlv().await;
        assert_eq!(tlv.tlv_type, UiToMgmt::Pong);
        assert_eq!(&tlv.value, data);
    }

    pub async fn net_ping(&mut self, data: &[u8]) {
        self.writer
            .net()
            .must_write_tlv(MgmtToNet::Ping, data)
            .await;
        let tlv: Tlv<NetToMgmt> = self.reader.net().must_read_tlv().await;
        assert_eq!(tlv.tlv_type, NetToMgmt::Pong);
        assert_eq!(&tlv.value, data);
    }

    pub async fn ui_first_circular_ping(&mut self, data: &[u8]) {
        self.writer
            .ui()
            .must_write_tlv(MgmtToUi::CircularPing, data)
            .await;
        let tlv: Tlv<NetToMgmt> = self.reader.net().must_read_tlv().await;
        assert_eq!(tlv.tlv_type, NetToMgmt::CircularPing);
        assert_eq!(&tlv.value, data);
    }

    pub async fn net_first_circular_ping(&mut self, data: &[u8]) {
        self.writer
            .net()
            .must_write_tlv(MgmtToNet::CircularPing, data)
            .await;
        let tlv: Tlv<UiToMgmt> = self.reader.ui().must_read_tlv().await;
        assert_eq!(tlv.tlv_type, UiToMgmt::CircularPing);
        assert_eq!(&tlv.value, data);
    }

    /// Get the version stored in UI chip EEPROM.
    pub async fn get_version(&mut self) -> u32 {
        self.writer
            .ui()
            .must_write_tlv(MgmtToUi::GetVersion, &[])
            .await;
        let tlv: Tlv<UiToMgmt> = self.reader.ui().must_read_tlv().await;
        assert_eq!(tlv.tlv_type, UiToMgmt::Version);
        assert_eq!(tlv.value.len(), 4);
        u32::from_be_bytes([tlv.value[0], tlv.value[1], tlv.value[2], tlv.value[3]])
    }

    /// Set the version stored in UI chip EEPROM.
    pub async fn set_version(&mut self, version: u32) {
        self.writer
            .ui()
            .must_write_tlv(MgmtToUi::SetVersion, &version.to_be_bytes())
            .await;
        let tlv: Tlv<UiToMgmt> = self.reader.ui().must_read_tlv().await;
        assert_eq!(tlv.tlv_type, UiToMgmt::Ack);
    }

    /// Get the SFrame key stored in UI chip EEPROM.
    pub async fn get_sframe_key(&mut self) -> [u8; 16] {
        self.writer
            .ui()
            .must_write_tlv(MgmtToUi::GetSFrameKey, &[])
            .await;
        let tlv: Tlv<UiToMgmt> = self.reader.ui().must_read_tlv().await;
        assert_eq!(tlv.tlv_type, UiToMgmt::SFrameKey);
        assert_eq!(tlv.value.len(), 16);
        let mut key = [0u8; 16];
        key.copy_from_slice(&tlv.value);
        key
    }

    /// Set the SFrame key stored in UI chip EEPROM.
    pub async fn set_sframe_key(&mut self, key: &[u8; 16]) {
        self.writer
            .ui()
            .must_write_tlv(MgmtToUi::SetSFrameKey, key)
            .await;
        let tlv: Tlv<UiToMgmt> = self.reader.ui().must_read_tlv().await;
        assert_eq!(tlv.tlv_type, UiToMgmt::Ack);
    }

    /// Set UI chip loopback mode.
    /// When enabled, mic audio goes directly to speaker instead of to NET.
    pub async fn ui_set_loopback(&mut self, enabled: bool) {
        self.writer
            .ui()
            .must_write_tlv(MgmtToUi::SetLoopback, &[enabled as u8])
            .await;
        let tlv: Tlv<UiToMgmt> = self.reader.ui().must_read_tlv().await;
        assert_eq!(tlv.tlv_type, UiToMgmt::Ack);
    }

    /// Get UI chip loopback mode.
    pub async fn ui_get_loopback(&mut self) -> bool {
        self.writer
            .ui()
            .must_write_tlv(MgmtToUi::GetLoopback, &[])
            .await;
        let tlv: Tlv<UiToMgmt> = self.reader.ui().must_read_tlv().await;
        assert_eq!(tlv.tlv_type, UiToMgmt::Loopback);
        tlv.value.first().copied().unwrap_or(0) != 0
    }

    /// Set NET chip loopback mode.
    /// When enabled, audio from UI goes back to UI through jitter buffer instead of to WebSocket.
    pub async fn net_set_loopback(&mut self, enabled: bool) {
        self.writer
            .net()
            .must_write_tlv(MgmtToNet::SetLoopback, &[enabled as u8])
            .await;
        let tlv: Tlv<NetToMgmt> = self.reader.net().must_read_tlv().await;
        assert_eq!(tlv.tlv_type, NetToMgmt::Ack);
    }

    /// Get NET chip loopback mode.
    pub async fn net_get_loopback(&mut self) -> bool {
        self.writer
            .net()
            .must_write_tlv(MgmtToNet::GetLoopback, &[])
            .await;
        let tlv: Tlv<NetToMgmt> = self.reader.net().must_read_tlv().await;
        assert_eq!(tlv.tlv_type, NetToMgmt::Loopback);
        tlv.value.first().copied().unwrap_or(0) != 0
    }

    /// Add a WiFi SSID and password pair to NET chip storage.
    pub async fn add_wifi_ssid(&mut self, ssid: &str, password: &str) {
        let wifi = WifiSsid {
            ssid: ssid.try_into().expect("SSID too long"),
            password: password.try_into().expect("Password too long"),
        };
        let mut buf = [0u8; 128];
        let serialized = postcard::to_slice(&wifi, &mut buf).expect("Serialization failed");
        self.writer
            .net()
            .must_write_tlv(MgmtToNet::AddWifiSsid, serialized)
            .await;
        let tlv: Tlv<NetToMgmt> = self.reader.net().must_read_tlv().await;
        assert_eq!(tlv.tlv_type, NetToMgmt::Ack);
    }

    /// Get all WiFi SSIDs from NET chip storage.
    pub async fn get_wifi_ssids(&mut self) -> heapless::Vec<WifiSsid, 8> {
        self.writer
            .net()
            .must_write_tlv(MgmtToNet::GetWifiSsids, &[])
            .await;
        let tlv: Tlv<NetToMgmt> = self.reader.net().must_read_tlv().await;
        assert_eq!(tlv.tlv_type, NetToMgmt::WifiSsids);
        postcard::from_bytes(&tlv.value).expect("Deserialization failed")
    }

    /// Clear all WiFi SSIDs from NET chip storage.
    pub async fn clear_wifi_ssids(&mut self) {
        self.writer
            .net()
            .must_write_tlv(MgmtToNet::ClearWifiSsids, &[])
            .await;
        let tlv: Tlv<NetToMgmt> = self.reader.net().must_read_tlv().await;
        assert_eq!(tlv.tlv_type, NetToMgmt::Ack);
    }

    /// Get the relay URL from NET chip storage.
    pub async fn get_relay_url(&mut self) -> heapless::String<128> {
        self.writer
            .net()
            .must_write_tlv(MgmtToNet::GetRelayUrl, &[])
            .await;
        let tlv: Tlv<NetToMgmt> = self.reader.net().must_read_tlv().await;
        assert_eq!(tlv.tlv_type, NetToMgmt::RelayUrl);
        let url_str = core::str::from_utf8(&tlv.value).expect("Invalid UTF-8");
        url_str.try_into().expect("URL too long")
    }

    /// Set the relay URL in NET chip storage.
    pub async fn set_relay_url(&mut self, url: &str) {
        self.writer
            .net()
            .must_write_tlv(MgmtToNet::SetRelayUrl, url.as_bytes())
            .await;
        let tlv: Tlv<NetToMgmt> = self.reader.net().must_read_tlv().await;
        assert_eq!(tlv.tlv_type, NetToMgmt::Ack);
    }

    /// Send data over WebSocket and verify echo response.
    ///
    /// This sends data to the relay server via WebSocket and expects the same
    /// data back (assumes an echo server). Useful for testing WS connectivity.
    pub async fn ws_ping(&mut self, data: &[u8]) {
        self.writer
            .net()
            .must_write_tlv(MgmtToNet::WsSend, data)
            .await;
        let tlv: Tlv<NetToMgmt> = self.reader.net().must_read_tlv().await;
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
    pub async fn ws_echo_test(&mut self) -> EchoTestResults {
        // Tunnel through MGMT to NET (like ws_ping does)
        self.writer
            .net()
            .must_write_tlv(MgmtToNet::WsEchoTest, &[])
            .await;

        // Wait for result from NET (tunneled through MGMT as FromNet)
        let tlv: Tlv<NetToMgmt> = self.reader.net().must_read_tlv().await;
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
    pub async fn ws_speed_test(&mut self) -> SpeedTestResults {
        // Tunnel through MGMT to NET
        self.writer
            .net()
            .must_write_tlv(MgmtToNet::WsSpeedTest, &[])
            .await;

        // Wait for result from NET (tunneled through MGMT as FromNet)
        let tlv: Tlv<NetToMgmt> = self.reader.net().must_read_tlv().await;
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
    pub async fn get_mgmt_bootloader_info(
        &mut self,
    ) -> Result<MgmtBootloaderInfo, stm::Error<R::Error>>
    where
        W::Error: Into<R::Error>,
    {
        let mut bl = Bootloader::new(&mut self.reader, &mut self.writer);

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

        // Reset MGMT chip back to normal operation by jumping to user firmware
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
    pub async fn flash_mgmt<F>(
        &mut self,
        firmware: &[u8],
        mut progress: F,
    ) -> Result<(), FlashError<R::Error>>
    where
        W::Error: Into<R::Error>,
        F: FnMut(FlashPhase, usize, usize),
    {
        let mut bl = Bootloader::new(&mut self.reader, &mut self.writer);

        // Initialize communication
        bl.init().await?;

        // Erase pages needed for firmware (STM32F072CB has 2KB pages)
        // Erase page-by-page for progress feedback
        const PAGE_SIZE: usize = 2048;
        let pages_needed = (firmware.len() + PAGE_SIZE - 1) / PAGE_SIZE;
        let pages_needed = pages_needed.max(1); // At least 1 page

        for page in 0..pages_needed {
            progress(FlashPhase::Erasing, page, pages_needed);
            // Try extended erase first, fall back to legacy
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

        Ok(())
    }

    /// Reset the UI chip into bootloader mode.
    ///
    /// Sends a command to MGMT which toggles the BOOT0 and RST pins
    /// to put the UI chip into bootloader mode.
    pub async fn reset_ui_to_bootloader(&mut self) {
        self.writer
            .must_write_tlv(CtlToMgmt::ResetUiToBootloader, &[])
            .await;
        let tlv: Tlv<MgmtToCtl> = self.reader.must_read_tlv().await;
        assert_eq!(tlv.tlv_type, MgmtToCtl::Ack);
    }

    /// Reset the UI chip into user mode (normal operation).
    ///
    /// Sends a command to MGMT which toggles the BOOT0 and RST pins
    /// to put the UI chip back into normal user mode.
    pub async fn reset_ui_to_user(&mut self) {
        self.writer
            .must_write_tlv(CtlToMgmt::ResetUiToUser, &[])
            .await;
        let tlv: Tlv<MgmtToCtl> = self.reader.must_read_tlv().await;
        assert_eq!(tlv.tlv_type, MgmtToCtl::Ack);
    }

    /// Hold the UI chip in reset.
    ///
    /// Sends a command to MGMT to assert the RST pin low, keeping the
    /// UI chip in reset until released.
    pub async fn hold_ui_reset(&mut self) {
        self.writer
            .must_write_tlv(CtlToMgmt::HoldUiReset, &[])
            .await;
        let tlv: Tlv<MgmtToCtl> = self.reader.must_read_tlv().await;
        assert_eq!(tlv.tlv_type, MgmtToCtl::Ack);
    }

    /// Reset the NET chip into bootloader mode.
    ///
    /// Sends a command to MGMT which toggles the BOOT0 and RST pins
    /// to put the NET chip into bootloader mode.
    pub async fn reset_net_to_bootloader(&mut self) {
        self.writer
            .must_write_tlv(CtlToMgmt::ResetNetToBootloader, &[])
            .await;
        // Read TLVs, skipping any FromNet (boot messages from NET chip) until we get the Ack
        for _ in 0..100 {
            let tlv: Tlv<MgmtToCtl> = self.reader.must_read_tlv().await;
            match tlv.tlv_type {
                MgmtToCtl::Ack => return,
                MgmtToCtl::FromNet => continue,
                other => panic!("unexpected TLV type: {:?}", other),
            }
        }
        panic!("gave up waiting for Ack after discarding 100 FromNet TLVs");
    }

    /// Reset the NET chip into user mode (normal operation).
    ///
    /// Sends a command to MGMT which toggles the BOOT0 and RST pins
    /// to put the NET chip back into normal user mode.
    pub async fn reset_net_to_user(&mut self) {
        self.writer
            .must_write_tlv(CtlToMgmt::ResetNetToUser, &[])
            .await;
        // Read TLVs, skipping any FromNet (boot messages from NET chip) until we get the Ack
        for _ in 0..100 {
            let tlv: Tlv<MgmtToCtl> = self.reader.must_read_tlv().await;
            match tlv.tlv_type {
                MgmtToCtl::Ack => return,
                MgmtToCtl::FromNet => continue,
                other => panic!("unexpected TLV type: {:?}", other),
            }
        }
        panic!("gave up waiting for Ack after discarding 100 FromNet TLVs");
    }

    /// Get bootloader information from the UI chip.
    ///
    /// This method:
    /// 1. Resets the UI chip into bootloader mode
    /// 2. Queries bootloader information via the tunneled UI connection
    /// 3. Resets the UI chip back to user mode
    ///
    /// The `delay_ms` parameter should return a future that completes after
    /// the specified number of milliseconds. For tokio, use `|ms| tokio::time::sleep(Duration::from_millis(ms))`.
    ///
    /// Returns bootloader version, chip ID, supported commands, and optionally
    /// a sample of flash memory if read protection is not enabled.
    pub async fn get_ui_bootloader_info<D, Fut>(
        &mut self,
        delay_ms: D,
    ) -> Result<MgmtBootloaderInfo, stm::Error<R::Error>>
    where
        W::Error: Into<R::Error>,
        D: FnOnce(u64) -> Fut,
        Fut: Future<Output = ()>,
    {
        // Reset UI chip into bootloader mode
        self.reset_ui_to_bootloader().await;

        // Wait for bootloader to be ready
        delay_ms(1000).await;

        // Query bootloader info, capturing any error
        let result = self.query_ui_bootloader().await;

        // Always reset UI chip back to user mode
        self.reset_ui_to_user().await;

        result
    }

    /// Helper to query the UI bootloader. Separated so borrows are released before reset.
    async fn query_ui_bootloader(&mut self) -> Result<MgmtBootloaderInfo, stm::Error<R::Error>>
    where
        W::Error: Into<R::Error>,
    {
        // Create a bootloader client using the tunneled UI connection
        let mut ui_reader = self.reader.ui();
        let mut ui_writer = self.writer.ui();
        let mut bl = Bootloader::new(&mut ui_reader, &mut ui_writer);

        // Initialize communication (sends 0x7F for auto-baud detection)
        bl.init().await?;

        // Get bootloader info
        let info = bl.get().await?;

        // Get chip ID
        let chip_id = bl.get_id().await?;

        // Try to read a small amount of memory from the start of flash
        let mut flash_sample = [0u8; 32];
        let flash_sample = match bl.read_memory(0x0800_0000, &mut flash_sample).await {
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
    /// The `delay_ms` parameter should return a future that completes after
    /// the specified number of milliseconds.
    ///
    /// The progress callback is called with (phase, bytes_processed, total_bytes).
    pub async fn flash_ui<F, D, Fut>(
        &mut self,
        firmware: &[u8],
        delay_ms: D,
        mut progress: F,
    ) -> Result<(), FlashError<R::Error>>
    where
        W::Error: Into<R::Error>,
        F: FnMut(FlashPhase, usize, usize),
        D: FnOnce(u64) -> Fut,
        Fut: Future<Output = ()>,
    {
        // Reset UI chip into bootloader mode
        self.reset_ui_to_bootloader().await;

        // Wait for bootloader to be ready
        delay_ms(100).await;

        // Flash the firmware
        let result = self.do_flash_ui(firmware, &mut progress).await;

        // Always reset UI chip back to user mode
        self.reset_ui_to_user().await;

        result
    }

    /// Helper to flash the UI chip. Separated so borrows are released before reset.
    async fn do_flash_ui<F>(
        &mut self,
        firmware: &[u8],
        progress: &mut F,
    ) -> Result<(), FlashError<R::Error>>
    where
        W::Error: Into<R::Error>,
        F: FnMut(FlashPhase, usize, usize),
    {
        let mut ui_reader = self.reader.ui();
        let mut ui_writer = self.writer.ui();
        let mut bl = Bootloader::new(&mut ui_reader, &mut ui_writer);

        // Initialize communication
        bl.init().await?;

        // Erase sectors needed for firmware (STM32F405RG has variable sector sizes)
        // Sectors 0-3: 16KB, Sector 4: 64KB, Sectors 5-11: 128KB
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

        // Verify by reading back
        let mut verified = 0;
        let mut read_buf = [0u8; 256];

        for chunk in firmware.chunks(256) {
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

        Ok(())
    }

    /// Get bootloader information from the NET chip (ESP32).
    ///
    /// This method:
    /// 1. Resets the NET chip into bootloader mode
    /// 2. Syncs with the ESP32 bootloader via SLIP framing
    /// 3. Queries security information
    /// 4. Resets the NET chip back to user mode
    ///
    /// Returns security information including chip ID and security flags.
    pub async fn get_net_bootloader_info(
        &mut self,
    ) -> Result<NetBootloaderInfo, esp::Error<R::Error>>
    where
        W::Error: Into<R::Error>,
    {
        // Reset NET chip into bootloader mode
        self.reset_net_to_bootloader().await;

        // Query bootloader info (waits for "waiting for download" message)
        let result = self.query_net_bootloader().await;

        // Always reset NET chip back to user mode.
        // We can't use reset_net_to_user() directly because there may be
        // pending FromNet TLVs from bootloader communication that we need to skip.
        self.writer
            .must_write_tlv(CtlToMgmt::ResetNetToUser, &[])
            .await;

        // Read TLVs, skipping any FromNet until we get the Ack
        // Limit iterations to prevent infinite loop if Ack never arrives
        for _ in 0..100 {
            let tlv: Tlv<MgmtToCtl> = self.reader.must_read_tlv().await;
            match tlv.tlv_type {
                MgmtToCtl::Ack => {
                    return result;
                }
                MgmtToCtl::FromNet => {
                    continue;
                }
                other => panic!("unexpected TLV type: {:?}", other),
            }
        }
        // Gave up waiting for Ack after discarding 100 FromNet TLVs

        result
    }

    /// Helper to query the NET bootloader. Separated so borrows are released before reset.
    async fn query_net_bootloader(&mut self) -> Result<NetBootloaderInfo, esp::Error<R::Error>>
    where
        W::Error: Into<R::Error>,
    {
        // Create reader for the NET tunnel
        let mut net_reader = self.reader.net();

        // Wait for ESP32 to be ready by scanning for "waiting for download"
        // The ESP32 prints boot messages before it's ready to receive SLIP commands
        let mut line_buf: heapless::Vec<u8, LINE_BUFFER_SIZE> = heapless::Vec::new();
        loop {
            let mut byte = [0u8; 1];
            embedded_io_async::Read::read_exact(&mut net_reader, &mut byte)
                .await
                .map_err(|_| esp::Error::Timeout)?;

            if byte[0] == 0x0a {
                // End of line
                if let Ok(line) = core::str::from_utf8(&line_buf) {
                    let line = line.trim();
                    if line.contains("waiting for download") {
                        break;
                    }
                }
                line_buf.clear();
            } else if byte[0] != 0x0d {
                // Ignore error if buffer is full - just skip extra chars
                let _ = line_buf.push(byte[0]);
            }
        }

        // Now create the bootloader client and sync
        let mut net_writer = self.writer.net();
        let mut bl = EspBootloader::new(&mut net_reader, &mut net_writer);

        // Synchronize with the ESP32 bootloader
        bl.sync().await?;

        // Get security information (includes chip type detection)
        let security_info = bl.get_security_info().await?;

        Ok(NetBootloaderInfo { security_info })
    }

    /// Flash firmware to the NET chip (ESP32-S3).
    ///
    /// This method:
    /// 1. Resets the NET chip into bootloader mode
    /// 2. Waits for the bootloader to be ready
    /// 3. Syncs with the ESP32 bootloader
    /// 4. Writes the firmware to flash
    /// 5. Verifies with MD5 checksum
    /// 6. Resets the NET chip back to user mode
    ///
    /// # Address Parameter
    ///
    /// The `address` parameter specifies the flash offset. Standard ESP-IDF layout:
    /// - `0x0` - Bootloader
    /// - `0x8000` - Partition table
    /// - `0x10000` - Application (most common for app-only updates)
    ///
    /// **WARNING:** This function only flashes a single binary. For full ESP-IDF
    /// firmware updates, you may need to flash bootloader, partition table, and app
    /// separately. A future version should parse `flasher_args.json` from the
    /// ESP-IDF build directory to automate this.
    ///
    /// The progress callback is called with (phase, bytes_processed, total_bytes).
    /// Note: ESP32 combines erase and write, so Erasing phase reports 0/1 then 1/1.
    pub async fn flash_net<F>(
        &mut self,
        firmware: &[u8],
        address: u32,
        compress: bool,
        verify: bool,
        mut progress: F,
    ) -> Result<(), NetFlashError<R::Error>>
    where
        W::Error: Into<R::Error>,
        F: FnMut(FlashPhase, usize, usize),
    {
        // Reset NET chip into bootloader mode
        self.reset_net_to_bootloader().await;

        // Flash the firmware
        let result = self
            .do_flash_net(firmware, address, compress, verify, &mut progress)
            .await;

        // Always reset NET chip back to user mode.
        // We can't use reset_net_to_user() directly because there may be
        // pending FromNet TLVs from bootloader communication that we need to skip.
        self.writer
            .must_write_tlv(CtlToMgmt::ResetNetToUser, &[])
            .await;

        // Read TLVs, skipping any FromNet until we get the Ack
        for _ in 0..100 {
            let tlv: Tlv<MgmtToCtl> = self.reader.must_read_tlv().await;
            match tlv.tlv_type {
                MgmtToCtl::Ack => {
                    return result;
                }
                MgmtToCtl::FromNet => {
                    continue;
                }
                other => panic!("unexpected TLV type: {:?}", other),
            }
        }
        // Gave up waiting for Ack after discarding 100 FromNet TLVs

        result
    }

    /// Helper to flash the NET chip. Separated so borrows are released before reset.
    async fn do_flash_net<F>(
        &mut self,
        firmware: &[u8],
        address: u32,
        compress: bool,
        verify: bool,
        progress: &mut F,
    ) -> Result<(), NetFlashError<R::Error>>
    where
        W::Error: Into<R::Error>,
        F: FnMut(FlashPhase, usize, usize),
    {
        // Create reader for the NET tunnel
        let mut net_reader = self.reader.net();

        // Wait for ESP32 to be ready by scanning for "waiting for download"
        let mut line_buf: heapless::Vec<u8, LINE_BUFFER_SIZE> = heapless::Vec::new();
        loop {
            let mut byte = [0u8; 1];
            embedded_io_async::Read::read_exact(&mut net_reader, &mut byte)
                .await
                .map_err(|_| esp::Error::<R::Error>::Timeout)?;

            if byte[0] == 0x0a {
                // End of line
                if let Ok(line) = core::str::from_utf8(&line_buf) {
                    let line = line.trim();
                    if line.contains("waiting for download") {
                        break;
                    }
                }
                line_buf.clear();
            } else if byte[0] != 0x0d {
                // Ignore error if buffer is full - just skip extra chars
                let _ = line_buf.push(byte[0]);
            }
        }

        // Create the bootloader client
        let mut net_writer = self.writer.net();
        let mut bl = EspBootloader::new(&mut net_reader, &mut net_writer);

        // Synchronize with the ESP32 bootloader
        bl.sync().await?;

        // Use 1KB block size for good balance of speed and reliability
        const BLOCK_SIZE: u32 = 1024;
        let uncompressed_size = firmware.len() as u32;

        if compress {
            // Compress the firmware using zlib/deflate
            use miniz_oxide::deflate::compress_to_vec_zlib;

            progress(FlashPhase::Compressing, 0, firmware.len());

            // Compression level 6 is default (good balance of speed and ratio)
            let compressed = compress_to_vec_zlib(firmware, 6);

            progress(FlashPhase::Compressing, firmware.len(), firmware.len());

            // Report erase phase (ESP32 erases during flash_defl_begin)
            progress(FlashPhase::Erasing, 0, 1);

            // Begin compressed flash operation
            let _packet_count = bl
                .flash_defl_begin(uncompressed_size, BLOCK_SIZE, address)
                .await?;

            progress(FlashPhase::Erasing, 1, 1);

            // Write compressed data in blocks
            let compressed_total = compressed.len();
            let mut written = 0;

            for (seq, chunk) in compressed.chunks(BLOCK_SIZE as usize).enumerate() {
                bl.flash_defl_data(chunk, seq as u32).await?;

                written += chunk.len();
                progress(FlashPhase::Writing, written, compressed_total);
            }

            // End compressed flash operation (don't reboot - we'll reset via MGMT)
            bl.flash_defl_end(false).await?;
        } else {
            // Uncompressed flash path
            progress(FlashPhase::Erasing, 0, 1);

            let _packet_count = bl
                .flash_begin(uncompressed_size, BLOCK_SIZE, address)
                .await?;

            progress(FlashPhase::Erasing, 1, 1);

            // Write firmware in blocks
            let total = firmware.len();
            let mut written = 0;

            for (seq, chunk) in firmware.chunks(BLOCK_SIZE as usize).enumerate() {
                // Pad to block size with 0xFF and align to 4 bytes
                let mut block = [0xFFu8; BLOCK_SIZE as usize];
                block[..chunk.len()].copy_from_slice(chunk);
                let padded_len = chunk.len().div_ceil(4) * 4;

                bl.flash_data(&block[..padded_len.max(chunk.len())], seq as u32)
                    .await?;

                written += chunk.len();
                progress(FlashPhase::Writing, written, total);
            }

            // End flash operation (don't reboot - we'll reset via MGMT)
            bl.flash_end(false).await?;
        }

        // Verify by computing MD5 of flashed region and comparing to firmware
        if verify {
            let total = firmware.len();
            progress(FlashPhase::Verifying, 0, total);

            // Compute expected MD5 from firmware data
            use md5::{Digest, Md5};
            let mut hasher = Md5::new();
            hasher.update(firmware);
            let expected_md5: [u8; 16] = hasher.finalize().into();

            // Get actual MD5 from flash
            let actual_md5 = bl.spi_flash_md5(address, uncompressed_size).await?;

            progress(FlashPhase::Verifying, total, total);

            if expected_md5 != actual_md5 {
                return Err(NetFlashError::VerifyFailed {
                    address,
                    size: uncompressed_size,
                    expected: expected_md5,
                    actual: actual_md5,
                });
            }
        }

        Ok(())
    }
}

//! CTL (Controller) chip - the host computer interface.

use crate::net::WifiSsid;
use crate::shared::{
    CtlToMgmt, MgmtToCtl, MgmtToNet, MgmtToUi, NetToMgmt, ReadTlv, Tlv, UiToMgmt, WriteTlv,
};
use embedded_io_async::{ErrorType, Read, Write};
use heapless::Vec;

type TunnelBuffer = Vec<u8, { 2 * crate::shared::tlv::MAX_TLV_SIZE }>;

/// A reader that extracts data from TLV packets received through MGMT.
///
/// Buffers incoming TLV values and exposes them as a continuous byte stream
/// via the `Read` trait. Also implements `ReadTlv` via the blanket impl.
struct TunnelReader<'a, R> {
    tlv_type: MgmtToCtl,
    reader: &'a mut R,
    buffer: &'a mut TunnelBuffer,
}

impl<'a, R> TunnelReader<'a, R> {
    fn new(tlv_type: MgmtToCtl, reader: &'a mut R, buffer: &'a mut TunnelBuffer) -> Self {
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
            assert_eq!(tlv.tlv_type, self.tlv_type);
            self.buffer.extend_from_slice(&tlv.value).unwrap();
        }

        let to_copy = core::cmp::min(self.buffer.len(), buf.len());
        buf[..to_copy].copy_from_slice(&self.buffer[..to_copy]);
        self.buffer.drain(..to_copy);
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

impl<'a, T, W> WriteTlv<T> for TunnelWriter<'a, W>
where
    T: Into<u16>,
    W: Write,
{
    type Error = <W as ErrorType>::Error;

    async fn write_tlv(&mut self, tlv_type: T, value: &[u8]) -> Result<(), Self::Error> {
        // Encode the inner TLV first
        let encoded = Tlv::encode(tlv_type, value);
        // Send it as the value of the outer tunnel TLV
        self.writer.write_tlv(self.tlv_type, &encoded).await
    }
}

/// Encapsulates the read side of MGMT communication.
///
/// Provides typed readers for UI and NET tunnels that can be borrowed
/// independently from the write side.
struct MgmtReader<R> {
    from_mgmt: R,
    ui_buffer: TunnelBuffer,
    net_buffer: TunnelBuffer,
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
            ui_buffer: TunnelBuffer::default(),
            net_buffer: TunnelBuffer::default(),
        }
    }

    /// Get a reader for the UI tunnel.
    fn ui(&mut self) -> TunnelReader<'_, R> {
        TunnelReader::new(MgmtToCtl::FromUi, &mut self.from_mgmt, &mut self.ui_buffer)
    }

    /// Get a reader for the NET tunnel.
    fn net(&mut self) -> TunnelReader<'_, R> {
        TunnelReader::new(MgmtToCtl::FromNet, &mut self.from_mgmt, &mut self.net_buffer)
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

    /// Get a writer for the UI tunnel.
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
    pub fn new(to_mgmt: W, from_mgmt: R) -> Self {
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
    pub async fn get_wifi_ssids(&mut self) -> Vec<WifiSsid, 8> {
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

    /// Get the MOQ URL from NET chip storage.
    pub async fn get_moq_url(&mut self) -> heapless::String<128> {
        self.writer
            .net()
            .must_write_tlv(MgmtToNet::GetMoqUrl, &[])
            .await;
        let tlv: Tlv<NetToMgmt> = self.reader.net().must_read_tlv().await;
        assert_eq!(tlv.tlv_type, NetToMgmt::MoqUrl);
        let url_str = core::str::from_utf8(&tlv.value).expect("Invalid UTF-8");
        url_str.try_into().expect("URL too long")
    }

    /// Set the MOQ URL in NET chip storage.
    pub async fn set_moq_url(&mut self, url: &str) {
        self.writer
            .net()
            .must_write_tlv(MgmtToNet::SetMoqUrl, url.as_bytes())
            .await;
        let tlv: Tlv<NetToMgmt> = self.reader.net().must_read_tlv().await;
        assert_eq!(tlv.tlv_type, NetToMgmt::Ack);
    }
}

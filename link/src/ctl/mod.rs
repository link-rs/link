//! CTL (Controller) chip - the host computer interface.

use crate::net::WifiSsid;
use crate::shared::{
    CtlToMgmt, MgmtToCtl, MgmtToNet, MgmtToUi, NetToMgmt, ReadTlv, Tlv, UiToMgmt, WriteTlv,
};
use embedded_io_async::{ErrorType, Read, Write};
use heapless::Vec;

type TunnelBuffer = Vec<u8, 128>;

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
        return Ok(to_copy);
    }
}

pub struct App<R, W> {
    to_mgmt: W,
    from_mgmt: R,
    ui_buffer: TunnelBuffer,
    net_buffer: TunnelBuffer,
}

impl<R, W> App<R, W>
where
    W: Write,
    R: Read,
{
    pub fn new(to_mgmt: W, from_mgmt: R) -> Self {
        Self {
            to_mgmt,
            from_mgmt,
            ui_buffer: TunnelBuffer::default(),
            net_buffer: TunnelBuffer::default(),
        }
    }

    fn ui_reader(&mut self) -> TunnelReader<'_, R> {
        TunnelReader::new(MgmtToCtl::FromUi, &mut self.from_mgmt, &mut self.ui_buffer)
    }

    fn ui_tlv_reader(&mut self) -> TunnelReader<'_, R> {
        self.ui_reader()
    }

    fn net_reader(&mut self) -> TunnelReader<'_, R> {
        TunnelReader::new(
            MgmtToCtl::FromNet,
            &mut self.from_mgmt,
            &mut self.net_buffer,
        )
    }

    fn net_tlv_reader(&mut self) -> TunnelReader<'_, R> {
        self.net_reader()
    }

    async fn write_tunneled_tlv<T: Into<u16> + core::fmt::Debug>(
        &mut self,
        tunnel_type: CtlToMgmt,
        tlv_type: T,
        value: &[u8],
    ) {
        let payload = Tlv::encode(tlv_type, value);
        self.to_mgmt.must_write_tlv(tunnel_type, &payload).await;
    }

    pub async fn mgmt_ping(&mut self, data: &[u8]) {
        self.to_mgmt.must_write_tlv(CtlToMgmt::Ping, data).await;
        let tlv: Tlv<MgmtToCtl> = self.from_mgmt.must_read_tlv().await;
        assert_eq!(tlv.tlv_type, MgmtToCtl::Pong);
        assert_eq!(&tlv.value, data);
    }

    pub async fn ui_ping(&mut self, data: &[u8]) {
        self.write_tunneled_tlv(CtlToMgmt::ToUi, MgmtToUi::Ping, data)
            .await;
        let tlv: Tlv<UiToMgmt> = self.ui_tlv_reader().must_read_tlv().await;
        assert_eq!(tlv.tlv_type, UiToMgmt::Pong);
        assert_eq!(&tlv.value, data);
    }

    pub async fn net_ping(&mut self, data: &[u8]) {
        self.write_tunneled_tlv(CtlToMgmt::ToNet, MgmtToNet::Ping, data)
            .await;
        let tlv: Tlv<NetToMgmt> = self.net_tlv_reader().must_read_tlv().await;
        assert_eq!(tlv.tlv_type, NetToMgmt::Pong);
        assert_eq!(&tlv.value, data);
    }

    pub async fn ui_first_circular_ping(&mut self, data: &[u8]) {
        self.write_tunneled_tlv(CtlToMgmt::ToUi, MgmtToUi::CircularPing, data)
            .await;
        let tlv: Tlv<NetToMgmt> = self.net_tlv_reader().must_read_tlv().await;
        assert_eq!(tlv.tlv_type, NetToMgmt::CircularPing);
        assert_eq!(&tlv.value, data);
    }

    pub async fn net_first_circular_ping(&mut self, data: &[u8]) {
        self.write_tunneled_tlv(CtlToMgmt::ToNet, MgmtToNet::CircularPing, data)
            .await;
        let tlv: Tlv<UiToMgmt> = self.ui_tlv_reader().must_read_tlv().await;
        assert_eq!(tlv.tlv_type, UiToMgmt::CircularPing);
        assert_eq!(&tlv.value, data);
    }

    /// Get the version stored in UI chip EEPROM.
    pub async fn get_version(&mut self) -> u32 {
        self.write_tunneled_tlv(CtlToMgmt::ToUi, MgmtToUi::GetVersion, &[])
            .await;
        let tlv: Tlv<UiToMgmt> = self.ui_tlv_reader().must_read_tlv().await;
        assert_eq!(tlv.tlv_type, UiToMgmt::Version);
        assert_eq!(tlv.value.len(), 4);
        u32::from_be_bytes([tlv.value[0], tlv.value[1], tlv.value[2], tlv.value[3]])
    }

    /// Set the version stored in UI chip EEPROM.
    pub async fn set_version(&mut self, version: u32) {
        self.write_tunneled_tlv(
            CtlToMgmt::ToUi,
            MgmtToUi::SetVersion,
            &version.to_be_bytes(),
        )
        .await;
        let tlv: Tlv<UiToMgmt> = self.ui_tlv_reader().must_read_tlv().await;
        assert_eq!(tlv.tlv_type, UiToMgmt::Ack);
    }

    /// Get the SFrame key stored in UI chip EEPROM.
    pub async fn get_sframe_key(&mut self) -> [u8; 16] {
        self.write_tunneled_tlv(CtlToMgmt::ToUi, MgmtToUi::GetSFrameKey, &[])
            .await;
        let tlv: Tlv<UiToMgmt> = self.ui_tlv_reader().must_read_tlv().await;
        assert_eq!(tlv.tlv_type, UiToMgmt::SFrameKey);
        assert_eq!(tlv.value.len(), 16);
        let mut key = [0u8; 16];
        key.copy_from_slice(&tlv.value);
        key
    }

    /// Set the SFrame key stored in UI chip EEPROM.
    pub async fn set_sframe_key(&mut self, key: &[u8; 16]) {
        self.write_tunneled_tlv(CtlToMgmt::ToUi, MgmtToUi::SetSFrameKey, key)
            .await;
        let tlv: Tlv<UiToMgmt> = self.ui_tlv_reader().must_read_tlv().await;
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
        self.write_tunneled_tlv(CtlToMgmt::ToNet, MgmtToNet::AddWifiSsid, serialized)
            .await;
        let tlv: Tlv<NetToMgmt> = self.net_tlv_reader().must_read_tlv().await;
        assert_eq!(tlv.tlv_type, NetToMgmt::Ack);
    }

    /// Get all WiFi SSIDs from NET chip storage.
    pub async fn get_wifi_ssids(&mut self) -> Vec<WifiSsid, 8> {
        self.write_tunneled_tlv(CtlToMgmt::ToNet, MgmtToNet::GetWifiSsids, &[])
            .await;
        let tlv: Tlv<NetToMgmt> = self.net_tlv_reader().must_read_tlv().await;
        assert_eq!(tlv.tlv_type, NetToMgmt::WifiSsids);
        postcard::from_bytes(&tlv.value).expect("Deserialization failed")
    }

    /// Clear all WiFi SSIDs from NET chip storage.
    pub async fn clear_wifi_ssids(&mut self) {
        self.write_tunneled_tlv(CtlToMgmt::ToNet, MgmtToNet::ClearWifiSsids, &[])
            .await;
        let tlv: Tlv<NetToMgmt> = self.net_tlv_reader().must_read_tlv().await;
        assert_eq!(tlv.tlv_type, NetToMgmt::Ack);
    }

    /// Get the MOQ URL from NET chip storage.
    pub async fn get_moq_url(&mut self) -> heapless::String<128> {
        self.write_tunneled_tlv(CtlToMgmt::ToNet, MgmtToNet::GetMoqUrl, &[])
            .await;
        let tlv: Tlv<NetToMgmt> = self.net_tlv_reader().must_read_tlv().await;
        assert_eq!(tlv.tlv_type, NetToMgmt::MoqUrl);
        let url_str = core::str::from_utf8(&tlv.value).expect("Invalid UTF-8");
        url_str.try_into().expect("URL too long")
    }

    /// Set the MOQ URL in NET chip storage.
    pub async fn set_moq_url(&mut self, url: &str) {
        self.write_tunneled_tlv(CtlToMgmt::ToNet, MgmtToNet::SetMoqUrl, url.as_bytes())
            .await;
        let tlv: Tlv<NetToMgmt> = self.net_tlv_reader().must_read_tlv().await;
        assert_eq!(tlv.tlv_type, NetToMgmt::Ack);
    }
}

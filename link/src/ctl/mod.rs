//! CTL (Controller) chip - the host computer interface.

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

    async fn write_tunneled_tlv<T: Into<u16>>(
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
}

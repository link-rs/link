//! NET (Network) chip - handles network communication.

mod storage;
pub use storage::{
    NetStorage, WifiSsid, MAX_MOQ_URL_LEN, MAX_PASSWORD_LEN, MAX_SSID_LEN, MAX_WIFI_SSIDS,
};

use crate::info;
use crate::shared::{
    read_tlv_loop, Channel, Color, Led, MgmtToNet, NetToMgmt, NetToUi, RawMutex, Tlv, UiToNet,
    WriteTlv,
};
use embedded_hal::digital::StatefulOutputPin;
use embedded_io_async::{Read, Write};
use embedded_storage::{ReadStorage, Storage};

enum Event {
    Mgmt(Tlv<MgmtToNet>),
    Ui(Tlv<UiToNet>),
}

pub struct App<W, R, LR, LG, LB, F> {
    to_mgmt: W,
    to_ui: W,
    from_mgmt: R,
    from_ui: R,
    led: (LR, LG, LB),
    storage: NetStorage<F>,
}

impl<W, R, LR, LG, LB, F> App<W, R, LR, LG, LB, F>
where
    W: Write,
    R: Read,
    LR: StatefulOutputPin,
    LG: StatefulOutputPin,
    LB: StatefulOutputPin,
    F: ReadStorage + Storage,
{
    pub fn new(
        to_mgmt: W,
        from_mgmt: R,
        to_ui: W,
        from_ui: R,
        led: (LR, LG, LB),
        flash: F,
        flash_offset: u32,
    ) -> Self {
        Self {
            to_mgmt,
            to_ui,
            from_mgmt,
            from_ui,
            led,
            storage: NetStorage::new(flash, flash_offset),
        }
    }

    #[allow(unreachable_code)]
    pub async fn run(self) -> ! {
        info!("net: starting");

        let Self {
            mut to_mgmt,
            mut to_ui,
            from_mgmt,
            from_ui,
            led,
            mut storage,
        } = self;

        // Initialize LED
        let mut led = Led::new(led.0, led.1, led.2);
        led.set(Color::Yellow);

        const MAX_QUEUE_DEPTH: usize = 2;
        let channel: Channel<RawMutex, Event, MAX_QUEUE_DEPTH> = Channel::new();

        let mgmt_read_task = read_tlv_loop(from_mgmt, channel.sender(), Event::Mgmt);
        let ui_read_task = read_tlv_loop(from_ui, channel.sender(), Event::Ui);

        let handle_task = async {
            info!("net: ready to handle events");
            loop {
                match channel.receive().await {
                    Event::Mgmt(tlv) => {
                        handle_mgmt(tlv, &mut to_mgmt, &mut to_ui, &mut storage).await
                    }
                    Event::Ui(tlv) => handle_ui(tlv, &mut to_mgmt).await,
                }
            }
        };

        futures::join!(mgmt_read_task, ui_read_task, handle_task);
        unreachable!()
    }
}

async fn handle_mgmt<M, U, F>(
    tlv: Tlv<MgmtToNet>,
    to_mgmt: &mut M,
    to_ui: &mut U,
    storage: &mut NetStorage<F>,
) where
    M: WriteTlv<NetToMgmt>,
    U: WriteTlv<NetToUi>,
    F: ReadStorage + Storage,
{
    match tlv.tlv_type {
        MgmtToNet::Ping => {
            info!("net: mgmt ping, sending pong");
            to_mgmt.must_write_tlv(NetToMgmt::Pong, &tlv.value).await;
        }
        MgmtToNet::CircularPing => {
            info!("net: mgmt circular ping -> ui");
            to_ui
                .must_write_tlv(NetToUi::CircularPing, &tlv.value)
                .await;
        }
        MgmtToNet::AddWifiSsid => {
            info!("net: add wifi ssid");
            // Deserialize WifiSsid from postcard
            let Ok(wifi): Result<WifiSsid, _> = postcard::from_bytes(&tlv.value) else {
                info!("net: failed to deserialize wifi ssid");
                to_mgmt
                    .must_write_tlv(NetToMgmt::Error, b"deserialize")
                    .await;
                return;
            };
            if storage.add_wifi_ssid(&wifi.ssid, &wifi.password).is_err() {
                info!("net: failed to add wifi ssid");
                to_mgmt.must_write_tlv(NetToMgmt::Error, b"add").await;
                return;
            }
            if storage.save().is_err() {
                info!("net: failed to save storage");
                to_mgmt.must_write_tlv(NetToMgmt::Error, b"save").await;
                return;
            }
            to_mgmt.must_write_tlv(NetToMgmt::Ack, &[]).await;
        }
        MgmtToNet::GetWifiSsids => {
            info!("net: get wifi ssids");
            let ssids = storage.get_wifi_ssids();
            let mut buf = [0u8; 256];
            let Ok(serialized) = postcard::to_slice(ssids, &mut buf) else {
                info!("net: failed to serialize wifi ssids");
                return;
            };
            to_mgmt
                .must_write_tlv(NetToMgmt::WifiSsids, serialized)
                .await;
        }
        MgmtToNet::ClearWifiSsids => {
            info!("net: clear wifi ssids");
            storage.clear_wifi_ssids();
            if storage.save().is_err() {
                info!("net: failed to save storage");
                to_mgmt.must_write_tlv(NetToMgmt::Error, b"save").await;
                return;
            }
            to_mgmt.must_write_tlv(NetToMgmt::Ack, &[]).await;
        }
        MgmtToNet::GetMoqUrl => {
            info!("net: get moq url");
            to_mgmt
                .must_write_tlv(NetToMgmt::MoqUrl, storage.get_moq_url().as_bytes())
                .await;
        }
        MgmtToNet::SetMoqUrl => {
            info!("net: set moq url");
            let Ok(url) = core::str::from_utf8(&tlv.value) else {
                info!("net: invalid utf8 in moq url");
                to_mgmt.must_write_tlv(NetToMgmt::Error, b"utf8").await;
                return;
            };
            if storage.set_moq_url(url).is_err() {
                info!("net: failed to set moq url");
                to_mgmt.must_write_tlv(NetToMgmt::Error, b"set").await;
                return;
            }
            if storage.save().is_err() {
                info!("net: failed to save storage");
                to_mgmt.must_write_tlv(NetToMgmt::Error, b"save").await;
                return;
            }
            to_mgmt.must_write_tlv(NetToMgmt::Ack, &[]).await;
        }
    }
}

async fn handle_ui<M>(tlv: Tlv<UiToNet>, to_mgmt: &mut M)
where
    M: WriteTlv<NetToMgmt>,
{
    match tlv.tlv_type {
        UiToNet::CircularPing => {
            info!("net: ui circular ping -> mgmt");
            to_mgmt
                .must_write_tlv(NetToMgmt::CircularPing, &tlv.value)
                .await;
        }
    }
}

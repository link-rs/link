//! NET (Network) chip - handles network communication.

mod storage;
pub use storage::{
    NetStorage, WifiSsid, MAX_PASSWORD_LEN, MAX_RELAY_URL_LEN, MAX_SSID_LEN, MAX_WIFI_SSIDS,
};

use crate::info;
use crate::shared::{
    read_tlv_loop, Channel, Color, CriticalSectionRawMutex, Led, MgmtToNet, NetToMgmt, NetToUi,
    RawMutex, Receiver, Sender, Tlv, UiToNet, WriteTlv,
};
use embedded_hal::digital::StatefulOutputPin;
use embedded_io_async::{Read, Write};
use embedded_storage::{ReadStorage, Storage};
use heapless::{String, Vec};

/// Maximum size for WebSocket message payload.
pub const MAX_WS_PAYLOAD: usize = 256;

/// Commands sent to the WebSocket task.
#[derive(Clone)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub enum WsCommand {
    /// Send data over the WebSocket.
    Send(Vec<u8, MAX_WS_PAYLOAD>),
    /// Connect/reconnect to the relay with the given URL.
    Connect(String<MAX_RELAY_URL_LEN>),
}

/// Events received from the WebSocket task.
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub enum WsEvent {
    /// WebSocket connected to relay.
    Connected,
    /// WebSocket disconnected from relay.
    Disconnected,
    /// Data received from WebSocket.
    Received(Vec<u8, MAX_WS_PAYLOAD>),
}

enum Event {
    Mgmt(Tlv<MgmtToNet>),
    Ui(Tlv<UiToNet>),
    Ws(WsEvent),
}

pub struct App<'a, W, R, LR, LG, LB, F, M: RawMutex, const CMD_N: usize, const EVT_N: usize> {
    to_mgmt: W,
    to_ui: W,
    from_mgmt: R,
    from_ui: R,
    led: (LR, LG, LB),
    storage: NetStorage<F>,
    ws_cmd_tx: Sender<'a, M, WsCommand, CMD_N>,
    ws_event_rx: Receiver<'a, M, WsEvent, EVT_N>,
}

impl<'a, W, R, LR, LG, LB, F, M, const CMD_N: usize, const EVT_N: usize>
    App<'a, W, R, LR, LG, LB, F, M, CMD_N, EVT_N>
where
    W: Write,
    R: Read,
    LR: StatefulOutputPin,
    LG: StatefulOutputPin,
    LB: StatefulOutputPin,
    F: ReadStorage + Storage,
    M: RawMutex,
{
    pub fn new(
        to_mgmt: W,
        from_mgmt: R,
        to_ui: W,
        from_ui: R,
        led: (LR, LG, LB),
        flash: F,
        flash_offset: u32,
        ws_cmd_tx: Sender<'a, M, WsCommand, CMD_N>,
        ws_event_rx: Receiver<'a, M, WsEvent, EVT_N>,
    ) -> Self {
        Self {
            to_mgmt,
            to_ui,
            from_mgmt,
            from_ui,
            led,
            storage: NetStorage::new(flash, flash_offset),
            ws_cmd_tx,
            ws_event_rx,
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
            ws_cmd_tx,
            ws_event_rx,
        } = self;

        // Initialize LED
        let mut led = Led::new(led.0, led.1, led.2);
        led.set(Color::Blue);

        // Send initial relay URL to ws_task (if configured)
        let relay_url = storage.get_relay_url();
        if !relay_url.is_empty() {
            if let Ok(url_string) = String::try_from(relay_url) {
                info!("net: sending initial relay url to ws_task");
                ws_cmd_tx.send(WsCommand::Connect(url_string)).await;
            }
        }

        const MAX_QUEUE_DEPTH: usize = 4;
        let channel: Channel<CriticalSectionRawMutex, Event, MAX_QUEUE_DEPTH> = Channel::new();

        let mgmt_read_task = read_tlv_loop(from_mgmt, channel.sender(), Event::Mgmt);
        let ui_read_task = read_tlv_loop(from_ui, channel.sender(), Event::Ui);

        // Forward WS events to the main event channel
        let ws_event_task = async {
            loop {
                let event = ws_event_rx.receive().await;
                channel.send(Event::Ws(event)).await;
            }
        };

        let handle_task = async {
            info!("net: ready to handle events");
            loop {
                match channel.receive().await {
                    Event::Mgmt(tlv) => {
                        handle_mgmt(tlv, &mut to_mgmt, &mut to_ui, &mut storage, &ws_cmd_tx).await
                    }
                    Event::Ui(tlv) => handle_ui(tlv, &mut to_mgmt, &mut to_ui).await,
                    Event::Ws(event) => handle_ws(event, &mut to_mgmt, &mut led).await,
                }
            }
        };

        futures::join!(mgmt_read_task, ui_read_task, ws_event_task, handle_task);
        unreachable!()
    }
}

async fn handle_mgmt<'a, M, U, F, RM: RawMutex, const N: usize>(
    tlv: Tlv<MgmtToNet>,
    to_mgmt: &mut M,
    to_ui: &mut U,
    storage: &mut NetStorage<F>,
    ws_cmd_tx: &Sender<'a, RM, WsCommand, N>,
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
        MgmtToNet::GetRelayUrl => {
            info!("net: get relay url");
            to_mgmt
                .must_write_tlv(NetToMgmt::RelayUrl, storage.get_relay_url().as_bytes())
                .await;
        }
        MgmtToNet::SetRelayUrl => {
            info!("net: set relay url");
            let Ok(url) = core::str::from_utf8(&tlv.value) else {
                info!("net: invalid utf8 in relay url");
                to_mgmt.must_write_tlv(NetToMgmt::Error, b"utf8").await;
                return;
            };
            if storage.set_relay_url(url).is_err() {
                info!("net: failed to set relay url");
                to_mgmt.must_write_tlv(NetToMgmt::Error, b"set").await;
                return;
            }
            if storage.save().is_err() {
                info!("net: failed to save storage");
                to_mgmt.must_write_tlv(NetToMgmt::Error, b"save").await;
                return;
            }
            // Trigger WebSocket reconnect to new URL
            let Ok(url_string) = String::try_from(url) else {
                info!("net: url too long for command");
                to_mgmt.must_write_tlv(NetToMgmt::Error, b"url").await;
                return;
            };
            ws_cmd_tx.send(WsCommand::Connect(url_string)).await;
            to_mgmt.must_write_tlv(NetToMgmt::Ack, &[]).await;
        }
        MgmtToNet::WsSend => {
            info!("net: ws send ({} bytes)", tlv.value.len());
            let Ok(payload) = Vec::try_from(tlv.value.as_slice()) else {
                info!("net: ws payload too large");
                to_mgmt.must_write_tlv(NetToMgmt::Error, b"size").await;
                return;
            };
            ws_cmd_tx.send(WsCommand::Send(payload)).await;
        }
    }
}

async fn handle_ui<M, U>(tlv: Tlv<UiToNet>, to_mgmt: &mut M, to_ui: &mut U)
where
    M: WriteTlv<NetToMgmt>,
    U: WriteTlv<NetToUi>,
{
    match tlv.tlv_type {
        UiToNet::CircularPing => {
            info!("net: ui circular ping -> mgmt");
            to_mgmt
                .must_write_tlv(NetToMgmt::CircularPing, &tlv.value)
                .await;
        }
        UiToNet::AudioFrameA | UiToNet::AudioFrameB => {
            // Drop audio frames on the floor for now
            // TODO: Process audio frames (e.g., encode and send over network)
            to_ui.must_write_tlv(NetToUi::AudioFrame, &tlv.value).await;
        }
    }
}

async fn handle_ws<M, LR, LG, LB>(event: WsEvent, to_mgmt: &mut M, led: &mut Led<LR, LG, LB>)
where
    M: WriteTlv<NetToMgmt>,
    LR: StatefulOutputPin,
    LG: StatefulOutputPin,
    LB: StatefulOutputPin,
{
    match event {
        WsEvent::Connected => {
            info!("net: ws connected");
            led.set(Color::Green);
            to_mgmt.must_write_tlv(NetToMgmt::WsConnected, &[]).await;
        }
        WsEvent::Disconnected => {
            info!("net: ws disconnected");
            led.set(Color::Red);
            to_mgmt.must_write_tlv(NetToMgmt::WsDisconnected, &[]).await;
        }
        WsEvent::Received(data) => {
            info!("net: ws received {} bytes", data.len());
            to_mgmt.must_write_tlv(NetToMgmt::WsReceived, &data).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mocks::{MockFlash, MockPin};
    use crate::shared::Tlv;
    use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
    use embassy_sync::channel::Channel;

    /// Mock writer that captures TLVs sent to MGMT
    struct MockMgmtWriter {
        written: std::vec::Vec<(NetToMgmt, std::vec::Vec<u8>)>,
    }

    impl MockMgmtWriter {
        fn new() -> Self {
            Self {
                written: std::vec::Vec::new(),
            }
        }
    }

    impl WriteTlv<NetToMgmt> for MockMgmtWriter {
        type Error = ();

        async fn write_tlv(&mut self, tlv_type: NetToMgmt, value: &[u8]) -> Result<(), ()> {
            self.written.push((tlv_type, value.to_vec()));
            Ok(())
        }
    }

    /// Mock writer that captures TLVs sent to UI
    struct MockUiWriter {
        written: std::vec::Vec<(NetToUi, std::vec::Vec<u8>)>,
    }

    impl MockUiWriter {
        fn new() -> Self {
            Self {
                written: std::vec::Vec::new(),
            }
        }
    }

    impl WriteTlv<NetToUi> for MockUiWriter {
        type Error = ();

        async fn write_tlv(&mut self, tlv_type: NetToUi, value: &[u8]) -> Result<(), ()> {
            self.written.push((tlv_type, value.to_vec()));
            Ok(())
        }
    }

    fn mock_led() -> Led<MockPin, MockPin, MockPin> {
        Led::new(MockPin::new(), MockPin::new(), MockPin::new())
    }

    // ==================== WsEvent Tests ====================

    #[tokio::test]
    async fn handle_ws_connected_sends_tlv() {
        let mut to_mgmt = MockMgmtWriter::new();
        let mut led = mock_led();

        handle_ws(WsEvent::Connected, &mut to_mgmt, &mut led).await;

        assert_eq!(to_mgmt.written.len(), 1);
        assert_eq!(to_mgmt.written[0].0, NetToMgmt::WsConnected);
        assert!(to_mgmt.written[0].1.is_empty());
    }

    #[tokio::test]
    async fn handle_ws_disconnected_sends_tlv() {
        let mut to_mgmt = MockMgmtWriter::new();
        let mut led = mock_led();

        handle_ws(WsEvent::Disconnected, &mut to_mgmt, &mut led).await;

        assert_eq!(to_mgmt.written.len(), 1);
        assert_eq!(to_mgmt.written[0].0, NetToMgmt::WsDisconnected);
        assert!(to_mgmt.written[0].1.is_empty());
    }

    #[tokio::test]
    async fn handle_ws_received_sends_tlv_with_data() {
        let mut to_mgmt = MockMgmtWriter::new();
        let mut led = mock_led();

        let data: Vec<u8, MAX_WS_PAYLOAD> = Vec::from_slice(b"hello world").unwrap();
        handle_ws(WsEvent::Received(data), &mut to_mgmt, &mut led).await;

        assert_eq!(to_mgmt.written.len(), 1);
        assert_eq!(to_mgmt.written[0].0, NetToMgmt::WsReceived);
        assert_eq!(to_mgmt.written[0].1, b"hello world");
    }

    // ==================== handle_mgmt Tests ====================

    #[tokio::test]
    async fn handle_mgmt_ping_sends_pong() {
        let mut to_mgmt = MockMgmtWriter::new();
        let mut to_ui = MockUiWriter::new();
        let mut storage = NetStorage::new(MockFlash::new(), 0);
        let channel: Channel<CriticalSectionRawMutex, WsCommand, 4> = Channel::new();

        let tlv = Tlv {
            tlv_type: MgmtToNet::Ping,
            value: heapless::Vec::from_slice(b"test").unwrap(),
        };

        handle_mgmt(
            tlv,
            &mut to_mgmt,
            &mut to_ui,
            &mut storage,
            &channel.sender(),
        )
        .await;

        assert_eq!(to_mgmt.written.len(), 1);
        assert_eq!(to_mgmt.written[0].0, NetToMgmt::Pong);
        assert_eq!(to_mgmt.written[0].1, b"test");
    }

    #[tokio::test]
    async fn handle_mgmt_ws_send_queues_command() {
        let mut to_mgmt = MockMgmtWriter::new();
        let mut to_ui = MockUiWriter::new();
        let mut storage = NetStorage::new(MockFlash::new(), 0);
        let channel: Channel<CriticalSectionRawMutex, WsCommand, 4> = Channel::new();

        let tlv = Tlv {
            tlv_type: MgmtToNet::WsSend,
            value: heapless::Vec::from_slice(b"ws payload").unwrap(),
        };

        handle_mgmt(
            tlv,
            &mut to_mgmt,
            &mut to_ui,
            &mut storage,
            &channel.sender(),
        )
        .await;

        // Should have queued a command
        let cmd = channel.receiver().try_receive().unwrap();
        match cmd {
            WsCommand::Send(data) => {
                assert_eq!(data.as_slice(), b"ws payload");
            }
            _ => panic!("Expected WsCommand::Send"),
        }
    }

    #[tokio::test]
    async fn handle_mgmt_set_relay_url_queues_connect() {
        let mut to_mgmt = MockMgmtWriter::new();
        let mut to_ui = MockUiWriter::new();
        let mut storage = NetStorage::new(MockFlash::new(), 0);
        let channel: Channel<CriticalSectionRawMutex, WsCommand, 4> = Channel::new();

        let tlv = Tlv {
            tlv_type: MgmtToNet::SetRelayUrl,
            value: heapless::Vec::from_slice(b"wss://relay.example.com").unwrap(),
        };

        handle_mgmt(
            tlv,
            &mut to_mgmt,
            &mut to_ui,
            &mut storage,
            &channel.sender(),
        )
        .await;

        // Should have sent Ack
        assert!(to_mgmt.written.iter().any(|(t, _)| *t == NetToMgmt::Ack));

        // Should have queued a Connect command
        let cmd = channel.receiver().try_receive().unwrap();
        match cmd {
            WsCommand::Connect(url) => {
                assert_eq!(url.as_str(), "wss://relay.example.com");
            }
            _ => panic!("Expected WsCommand::Connect"),
        }

        // Verify URL was saved to storage
        assert_eq!(storage.get_relay_url(), "wss://relay.example.com");
    }

    #[tokio::test]
    async fn handle_mgmt_get_relay_url_returns_stored_url() {
        let mut to_mgmt = MockMgmtWriter::new();
        let mut to_ui = MockUiWriter::new();
        let mut storage = NetStorage::new(MockFlash::new(), 0);
        storage.set_relay_url("wss://test.relay").unwrap();
        let channel: Channel<CriticalSectionRawMutex, WsCommand, 4> = Channel::new();

        let tlv = Tlv {
            tlv_type: MgmtToNet::GetRelayUrl,
            value: heapless::Vec::new(),
        };

        handle_mgmt(
            tlv,
            &mut to_mgmt,
            &mut to_ui,
            &mut storage,
            &channel.sender(),
        )
        .await;

        assert_eq!(to_mgmt.written.len(), 1);
        assert_eq!(to_mgmt.written[0].0, NetToMgmt::RelayUrl);
        assert_eq!(to_mgmt.written[0].1, b"wss://test.relay");
    }

    // ==================== WsCommand/WsEvent Construction Tests ====================

    #[test]
    fn ws_command_send_construction() {
        let data: Vec<u8, MAX_WS_PAYLOAD> = Vec::from_slice(b"test data").unwrap();
        let cmd = WsCommand::Send(data.clone());
        match cmd {
            WsCommand::Send(d) => assert_eq!(d, data),
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn ws_command_connect_construction() {
        let url: String<MAX_RELAY_URL_LEN> = String::try_from("wss://example.com").unwrap();
        let cmd = WsCommand::Connect(url.clone());
        match cmd {
            WsCommand::Connect(u) => assert_eq!(u, url),
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn ws_event_received_construction() {
        let data: Vec<u8, MAX_WS_PAYLOAD> = Vec::from_slice(b"received data").unwrap();
        let event = WsEvent::Received(data.clone());
        match event {
            WsEvent::Received(d) => assert_eq!(d, data),
            _ => panic!("Wrong variant"),
        }
    }
}

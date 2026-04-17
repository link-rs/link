//! NET (Network) chip - handles network communication.

mod storage;

// Re-export jitter buffer from shared
pub use crate::shared::{BUFFER_FRAMES, JitterBuffer, JitterState, JitterStats, MIN_START_LEVEL};
pub use storage::{
    MAX_MOQ_NAMESPACE_LEN, MAX_MOQ_TRACK_NAME_LEN, MAX_RELAY_URL_LEN, MAX_WIFI_SSIDS, MoqError,
    MoqExampleType, NetStorage, WifiSsid,
};

use crate::info;
#[cfg(feature = "ui")]
use crate::shared::ChannelId;
use crate::shared::{
    Channel, Color, CriticalSectionRawMutex, CtlToNet, Led, NetLoopbackMode, NetToCtl, NetToUi,
    RawMutex, Receiver, Sender, Tlv, UiToNet, WriteTlv, read_tlv_loop,
};
#[cfg(feature = "ui")]
use embassy_futures::select::{Either, select};
use embedded_hal::digital::StatefulOutputPin;
use embedded_io_async::{Read, Write};
use embedded_storage::{ReadStorage, Storage};
use heapless::{String, Vec};

/// Async ticker that fires at a fixed interval using `std::time`.
#[cfg(feature = "ui")]
mod ticker {
    use core::future::Future;
    use core::pin::Pin;
    use core::task::{Context, Poll};
    use std::time::{Duration, Instant};

    pub struct Ticker {
        interval: Duration,
        next_tick: Instant,
    }

    impl Ticker {
        pub fn every(interval: Duration) -> Self {
            Self {
                interval,
                next_tick: Instant::now() + interval,
            }
        }

        pub fn next(&mut self) -> TickFuture<'_> {
            TickFuture { ticker: self }
        }
    }

    pub struct TickFuture<'a> {
        ticker: &'a mut Ticker,
    }

    impl Future for TickFuture<'_> {
        type Output = ();

        fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
            if Instant::now() >= self.ticker.next_tick {
                let interval = self.ticker.interval;
                self.ticker.next_tick += interval;
                Poll::Ready(())
            } else {
                cx.waker().wake_by_ref();
                Poll::Pending
            }
        }
    }
}

/// Maximum size for WebSocket message payload.
/// Matches the audio frame size (160 bytes = 20ms at 8kHz).
pub const MAX_WS_PAYLOAD: usize = 160;

/// Number of packets to send in echo test (1 second at 20ms interval = 50 fps).
pub const ECHO_TEST_PACKET_COUNT: usize = 50;

/// WebSocket routing mode.
///
/// Determines where received WebSocket data should be routed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub enum WsMode {
    /// Normal operation: route received data to UI for audio playback.
    #[default]
    Normal,
}

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
#[derive(Debug)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub enum WsEvent {
    /// WiFi connected.
    WifiConnected,
    /// WiFi disconnected.
    WifiDisconnected,
    /// WebSocket connected to relay.
    Connected,
    /// WebSocket disconnected from relay.
    Disconnected,
    /// Data received from WebSocket.
    Received(Vec<u8, MAX_WS_PAYLOAD>),
}

enum Event {
    Mgmt(Tlv<CtlToNet>),
    Ui(Tlv<UiToNet>),
    Ws(WsEvent),
}

/// Run the NET chip event loop.
#[allow(unreachable_code)]
pub async fn run<'a, W, R, LR, LG, LB, F, M, const CMD_N: usize, const EVT_N: usize>(
    mut to_mgmt: W,
    from_mgmt: R,
    mut to_ui: W,
    from_ui: R,
    led: (LR, LG, LB),
    flash: F,
    flash_offset: u32,
    ws_cmd_tx: Sender<'a, M, WsCommand, CMD_N>,
    ws_event_rx: Receiver<'a, M, WsEvent, EVT_N>,
) -> !
where
    W: Write,
    R: Read,
    LR: StatefulOutputPin,
    LG: StatefulOutputPin,
    LB: StatefulOutputPin,
    F: ReadStorage + Storage,
    M: RawMutex,
{
    info!("net: starting");

    let mut storage = NetStorage::new(flash, flash_offset);

    // Initialize LED - Red indicates WiFi not connected
    let mut led = Led::new(led.0, led.1, led.2);
    led.set(Color::Red);

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

    // Event handling task - with or without audio jitter buffer
    #[cfg(feature = "ui")]
    let handle_task = async {
        let mut ws_mode = WsMode::Normal;
        let mut loopback = NetLoopbackMode::Off;
        let mut wifi_connected = false;
        let mut ws_connected = false;
        // Per-channel jitter buffers
        let mut ptt_buffer: JitterBuffer = JitterBuffer::new();
        let mut ptt_ai_buffer: JitterBuffer = JitterBuffer::new();
        let mut ticker = ticker::Ticker::every(std::time::Duration::from_millis(20));
        info!("net: ready to handle events (audio buffering enabled)");
        loop {
            match select(channel.receive(), ticker.next()).await {
                Either::First(event) => match event {
                    Event::Mgmt(tlv) => {
                        handle_mgmt(
                            tlv,
                            &mut to_mgmt,
                            &mut to_ui,
                            &mut storage,
                            &ws_cmd_tx,
                            &mut ws_mode,
                            &mut loopback,
                        )
                        .await
                    }
                    Event::Ui(tlv) => {
                        if let Some(audio) =
                            handle_ui(tlv, &mut to_mgmt, &ws_cmd_tx, loopback).await
                        {
                            // Loopback: send directly to UI (bypass jitter buffer)
                            to_ui.must_write_tlv(NetToUi::AudioFrame, &audio).await;
                        }
                    }
                    Event::Ws(event) => {
                        handle_ws_buffered(
                            event,
                            &mut led,
                            &mut ptt_buffer,
                            &mut ptt_ai_buffer,
                            &mut wifi_connected,
                            &mut ws_connected,
                        )
                        .await
                    }
                },
                Either::Second(_) => {
                    // Timer tick - pop from each channel buffer and send to UI
                    for (buffer, channel_id) in [
                        (&mut ptt_buffer, ChannelId::Ptt),
                        (&mut ptt_ai_buffer, ChannelId::PttAi),
                    ] {
                        if buffer.state() == JitterState::Playing || buffer.level() >= 5 {
                            if let Some(frame) = buffer.pop() {
                                // Prepend channel_id to the frame
                                let mut out: heapless::Vec<u8, 256> = heapless::Vec::new();
                                let _ = out.push(channel_id as u8);
                                let _ = out.extend_from_slice(&frame);
                                to_ui.must_write_tlv(NetToUi::AudioFrame, &out).await;
                            }
                        }
                    }
                }
            }
        }
    };

    #[cfg(not(feature = "ui"))]
    let handle_task = async {
        let mut ws_mode = WsMode::Normal;
        let mut loopback = NetLoopbackMode::Off;
        let mut wifi_connected = false;
        let mut ws_connected = false;
        info!("net: ready to handle events");
        loop {
            match channel.receive().await {
                Event::Mgmt(tlv) => {
                    handle_mgmt(
                        tlv,
                        &mut to_mgmt,
                        &mut to_ui,
                        &mut storage,
                        &ws_cmd_tx,
                        &mut ws_mode,
                        &mut loopback,
                    )
                    .await
                }
                Event::Ui(tlv) => {
                    if let Some(audio) = handle_ui(tlv, &mut to_mgmt, &ws_cmd_tx, loopback).await {
                        // Loopback: send directly to UI
                        to_ui.must_write_tlv(NetToUi::AudioFrame, &audio).await;
                    }
                }
                Event::Ws(event) => {
                    handle_ws(
                        event,
                        &mut to_ui,
                        &mut led,
                        &mut wifi_connected,
                        &mut ws_connected,
                    )
                    .await
                }
            }
        }
    };

    futures::join!(mgmt_read_task, ui_read_task, ws_event_task, handle_task);
    unreachable!()
}

async fn handle_mgmt<'a, M, U, F, RM: RawMutex, const N: usize>(
    tlv: Tlv<CtlToNet>,
    to_mgmt: &mut M,
    to_ui: &mut U,
    storage: &mut NetStorage<F>,
    ws_cmd_tx: &Sender<'a, RM, WsCommand, N>,
    _ws_mode: &mut WsMode,
    loopback: &mut NetLoopbackMode,
) where
    M: WriteTlv<NetToCtl>,
    U: WriteTlv<NetToUi>,
    F: ReadStorage + Storage,
{
    match tlv.tlv_type {
        CtlToNet::Ping => {
            info!("net: mgmt ping, sending pong");
            to_mgmt.must_write_tlv(NetToCtl::Pong, &tlv.value).await;
        }
        CtlToNet::CircularPing => {
            info!("net: mgmt circular ping -> ui");
            to_ui
                .must_write_tlv(NetToUi::CircularPing, &tlv.value)
                .await;
        }
        CtlToNet::AddWifiSsid => {
            info!("net: add wifi ssid");
            let Ok((wifi, _)): Result<(WifiSsid, _), _> = serde_json_core::from_slice(&tlv.value)
            else {
                info!("net: failed to deserialize wifi ssid");
                to_mgmt
                    .must_write_tlv(NetToCtl::Error, b"deserialize")
                    .await;
                return;
            };
            if storage.add_wifi_ssid(&wifi.ssid, &wifi.password).is_err() {
                info!("net: failed to add wifi ssid");
                to_mgmt.must_write_tlv(NetToCtl::Error, b"add").await;
                return;
            }
            if storage.save().is_err() {
                info!("net: failed to save storage");
                to_mgmt.must_write_tlv(NetToCtl::Error, b"save").await;
                return;
            }
            to_mgmt.must_write_tlv(NetToCtl::Ack, &[]).await;
        }
        CtlToNet::GetWifiSsids => {
            info!("net: get wifi ssids");
            let ssids = storage.get_wifi_ssids();
            let mut buf = [0u8; 512];
            let Ok(len) = serde_json_core::to_slice(ssids, &mut buf) else {
                info!("net: failed to serialize wifi ssids");
                return;
            };
            to_mgmt
                .must_write_tlv(NetToCtl::WifiSsids, &buf[..len])
                .await;
        }
        CtlToNet::ClearWifiSsids => {
            info!("net: clear wifi ssids");
            storage.clear_wifi_ssids();
            if storage.save().is_err() {
                info!("net: failed to save storage");
                to_mgmt.must_write_tlv(NetToCtl::Error, b"save").await;
                return;
            }
            to_mgmt.must_write_tlv(NetToCtl::Ack, &[]).await;
        }
        CtlToNet::GetRelayUrl => {
            info!("net: get relay url");
            to_mgmt
                .must_write_tlv(NetToCtl::RelayUrl, storage.get_relay_url().as_bytes())
                .await;
        }
        CtlToNet::SetRelayUrl => {
            info!("net: set relay url");
            let Ok(url) = core::str::from_utf8(&tlv.value) else {
                info!("net: invalid utf8 in relay url");
                to_mgmt.must_write_tlv(NetToCtl::Error, b"utf8").await;
                return;
            };
            if storage.set_relay_url(url).is_err() {
                info!("net: failed to set relay url");
                to_mgmt.must_write_tlv(NetToCtl::Error, b"set").await;
                return;
            }
            if storage.save().is_err() {
                info!("net: failed to save storage");
                to_mgmt.must_write_tlv(NetToCtl::Error, b"save").await;
                return;
            }
            // Trigger WebSocket reconnect to new URL
            let Ok(url_string) = String::try_from(url) else {
                info!("net: url too long for command");
                to_mgmt.must_write_tlv(NetToCtl::Error, b"url").await;
                return;
            };
            ws_cmd_tx.send(WsCommand::Connect(url_string)).await;
            to_mgmt.must_write_tlv(NetToCtl::Ack, &[]).await;
        }
        CtlToNet::SetLoopback => {
            let mode = match tlv.value.first().copied().unwrap_or(0) {
                1 => NetLoopbackMode::Raw,
                2 => NetLoopbackMode::Moq,
                _ => NetLoopbackMode::Off,
            };
            info!("net: set loopback = {}", mode);
            *loopback = mode;
            to_mgmt.must_write_tlv(NetToCtl::Ack, &[]).await;
        }
        CtlToNet::GetLoopback => {
            info!("net: get loopback = {}", *loopback);
            to_mgmt
                .must_write_tlv(NetToCtl::Loopback, &[*loopback as u8])
                .await;
        }
        CtlToNet::GetLogsEnabled => {
            info!("net: get logs enabled");
            let enabled = storage.get_logs_enabled();
            to_mgmt
                .must_write_tlv(NetToCtl::LogsEnabled, &[enabled as u8])
                .await;
        }
        CtlToNet::SetLogsEnabled => {
            let enabled = tlv.value.first().copied().unwrap_or(1) != 0;
            info!("net: set logs enabled = {}", enabled);
            storage.set_logs_enabled(enabled);
            if storage.save().is_err() {
                info!("net: failed to save storage");
                to_mgmt.must_write_tlv(NetToCtl::Error, b"save").await;
                return;
            }
            to_mgmt.must_write_tlv(NetToCtl::Ack, &[]).await;
        }
        CtlToNet::ClearStorage => {
            info!("net: clear storage");
            storage.clear();
            if storage.save().is_err() {
                info!("net: failed to save storage");
                to_mgmt.must_write_tlv(NetToCtl::Error, b"save").await;
                return;
            }
            to_mgmt.must_write_tlv(NetToCtl::Ack, &[]).await;
        }
        CtlToNet::GetLanguage => {
            info!("net: get language");
            let lang = storage.get_language();
            to_mgmt
                .must_write_tlv(NetToCtl::Language, lang.as_bytes())
                .await;
        }
        CtlToNet::SetLanguage => {
            let lang = core::str::from_utf8(&tlv.value).unwrap_or("");
            info!("net: set language = {}", lang);
            if storage.set_language(lang).is_err() {
                info!("net: failed to set language (too long)");
                to_mgmt.must_write_tlv(NetToCtl::Error, b"length").await;
                return;
            }
            if storage.save().is_err() {
                info!("net: failed to save storage");
                to_mgmt.must_write_tlv(NetToCtl::Error, b"save").await;
                return;
            }
            to_mgmt.must_write_tlv(NetToCtl::Ack, &[]).await;
        }
        CtlToNet::GetChannel => {
            info!("net: get channel");
            let channel = storage.get_channel();
            to_mgmt
                .must_write_tlv(NetToCtl::Channel, channel.as_bytes())
                .await;
        }
        CtlToNet::SetChannel => {
            let channel = core::str::from_utf8(&tlv.value).unwrap_or("");
            info!("net: set channel");
            if storage.set_channel(channel).is_err() {
                info!("net: failed to set channel (too long)");
                to_mgmt.must_write_tlv(NetToCtl::Error, b"length").await;
                return;
            }
            if storage.save().is_err() {
                info!("net: failed to save storage");
                to_mgmt.must_write_tlv(NetToCtl::Error, b"save").await;
                return;
            }
            to_mgmt.must_write_tlv(NetToCtl::Ack, &[]).await;
        }
        CtlToNet::GetAi => {
            info!("net: get ai");
            let config = storage.get_ai();
            to_mgmt
                .must_write_tlv(NetToCtl::Ai, config.as_bytes())
                .await;
        }
        CtlToNet::SetAi => {
            let config = core::str::from_utf8(&tlv.value).unwrap_or("");
            info!("net: set ai");
            if storage.set_ai(config).is_err() {
                info!("net: failed to set ai (too long)");
                to_mgmt.must_write_tlv(NetToCtl::Error, b"length").await;
                return;
            }
            if storage.save().is_err() {
                info!("net: failed to save storage");
                to_mgmt.must_write_tlv(NetToCtl::Error, b"save").await;
                return;
            }
            to_mgmt.must_write_tlv(NetToCtl::Ack, &[]).await;
        }
        CtlToNet::BurnJtagEfuse => {
            info!("net: burn jtag efuse");
            // Stub: return error - actual implementation would burn efuse
            to_mgmt
                .must_write_tlv(NetToCtl::Error, b"not implemented")
                .await;
        }
    }
}

async fn handle_ui<'a, M, RM: RawMutex, const N: usize>(
    tlv: Tlv<UiToNet>,
    to_mgmt: &mut M,
    ws_cmd_tx: &Sender<'a, RM, WsCommand, N>,
    loopback: NetLoopbackMode,
) -> Option<heapless::Vec<u8, MAX_WS_PAYLOAD>>
where
    M: WriteTlv<NetToCtl>,
{
    match tlv.tlv_type {
        UiToNet::CircularPing => {
            info!("net: ui circular ping -> mgmt");
            to_mgmt
                .must_write_tlv(NetToCtl::CircularPing, &tlv.value)
                .await;
            None
        }
        UiToNet::AudioFrame => {
            // New hactar format: channel_id (1 byte) + encrypted payload
            // For now, handle same as legacy but payload includes channel_id prefix
            let Ok(payload) = heapless::Vec::try_from(tlv.value.as_slice()) else {
                info!("net: audio payload too large");
                return None;
            };

            if loopback != NetLoopbackMode::Off {
                // Return audio data for loopback to jitter buffer
                Some(payload)
            } else {
                // Send to WebSocket
                ws_cmd_tx.send(WsCommand::Send(payload)).await;
                None
            }
        }
        // AudioStart/AudioEnd are ignored by NET (used by CTL for capture timing)
        UiToNet::AudioStart | UiToNet::AudioEnd => None,
    }
}

/// Update LED based on WiFi and WebSocket connection status.
/// RED - WiFi not connected
/// YELLOW - WiFi connected, WS not connected
/// GREEN - WS connected
fn update_led<LR, LG, LB>(led: &mut Led<LR, LG, LB>, wifi_connected: bool, ws_connected: bool)
where
    LR: StatefulOutputPin,
    LG: StatefulOutputPin,
    LB: StatefulOutputPin,
{
    if ws_connected {
        led.set(Color::Green);
    } else if wifi_connected {
        led.set(Color::Yellow);
    } else {
        led.set(Color::Red);
    }
}

#[cfg(any(test, not(feature = "ui")))]
async fn handle_ws<U, LR, LG, LB>(
    event: WsEvent,
    to_ui: &mut U,
    led: &mut Led<LR, LG, LB>,
    wifi_connected: &mut bool,
    ws_connected: &mut bool,
) where
    U: WriteTlv<NetToUi>,
    LR: StatefulOutputPin,
    LG: StatefulOutputPin,
    LB: StatefulOutputPin,
{
    match event {
        WsEvent::WifiConnected => {
            info!("net: wifi connected");
            *wifi_connected = true;
            update_led(led, *wifi_connected, *ws_connected);
        }
        WsEvent::WifiDisconnected => {
            info!("net: wifi disconnected");
            *wifi_connected = false;
            *ws_connected = false; // WS can't be connected without WiFi
            update_led(led, *wifi_connected, *ws_connected);
        }
        WsEvent::Connected => {
            info!("net: ws connected");
            *ws_connected = true;
            update_led(led, *wifi_connected, *ws_connected);
        }
        WsEvent::Disconnected => {
            info!("net: ws disconnected");
            *ws_connected = false;
            update_led(led, *wifi_connected, *ws_connected);
        }
        WsEvent::Received(data) => {
            // Route to UI for audio playback
            to_ui.must_write_tlv(NetToUi::AudioFrame, &data).await;
        }
    }
}

/// Handle WebSocket events with audio buffering.
/// Audio frames are pushed to per-channel jitter buffers instead of being sent directly to UI.
#[cfg(feature = "ui")]
async fn handle_ws_buffered<LR, LG, LB>(
    event: WsEvent,
    led: &mut Led<LR, LG, LB>,
    ptt_buffer: &mut JitterBuffer,
    ptt_ai_buffer: &mut JitterBuffer,
    wifi_connected: &mut bool,
    ws_connected: &mut bool,
) where
    LR: StatefulOutputPin,
    LG: StatefulOutputPin,
    LB: StatefulOutputPin,
{
    match event {
        WsEvent::WifiConnected => {
            info!("net: wifi connected");
            *wifi_connected = true;
            update_led(led, *wifi_connected, *ws_connected);
        }
        WsEvent::WifiDisconnected => {
            info!("net: wifi disconnected");
            *wifi_connected = false;
            *ws_connected = false; // WS can't be connected without WiFi
            ptt_buffer.reset();
            ptt_ai_buffer.reset();
            update_led(led, *wifi_connected, *ws_connected);
        }
        WsEvent::Connected => {
            info!("net: ws connected");
            *ws_connected = true;
            update_led(led, *wifi_connected, *ws_connected);
            // Reset audio buffers on new connection
            ptt_buffer.reset();
            ptt_ai_buffer.reset();
        }
        WsEvent::Disconnected => {
            info!("net: ws disconnected");
            *ws_connected = false;
            update_led(led, *wifi_connected, *ws_connected);
            // Clear buffers on disconnect
            ptt_buffer.reset();
            ptt_ai_buffer.reset();
        }
        WsEvent::Received(data) => {
            // Extract channel_id from first byte and route to appropriate buffer
            if data.len() < 2 {
                info!("net: received data too short");
                return;
            }
            let channel_id = data[0];
            let payload = &data[1..];

            match ChannelId::try_from(channel_id) {
                Ok(ChannelId::Ptt) => {
                    if !ptt_buffer.push(payload) {
                        info!("net: ptt buffer overrun");
                    }
                }
                Ok(ChannelId::PttAi) => {
                    if !ptt_ai_buffer.push(payload) {
                        info!("net: ptt_ai buffer overrun");
                    }
                }
                _ => {
                    info!("net: unknown channel_id {}", channel_id);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shared::Tlv;
    use crate::shared::mocks::{MockFlash, MockPin};
    use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
    use embassy_sync::channel::Channel;

    /// Mock writer that captures TLVs sent to MGMT
    struct MockMgmtWriter {
        written: std::vec::Vec<(NetToCtl, std::vec::Vec<u8>)>,
    }

    impl MockMgmtWriter {
        fn new() -> Self {
            Self {
                written: std::vec::Vec::new(),
            }
        }
    }

    impl WriteTlv<NetToCtl> for MockMgmtWriter {
        type Error = ();

        async fn write_tlv(&mut self, tlv_type: NetToCtl, value: &[u8]) -> Result<(), ()> {
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
    async fn handle_ws_connected_updates_state() {
        let mut to_ui = MockUiWriter::new();
        let mut led = mock_led();
        let mut wifi_connected = true;
        let mut ws_connected = false;

        handle_ws(
            WsEvent::Connected,
            &mut to_ui,
            &mut led,
            &mut wifi_connected,
            &mut ws_connected,
        )
        .await;

        assert!(ws_connected);
    }

    #[tokio::test]
    async fn handle_ws_disconnected_updates_state() {
        let mut to_ui = MockUiWriter::new();
        let mut led = mock_led();
        let mut wifi_connected = true;
        let mut ws_connected = true;

        handle_ws(
            WsEvent::Disconnected,
            &mut to_ui,
            &mut led,
            &mut wifi_connected,
            &mut ws_connected,
        )
        .await;

        assert!(!ws_connected);
    }

    #[tokio::test]
    async fn handle_ws_received_forwards_audio_to_ui() {
        let mut to_ui = MockUiWriter::new();
        let mut led = mock_led();
        let mut wifi_connected = true;
        let mut ws_connected = true;

        // Simulate receiving audio data from WebSocket
        let audio_data: Vec<u8, MAX_WS_PAYLOAD> =
            Vec::from_slice(&[0x01, 0x02, 0x03, 0x04]).unwrap();
        handle_ws(
            WsEvent::Received(audio_data),
            &mut to_ui,
            &mut led,
            &mut wifi_connected,
            &mut ws_connected,
        )
        .await;

        // Should forward to UI as AudioFrame
        assert_eq!(to_ui.written.len(), 1);
        assert_eq!(to_ui.written[0].0, NetToUi::AudioFrame);
        assert_eq!(to_ui.written[0].1, &[0x01, 0x02, 0x03, 0x04]);
    }

    // ==================== handle_ui Audio Tests ====================

    #[tokio::test]
    async fn handle_ui_audio_frame_sends_to_ws() {
        let mut to_mgmt = MockMgmtWriter::new();
        let channel: Channel<CriticalSectionRawMutex, WsCommand, 4> = Channel::new();

        // Simulate audio frame from UI (button A) with loopback disabled
        let audio_data: heapless::Vec<u8, { crate::shared::MAX_VALUE_SIZE }> =
            heapless::Vec::from_slice(&[0xAA; 160]).unwrap();
        let tlv = Tlv {
            tlv_type: UiToNet::AudioFrame,
            value: audio_data,
        };

        let result = handle_ui(tlv, &mut to_mgmt, &channel.sender(), NetLoopbackMode::Off).await;

        // Should not return loopback audio when loopback is disabled
        assert!(result.is_none());
        // Should have queued a WsCommand::Send
        let cmd = channel.receiver().try_receive().unwrap();
        match cmd {
            WsCommand::Send(data) => assert_eq!(data.len(), 160),
            _ => panic!("Expected WsCommand::Send"),
        }
    }

    #[tokio::test]
    async fn handle_ui_audio_frame_loopback() {
        let mut to_mgmt = MockMgmtWriter::new();
        let channel: Channel<CriticalSectionRawMutex, WsCommand, 4> = Channel::new();

        // Simulate audio frame from UI (button A) with loopback enabled
        let audio_data: heapless::Vec<u8, { crate::shared::MAX_VALUE_SIZE }> =
            heapless::Vec::from_slice(&[0xAA; 160]).unwrap();
        let tlv = Tlv {
            tlv_type: UiToNet::AudioFrame,
            value: audio_data,
        };

        let result = handle_ui(tlv, &mut to_mgmt, &channel.sender(), NetLoopbackMode::Raw).await;

        // Should return loopback audio when loopback is enabled
        assert!(result.is_some());
        assert_eq!(result.unwrap().len(), 160);
        // Should NOT have queued a WsCommand
        assert!(channel.receiver().try_receive().is_err());
    }

    // ==================== handle_mgmt Tests ====================

    #[tokio::test]
    async fn handle_mgmt_ping_sends_pong() {
        let mut to_mgmt = MockMgmtWriter::new();
        let mut to_ui = MockUiWriter::new();
        let mut storage = NetStorage::new(MockFlash::new(), 0);
        let channel: Channel<CriticalSectionRawMutex, WsCommand, 4> = Channel::new();
        let mut ws_mode = WsMode::Normal;
        let mut loopback = NetLoopbackMode::Off;

        let tlv = Tlv {
            tlv_type: CtlToNet::Ping,
            value: heapless::Vec::from_slice(b"test").unwrap(),
        };

        handle_mgmt(
            tlv,
            &mut to_mgmt,
            &mut to_ui,
            &mut storage,
            &channel.sender(),
            &mut ws_mode,
            &mut loopback,
        )
        .await;

        assert_eq!(to_mgmt.written.len(), 1);
        assert_eq!(to_mgmt.written[0].0, NetToCtl::Pong);
        assert_eq!(to_mgmt.written[0].1, b"test");
    }

    #[tokio::test]
    async fn handle_mgmt_set_relay_url_queues_connect() {
        let mut to_mgmt = MockMgmtWriter::new();
        let mut to_ui = MockUiWriter::new();
        let mut storage = NetStorage::new(MockFlash::new(), 0);
        let channel: Channel<CriticalSectionRawMutex, WsCommand, 4> = Channel::new();
        let mut ws_mode = WsMode::Normal;
        let mut loopback = NetLoopbackMode::Off;

        let tlv = Tlv {
            tlv_type: CtlToNet::SetRelayUrl,
            value: heapless::Vec::from_slice(b"wss://relay.example.com").unwrap(),
        };

        handle_mgmt(
            tlv,
            &mut to_mgmt,
            &mut to_ui,
            &mut storage,
            &channel.sender(),
            &mut ws_mode,
            &mut loopback,
        )
        .await;

        // Should have sent Ack
        assert!(to_mgmt.written.iter().any(|(t, _)| *t == NetToCtl::Ack));

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
        let mut ws_mode = WsMode::Normal;
        let mut loopback = NetLoopbackMode::Off;

        let tlv = Tlv {
            tlv_type: CtlToNet::GetRelayUrl,
            value: heapless::Vec::new(),
        };

        handle_mgmt(
            tlv,
            &mut to_mgmt,
            &mut to_ui,
            &mut storage,
            &channel.sender(),
            &mut ws_mode,
            &mut loopback,
        )
        .await;

        assert_eq!(to_mgmt.written.len(), 1);
        assert_eq!(to_mgmt.written[0].0, NetToCtl::RelayUrl);
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

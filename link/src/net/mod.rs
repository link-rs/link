//! NET (Network) chip - handles network communication.

mod storage;

// Re-export jitter buffer from shared for backwards compatibility
pub use crate::shared::{BUFFER_FRAMES, JitterBuffer, JitterState, JitterStats, MIN_START_LEVEL};
pub use storage::{
    MAX_PASSWORD_LEN, MAX_RELAY_URL_LEN, MAX_SSID_LEN, MAX_WIFI_SSIDS, NetStorage, WifiSsid,
};

use crate::info;
#[cfg(feature = "audio-buffer")]
use crate::shared::MAX_VALUE_SIZE;
use crate::shared::{
    Channel, Color, CriticalSectionRawMutex, Led, MgmtToNet, NetToMgmt, NetToUi, RawMutex,
    Receiver, Sender, Tlv, UiToNet, WriteTlv, read_tlv_loop,
};
#[cfg(feature = "audio-buffer")]
use embassy_futures::select::{Either, select};
#[cfg(feature = "audio-buffer")]
use embassy_time::{Duration, Ticker};
use embedded_hal::digital::StatefulOutputPin;
use embedded_io_async::{Read, Write};
use embedded_storage::{ReadStorage, Storage};
use heapless::{String, Vec};

/// Maximum size for WebSocket message payload.
pub const MAX_WS_PAYLOAD: usize = 640;

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
    /// Ping mode: route received data to MGMT (for ws-ping command).
    Ping,
}

/// Commands sent to the WebSocket task.
#[derive(Clone)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub enum WsCommand {
    /// Send data over the WebSocket.
    Send(Vec<u8, MAX_WS_PAYLOAD>),
    /// Connect/reconnect to the relay with the given URL.
    Connect(String<MAX_RELAY_URL_LEN>),
    /// Run echo test: send packets, measure inter-arrival times of responses.
    EchoTest,
    /// Run speed test: blast packets as fast as possible, then read responses.
    SpeedTest,
}

/// Result of the WebSocket echo test.
#[derive(Debug, Clone)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub struct EchoTestResult {
    /// Number of packets sent.
    pub sent: u8,
    /// Number of packets received (before buffer).
    pub received: u8,
    /// Number of packets output from buffer.
    pub buffered_output: u8,
    /// Raw inter-arrival times in microseconds (before jitter buffer).
    /// Shows actual network jitter.
    pub raw_jitter_us: Vec<u32, ECHO_TEST_PACKET_COUNT>,
    /// Buffered inter-departure times in microseconds (after jitter buffer).
    /// Should be close to 20000us (20ms) if buffer is working.
    pub buffered_jitter_us: Vec<u32, ECHO_TEST_PACKET_COUNT>,
    /// Number of buffer underruns during the test.
    pub underruns: u8,
}

/// Result of the WebSocket speed test.
#[derive(Debug, Clone)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub struct SpeedTestResult {
    /// Number of packets sent.
    pub sent: u8,
    /// Number of packets received.
    pub received: u8,
    /// Time to send all packets in milliseconds.
    pub send_time_ms: u32,
    /// Time to receive all responses in milliseconds (or timeout).
    pub recv_time_ms: u32,
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
    /// Echo test completed.
    EchoTestResult(EchoTestResult),
    /// Speed test completed.
    SpeedTestResult(SpeedTestResult),
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
        #[cfg(feature = "audio-buffer")]
        let handle_task = async {
            let mut ws_mode = WsMode::Normal;
            let mut loopback = false;
            let mut wifi_connected = false;
            let mut ws_connected = false;
            let mut audio_buffer: JitterBuffer<MAX_VALUE_SIZE> = JitterBuffer::new();
            let mut ticker = Ticker::every(Duration::from_millis(20));
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
                                &mut to_mgmt,
                                &mut led,
                                &mut ws_mode,
                                &mut audio_buffer,
                                &mut wifi_connected,
                                &mut ws_connected,
                            )
                            .await
                        }
                    },
                    Either::Second(_) => {
                        // Timer tick - pop from buffer if playing
                        if audio_buffer.state() == JitterState::Playing || audio_buffer.level() >= 5
                        {
                            if let Some(frame) = audio_buffer.pop() {
                                /*
                                let energy: u32 =
                                    frame.iter().map(|&b| (b as i8).unsigned_abs() as u32).sum();
                                info!(
                                    "net: output {} bytes, energy={}, data={:02x}",
                                    frame.len(),
                                    energy,
                                    frame.as_slice()
                                );
                                */
                                to_ui.must_write_tlv(NetToUi::AudioFrame, &frame).await;
                            }
                        }
                    }
                }
            }
        };

        #[cfg(not(feature = "audio-buffer"))]
        let handle_task = async {
            let mut ws_mode = WsMode::Normal;
            let mut loopback = false;
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
                        if let Some(audio) =
                            handle_ui(tlv, &mut to_mgmt, &ws_cmd_tx, loopback).await
                        {
                            // Loopback: send directly to UI
                            to_ui.must_write_tlv(NetToUi::AudioFrame, &audio).await;
                        }
                    }
                    Event::Ws(event) => {
                        handle_ws(
                            event,
                            &mut to_mgmt,
                            &mut to_ui,
                            &mut led,
                            &mut ws_mode,
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
}

async fn handle_mgmt<'a, M, U, F, RM: RawMutex, const N: usize>(
    tlv: Tlv<MgmtToNet>,
    to_mgmt: &mut M,
    to_ui: &mut U,
    storage: &mut NetStorage<F>,
    ws_cmd_tx: &Sender<'a, RM, WsCommand, N>,
    ws_mode: &mut WsMode,
    loopback: &mut bool,
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
            // Set mode to Ping so response is routed to MGMT
            *ws_mode = WsMode::Ping;
            let Ok(payload) = Vec::try_from(tlv.value.as_slice()) else {
                info!("net: ws payload too large");
                to_mgmt.must_write_tlv(NetToMgmt::Error, b"size").await;
                return;
            };
            ws_cmd_tx.send(WsCommand::Send(payload)).await;
        }
        MgmtToNet::WsEchoTest => {
            info!("net: ws echo test requested");
            ws_cmd_tx.send(WsCommand::EchoTest).await;
        }
        MgmtToNet::WsSpeedTest => {
            info!("net: ws speed test requested");
            ws_cmd_tx.send(WsCommand::SpeedTest).await;
        }
        MgmtToNet::SetLoopback => {
            let enabled = tlv.value.first().copied().unwrap_or(0) != 0;
            info!("net: set loopback = {}", enabled);
            *loopback = enabled;
            to_mgmt.must_write_tlv(NetToMgmt::Ack, &[]).await;
        }
        MgmtToNet::GetLoopback => {
            info!("net: get loopback = {}", *loopback);
            to_mgmt
                .must_write_tlv(NetToMgmt::Loopback, &[*loopback as u8])
                .await;
        }
    }
}

async fn handle_ui<'a, M, RM: RawMutex, const N: usize>(
    tlv: Tlv<UiToNet>,
    to_mgmt: &mut M,
    ws_cmd_tx: &Sender<'a, RM, WsCommand, N>,
    loopback: bool,
) -> Option<heapless::Vec<u8, MAX_WS_PAYLOAD>>
where
    M: WriteTlv<NetToMgmt>,
{
    match tlv.tlv_type {
        UiToNet::CircularPing => {
            info!("net: ui circular ping -> mgmt");
            to_mgmt
                .must_write_tlv(NetToMgmt::CircularPing, &tlv.value)
                .await;
            None
        }
        UiToNet::AudioFrameA | UiToNet::AudioFrameB => {
            /* XXX
            let energy: u32 = tlv
                .value
                .iter()
                .map(|&b| (b as i8).unsigned_abs() as u32)
                .sum();
            info!(
                "net: ui audio {} bytes, energy={}, data={:02x}",
                tlv.value.len(),
                energy,
                tlv.value.as_slice()
            );
            */

            let Ok(payload) = heapless::Vec::try_from(tlv.value.as_slice()) else {
                info!("net: audio payload too large");
                return None;
            };

            if loopback {
                // Return audio data for loopback to jitter buffer
                Some(payload)
            } else {
                // Send to WebSocket
                ws_cmd_tx.send(WsCommand::Send(payload)).await;
                None
            }
        }
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

#[cfg_attr(feature = "audio-buffer", allow(dead_code))]
async fn handle_ws<M, U, LR, LG, LB>(
    event: WsEvent,
    to_mgmt: &mut M,
    to_ui: &mut U,
    led: &mut Led<LR, LG, LB>,
    ws_mode: &mut WsMode,
    wifi_connected: &mut bool,
    ws_connected: &mut bool,
) where
    M: WriteTlv<NetToMgmt>,
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
            to_mgmt.must_write_tlv(NetToMgmt::WsConnected, &[]).await;
        }
        WsEvent::Disconnected => {
            info!("net: ws disconnected");
            *ws_connected = false;
            update_led(led, *wifi_connected, *ws_connected);
            // Reset to Normal mode on disconnect
            *ws_mode = WsMode::Normal;
            to_mgmt.must_write_tlv(NetToMgmt::WsDisconnected, &[]).await;
        }
        WsEvent::Received(data) => {
            match *ws_mode {
                WsMode::Normal => {
                    // Route to UI for audio playback
                    to_ui.must_write_tlv(NetToUi::AudioFrame, &data).await;
                }
                WsMode::Ping => {
                    // Route to MGMT for ws-ping response
                    to_mgmt.must_write_tlv(NetToMgmt::WsReceived, &data).await;
                    // Reset to Normal after handling ping response
                    *ws_mode = WsMode::Normal;
                }
            }
        }
        WsEvent::EchoTestResult(result) => {
            info!(
                "net: ws echo test complete: sent={}, received={}, buffered={}",
                result.sent, result.received, result.buffered_output
            );
            // Serialize result:
            // - sent (1 byte)
            // - received (1 byte)
            // - buffered_output (1 byte)
            // - underruns (1 byte)
            // - raw_jitter_count (1 byte)
            // - raw_jitter_us (4 bytes each)
            // - buffered_jitter_count (1 byte)
            // - buffered_jitter_us (4 bytes each)
            let mut buf = [0u8; 6 + ECHO_TEST_PACKET_COUNT * 8];
            buf[0] = result.sent;
            buf[1] = result.received;
            buf[2] = result.buffered_output;
            buf[3] = result.underruns;
            buf[4] = result.raw_jitter_us.len() as u8;
            let mut offset = 5;
            for &time_us in result.raw_jitter_us.iter() {
                buf[offset..offset + 4].copy_from_slice(&time_us.to_le_bytes());
                offset += 4;
            }
            buf[offset] = result.buffered_jitter_us.len() as u8;
            offset += 1;
            for &time_us in result.buffered_jitter_us.iter() {
                buf[offset..offset + 4].copy_from_slice(&time_us.to_le_bytes());
                offset += 4;
            }
            to_mgmt
                .must_write_tlv(NetToMgmt::WsEchoTestResult, &buf[..offset])
                .await;
        }
        WsEvent::SpeedTestResult(result) => {
            info!(
                "net: ws speed test complete: sent={}, received={}, send_time={}ms, recv_time={}ms",
                result.sent, result.received, result.send_time_ms, result.recv_time_ms
            );
            // Serialize result: sent (1), received (1), send_time_ms (4), recv_time_ms (4)
            let mut buf = [0u8; 10];
            buf[0] = result.sent;
            buf[1] = result.received;
            buf[2..6].copy_from_slice(&result.send_time_ms.to_le_bytes());
            buf[6..10].copy_from_slice(&result.recv_time_ms.to_le_bytes());
            to_mgmt
                .must_write_tlv(NetToMgmt::WsSpeedTestResult, &buf)
                .await;
        }
    }
}

/// Handle WebSocket events with audio buffering.
/// Audio frames are pushed to the jitter buffer instead of being sent directly to UI.
#[cfg(feature = "audio-buffer")]
async fn handle_ws_buffered<M, LR, LG, LB, const N: usize>(
    event: WsEvent,
    to_mgmt: &mut M,
    led: &mut Led<LR, LG, LB>,
    ws_mode: &mut WsMode,
    audio_buffer: &mut JitterBuffer<N>,
    wifi_connected: &mut bool,
    ws_connected: &mut bool,
) where
    M: WriteTlv<NetToMgmt>,
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
            audio_buffer.reset();
            update_led(led, *wifi_connected, *ws_connected);
        }
        WsEvent::Connected => {
            info!("net: ws connected");
            *ws_connected = true;
            update_led(led, *wifi_connected, *ws_connected);
            // Reset audio buffer on new connection
            audio_buffer.reset();
            to_mgmt.must_write_tlv(NetToMgmt::WsConnected, &[]).await;
        }
        WsEvent::Disconnected => {
            info!("net: ws disconnected");
            *ws_connected = false;
            update_led(led, *wifi_connected, *ws_connected);
            // Reset to Normal mode and clear buffer on disconnect
            *ws_mode = WsMode::Normal;
            audio_buffer.reset();
            to_mgmt.must_write_tlv(NetToMgmt::WsDisconnected, &[]).await;
        }
        WsEvent::Received(data) => {
            match *ws_mode {
                WsMode::Normal => {
                    // Push audio to jitter buffer instead of sending directly
                    if !audio_buffer.push(&data) {
                        info!("net: audio buffer overrun");
                    }
                }
                WsMode::Ping => {
                    // Route to MGMT for ws-ping response (not buffered)
                    to_mgmt.must_write_tlv(NetToMgmt::WsReceived, &data).await;
                    // Reset to Normal after handling ping response
                    *ws_mode = WsMode::Normal;
                }
            }
        }
        WsEvent::EchoTestResult(result) => {
            info!(
                "net: ws echo test complete: sent={}, received={}, buffered={}",
                result.sent, result.received, result.buffered_output
            );
            // Serialize result (same as non-buffered version)
            let mut buf = [0u8; 6 + ECHO_TEST_PACKET_COUNT * 8];
            buf[0] = result.sent;
            buf[1] = result.received;
            buf[2] = result.buffered_output;
            buf[3] = result.underruns;
            buf[4] = result.raw_jitter_us.len() as u8;
            let mut offset = 5;
            for &time_us in result.raw_jitter_us.iter() {
                buf[offset..offset + 4].copy_from_slice(&time_us.to_le_bytes());
                offset += 4;
            }
            buf[offset] = result.buffered_jitter_us.len() as u8;
            offset += 1;
            for &time_us in result.buffered_jitter_us.iter() {
                buf[offset..offset + 4].copy_from_slice(&time_us.to_le_bytes());
                offset += 4;
            }
            to_mgmt
                .must_write_tlv(NetToMgmt::WsEchoTestResult, &buf[..offset])
                .await;
        }
        WsEvent::SpeedTestResult(result) => {
            info!(
                "net: ws speed test complete: sent={}, received={}, send_time={}ms, recv_time={}ms",
                result.sent, result.received, result.send_time_ms, result.recv_time_ms
            );
            // Serialize result: sent (1), received (1), send_time_ms (4), recv_time_ms (4)
            let mut buf = [0u8; 10];
            buf[0] = result.sent;
            buf[1] = result.received;
            buf[2..6].copy_from_slice(&result.send_time_ms.to_le_bytes());
            buf[6..10].copy_from_slice(&result.recv_time_ms.to_le_bytes());
            to_mgmt
                .must_write_tlv(NetToMgmt::WsSpeedTestResult, &buf)
                .await;
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
        let mut to_ui = MockUiWriter::new();
        let mut led = mock_led();
        let mut ws_mode = WsMode::Normal;
        let mut wifi_connected = true;
        let mut ws_connected = false;

        handle_ws(
            WsEvent::Connected,
            &mut to_mgmt,
            &mut to_ui,
            &mut led,
            &mut ws_mode,
            &mut wifi_connected,
            &mut ws_connected,
        )
        .await;

        assert_eq!(to_mgmt.written.len(), 1);
        assert_eq!(to_mgmt.written[0].0, NetToMgmt::WsConnected);
        assert!(to_mgmt.written[0].1.is_empty());
        assert!(ws_connected);
    }

    #[tokio::test]
    async fn handle_ws_disconnected_sends_tlv() {
        let mut to_mgmt = MockMgmtWriter::new();
        let mut to_ui = MockUiWriter::new();
        let mut led = mock_led();
        let mut ws_mode = WsMode::Normal;
        let mut wifi_connected = true;
        let mut ws_connected = true;

        handle_ws(
            WsEvent::Disconnected,
            &mut to_mgmt,
            &mut to_ui,
            &mut led,
            &mut ws_mode,
            &mut wifi_connected,
            &mut ws_connected,
        )
        .await;

        assert_eq!(to_mgmt.written.len(), 1);
        assert_eq!(to_mgmt.written[0].0, NetToMgmt::WsDisconnected);
        assert!(to_mgmt.written[0].1.is_empty());
        assert!(!ws_connected);
    }

    #[tokio::test]
    async fn handle_ws_disconnected_resets_mode() {
        let mut to_mgmt = MockMgmtWriter::new();
        let mut to_ui = MockUiWriter::new();
        let mut led = mock_led();
        let mut ws_mode = WsMode::Ping; // Start in Ping mode
        let mut wifi_connected = true;
        let mut ws_connected = true;

        handle_ws(
            WsEvent::Disconnected,
            &mut to_mgmt,
            &mut to_ui,
            &mut led,
            &mut ws_mode,
            &mut wifi_connected,
            &mut ws_connected,
        )
        .await;

        // Mode should be reset to Normal on disconnect
        assert_eq!(ws_mode, WsMode::Normal);
    }

    #[tokio::test]
    async fn handle_ws_received_forwards_audio_to_ui_in_normal_mode() {
        let mut to_mgmt = MockMgmtWriter::new();
        let mut to_ui = MockUiWriter::new();
        let mut led = mock_led();
        let mut ws_mode = WsMode::Normal;
        let mut wifi_connected = true;
        let mut ws_connected = true;

        // Simulate receiving audio data from WebSocket
        let audio_data: Vec<u8, MAX_WS_PAYLOAD> =
            Vec::from_slice(&[0x01, 0x02, 0x03, 0x04]).unwrap();
        handle_ws(
            WsEvent::Received(audio_data),
            &mut to_mgmt,
            &mut to_ui,
            &mut led,
            &mut ws_mode,
            &mut wifi_connected,
            &mut ws_connected,
        )
        .await;

        // Should forward to UI as AudioFrame
        assert_eq!(to_ui.written.len(), 1);
        assert_eq!(to_ui.written[0].0, NetToUi::AudioFrame);
        assert_eq!(to_ui.written[0].1, &[0x01, 0x02, 0x03, 0x04]);
        // Should NOT send to MGMT
        assert!(to_mgmt.written.is_empty());
    }

    #[tokio::test]
    async fn handle_ws_received_forwards_to_mgmt_in_ping_mode() {
        let mut to_mgmt = MockMgmtWriter::new();
        let mut to_ui = MockUiWriter::new();
        let mut led = mock_led();
        let mut ws_mode = WsMode::Ping;
        let mut wifi_connected = true;
        let mut ws_connected = true;

        // Simulate receiving ping response from WebSocket
        let ping_data: Vec<u8, MAX_WS_PAYLOAD> =
            Vec::from_slice(&[0x01, 0x02, 0x03, 0x04]).unwrap();
        handle_ws(
            WsEvent::Received(ping_data),
            &mut to_mgmt,
            &mut to_ui,
            &mut led,
            &mut ws_mode,
            &mut wifi_connected,
            &mut ws_connected,
        )
        .await;

        // Should forward to MGMT as WsReceived
        assert_eq!(to_mgmt.written.len(), 1);
        assert_eq!(to_mgmt.written[0].0, NetToMgmt::WsReceived);
        assert_eq!(to_mgmt.written[0].1, &[0x01, 0x02, 0x03, 0x04]);
        // Should NOT send to UI
        assert!(to_ui.written.is_empty());
        // Mode should be reset to Normal
        assert_eq!(ws_mode, WsMode::Normal);
    }

    // ==================== handle_ui Audio Tests ====================

    #[tokio::test]
    async fn handle_ui_audio_frame_sends_to_ws() {
        let mut to_mgmt = MockMgmtWriter::new();
        let channel: Channel<CriticalSectionRawMutex, WsCommand, 4> = Channel::new();

        // Simulate audio frame from UI (button A) with loopback disabled
        let audio_data: heapless::Vec<u8, { crate::shared::MAX_VALUE_SIZE }> =
            heapless::Vec::from_slice(&[0xAA; 640]).unwrap();
        let tlv = Tlv {
            tlv_type: UiToNet::AudioFrameA,
            value: audio_data,
        };

        let result = handle_ui(tlv, &mut to_mgmt, &channel.sender(), false).await;

        // Should not return loopback audio when loopback is disabled
        assert!(result.is_none());
        // Should have queued a WsCommand::Send
        let cmd = channel.receiver().try_receive().unwrap();
        match cmd {
            WsCommand::Send(data) => assert_eq!(data.len(), 640),
            _ => panic!("Expected WsCommand::Send"),
        }
    }

    #[tokio::test]
    async fn handle_ui_audio_frame_loopback() {
        let mut to_mgmt = MockMgmtWriter::new();
        let channel: Channel<CriticalSectionRawMutex, WsCommand, 4> = Channel::new();

        // Simulate audio frame from UI (button A) with loopback enabled
        let audio_data: heapless::Vec<u8, { crate::shared::MAX_VALUE_SIZE }> =
            heapless::Vec::from_slice(&[0xAA; 640]).unwrap();
        let tlv = Tlv {
            tlv_type: UiToNet::AudioFrameA,
            value: audio_data,
        };

        let result = handle_ui(tlv, &mut to_mgmt, &channel.sender(), true).await;

        // Should return loopback audio when loopback is enabled
        assert!(result.is_some());
        assert_eq!(result.unwrap().len(), 640);
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
        let mut loopback = false;

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
            &mut ws_mode,
            &mut loopback,
        )
        .await;

        assert_eq!(to_mgmt.written.len(), 1);
        assert_eq!(to_mgmt.written[0].0, NetToMgmt::Pong);
        assert_eq!(to_mgmt.written[0].1, b"test");
    }

    #[tokio::test]
    async fn handle_mgmt_ws_send_queues_command_and_sets_ping_mode() {
        let mut to_mgmt = MockMgmtWriter::new();
        let mut to_ui = MockUiWriter::new();
        let mut storage = NetStorage::new(MockFlash::new(), 0);
        let channel: Channel<CriticalSectionRawMutex, WsCommand, 4> = Channel::new();
        let mut ws_mode = WsMode::Normal;
        let mut loopback = false;

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
            &mut ws_mode,
            &mut loopback,
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

        // Mode should be set to Ping
        assert_eq!(ws_mode, WsMode::Ping);
    }

    #[tokio::test]
    async fn handle_mgmt_set_relay_url_queues_connect() {
        let mut to_mgmt = MockMgmtWriter::new();
        let mut to_ui = MockUiWriter::new();
        let mut storage = NetStorage::new(MockFlash::new(), 0);
        let channel: Channel<CriticalSectionRawMutex, WsCommand, 4> = Channel::new();
        let mut ws_mode = WsMode::Normal;
        let mut loopback = false;

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
            &mut ws_mode,
            &mut loopback,
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
        let mut ws_mode = WsMode::Normal;
        let mut loopback = false;

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
            &mut ws_mode,
            &mut loopback,
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

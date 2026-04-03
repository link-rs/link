//! NET chip firmware using ESP-IDF.
//!
//! This firmware provides:
//! - WiFi connectivity with stored credentials
//! - UART communication with MGMT and UI chips
//! - LED status indication
//! - NVS storage for WiFi credentials and relay URL
//! - Audio loopback mode
//! - MoQ (Media over QUIC) transport via quicr

use esp_idf_svc::{
    eventloop::EspSystemEventLoop,
    hal::{
        gpio::{OutputPin, PinDriver},
        prelude::Peripherals,
        task::block_on,
        uart::{self, UartDriver},
    },
    nvs::{EspDefaultNvsPartition, EspNvs, NvsDefault},
};
use link::{
    net::{JitterBuffer, JitterState, WifiSsid, MAX_RELAY_URL_LEN, MAX_WIFI_SSIDS},
    uart_config, ChannelId, Color, CtlToNet, NetLoopbackMode, NetToCtl, NetToUi, UiToNet,
    HEADER_SIZE, MAX_VALUE_SIZE, SYNC_WORD,
};
use log::{info, warn};
use quicr::{ClientBuilder, FullTrackName, ObjectHeaders, Subscription, TrackNamespace};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::{Duration, Instant};

// ============================================================================
// MoQ Command/Event Types
// ============================================================================

/// Which PTT channel is currently active for publishing.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
enum PttChannel {
    #[default]
    /// PTT channel (button A) - group voice chat
    Ptt,
    /// AI channel (button B) - AI assistant
    Ai,
}

/// PTT channel configuration (channel namespace + language as track name).
#[derive(Clone)]
struct PttConfig {
    /// Channel namespace (parsed from JSON array)
    namespace: Vec<String>,
    /// Track name (language code, e.g., "en-US")
    track_name: String,
}

impl PttConfig {
    /// Parse PTT config from storage. Returns None if config is incomplete.
    fn from_storage(language: &str, channel_json: &str) -> Option<Self> {
        if language.is_empty() || channel_json.is_empty() {
            return None;
        }

        let namespace: Vec<String> = serde_json::from_str(channel_json).ok()?;
        if namespace.is_empty() {
            return None;
        }

        Some(Self {
            namespace,
            track_name: language.to_string(),
        })
    }
}

/// AI configuration (query/audio/cmd namespaces + language as track name).
#[derive(Clone)]
struct AiConfig {
    /// AI query namespace
    query_ns: Vec<String>,
    /// AI audio response namespace
    audio_ns: Vec<String>,
    /// AI command response namespace
    cmd_ns: Vec<String>,
    /// Track name (language code, e.g., "en-US")
    track_name: String,
}

impl AiConfig {
    /// Parse AI config from storage. Returns None if config is incomplete.
    fn from_storage(language: &str, ai_json: &str) -> Option<Self> {
        if language.is_empty() || ai_json.is_empty() {
            return None;
        }

        let ai: serde_json::Value = serde_json::from_str(ai_json).ok()?;

        let query_ns: Vec<String> = ai
            .get("query")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();

        // Need at least query namespace for AI to work
        if query_ns.is_empty() {
            return None;
        }

        let audio_ns: Vec<String> = ai
            .get("audio")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();

        let cmd_ns: Vec<String> = ai
            .get("cmd")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();

        Some(Self {
            query_ns,
            audio_ns,
            cmd_ns,
            track_name: language.to_string(),
        })
    }
}

/// Commands sent to the MoQ task.
#[derive(Clone)]
#[allow(dead_code)]
enum MoqCommand {
    /// Set the relay URL (triggers reconnect if changed).
    SetRelayUrl(String),
    /// Send a chat message.
    SendChat { message: String },
    /// Audio frame to publish (used in PTT mode).
    /// channel_id byte determines which track to publish on.
    AudioFrame { data: Vec<u8> },
    /// Set active PTT channel for publishing.
    SetPttChannel(PttChannel),
    /// Set loopback mode.
    SetLoopback(NetLoopbackMode),
    /// Update track configuration (channel, language, AI settings changed).
    UpdateConfig {
        ptt: Option<PttConfig>,
        ai: Option<AiConfig>,
    },
}

/// Events sent from the MoQ task back to the main loop.
#[allow(dead_code)]
enum MoqEvent {
    /// Connected to relay.
    Connected,
    /// Disconnected from relay.
    Disconnected,
    /// Error occurred.
    Error { message: String },
    /// Chat message sent successfully.
    ChatSent,
    /// Chat message received.
    ChatReceived { message: String },
    /// Audio frame received from MoQ subscription.
    AudioReceived { data: Vec<u8> },
    /// WiFi connected (initial or reconnect).
    WifiConnected,
    /// WiFi disconnected.
    WifiDisconnected,
}

// ============================================================================
// MoQ Task
// ============================================================================

/// Device endpoint ID for MoQ connections.
const MOQ_ENDPOINT_ID: &str = "hactar-link-net";

/// Get device ID from MAC address (matches hactar's approach).
/// Returns a u64 derived from the ESP32's MAC address.
fn get_device_id_from_mac() -> u64 {
    let mut mac = [0u8; 6];
    unsafe {
        esp_idf_svc::sys::esp_efuse_mac_get_default(mac.as_mut_ptr());
    }
    // Convert 6-byte MAC to u64, then clear top 2 bits like hactar does
    let mut device_id: u64 = 0;
    for (i, &byte) in mac.iter().enumerate() {
        device_id |= (byte as u64) << (i * 8);
    }
    // Clear top 2 bits: (mac << 2) >> 2
    (device_id << 2) >> 2
}

/// Helper function to set up PTT tracks after connection.
/// Returns true if at least one track was set up successfully.
fn setup_ptt_tracks(
    client: &quicr::Client,
    ptt_config: Option<&PttConfig>,
    ai_config: Option<&AiConfig>,
    ptt_pub_track: &mut Option<std::sync::Arc<quicr::PublishTrack>>,
    ai_pub_track: &mut Option<std::sync::Arc<quicr::PublishTrack>>,
    ptt_subscription: &mut Option<Subscription>,
    ai_audio_subscription: &mut Option<Subscription>,
    ai_cmd_subscription: &mut Option<Subscription>,
    ptt_group_id: &mut u64,
    ptt_object_id: &mut u64,
    ai_group_id: &mut u64,
    ai_object_id: &mut u64,
) -> bool {
    // Track naming (from hactar):
    // Channel: publish and subscribe to {channel_ns, language}
    // AI query: publish to {ai_query_ns, language}
    // AI audio response: subscribe to {ai_audio_ns, device_id}

    // Get device ID from MAC for group_id (janet uses this to route AI responses)
    let device_id = get_device_id_from_mac();
    info!("device id: {}", device_id);

    let mut success = false;

    // Set up channel tracks if configured
    if let Some(ptt) = ptt_config {
        let ptt_ns = TrackNamespace::from_strings(
            &ptt.namespace.iter().map(|s| s.as_str()).collect::<Vec<_>>(),
        );
        client.publish_namespace(&ptt_ns);

        // Create PTT publish track with language as track name
        let track_name_bytes = ptt.track_name.clone().into_bytes();
        let ptt_track_name_full = FullTrackName::new(ptt_ns.clone(), track_name_bytes.clone());
        info!(
            "MoQ PTT: publish namespace={}, track={} (device_id={})",
            ptt_ns, ptt.track_name, device_id
        );
        match block_on(client.publish(ptt_track_name_full)) {
            Ok(track) => {
                *ptt_pub_track = Some(track);
                *ptt_group_id = device_id;
                *ptt_object_id = 0;
                info!(
                    "MoQ PTT: created PTT publish track (group_id={})",
                    device_id
                );
                success = true;
            }
            Err(e) => {
                warn!("MoQ PTT: failed to create PTT publish track: {:?}", e);
            }
        }

        // Subscribe to PTT channel (receive from others)
        let ptt_sub_track_name = FullTrackName::new(ptt_ns.clone(), track_name_bytes);
        info!(
            "MoQ PTT: subscribe namespace={}, track={}",
            ptt_ns, ptt.track_name
        );
        match block_on(client.subscribe(ptt_sub_track_name)) {
            Ok(sub) => {
                *ptt_subscription = Some(sub);
                info!("MoQ PTT: subscribed to PTT channel");
            }
            Err(e) => {
                warn!("MoQ PTT: failed to subscribe to PTT channel: {:?}", e);
            }
        }
    } else {
        warn!("MoQ PTT: channel not configured (missing namespace or language)");
    }

    // Set up AI tracks if configured
    if let Some(ai) = ai_config {
        // AI query publication track (publish to {ai_query_ns, language})
        let ai_query_ns = TrackNamespace::from_strings(
            &ai.query_ns.iter().map(|s| s.as_str()).collect::<Vec<_>>(),
        );
        client.publish_namespace(&ai_query_ns);

        let track_name_bytes = ai.track_name.clone().into_bytes();
        let ai_pub_track_name = FullTrackName::new(ai_query_ns.clone(), track_name_bytes);
        info!(
            "MoQ AI: publish namespace={}, track={}",
            ai_query_ns, ai.track_name
        );
        match block_on(client.publish(ai_pub_track_name)) {
            Ok(track) => {
                *ai_pub_track = Some(track);
                *ai_group_id = device_id;
                *ai_object_id = 0;
                info!(
                    "MoQ AI: created AI query publish track (group_id={})",
                    device_id
                );
                success = true;
            }
            Err(e) => {
                warn!("MoQ AI: failed to create AI query publish track: {:?}", e);
            }
        }

        // AI audio response subscription track (subscribe to {ai_audio_ns, device_id})
        if !ai.audio_ns.is_empty() {
            let ai_audio_ns = TrackNamespace::from_strings(
                &ai.audio_ns.iter().map(|s| s.as_str()).collect::<Vec<_>>(),
            );
            client.subscribe_namespace(&ai_audio_ns);

            let device_id_str = format!("{}", device_id);
            info!(
                "MoQ AI: subscribing to AI audio responses on ns={} track=\"{}\" (device_id=0x{:012x})",
                ai_audio_ns, device_id_str, device_id
            );
            let ai_sub_track_name = FullTrackName::new(ai_audio_ns, device_id_str.into_bytes());
            match block_on(client.subscribe(ai_sub_track_name)) {
                Ok(sub) => {
                    *ai_audio_subscription = Some(sub);
                    info!("MoQ AI: subscribed to AI audio responses");
                }
                Err(e) => {
                    warn!("MoQ AI: failed to subscribe to AI audio: {:?}", e);
                }
            }
        }

        // AI command response subscription track (subscribe to {ai_cmd_ns, device_id})
        if !ai.cmd_ns.is_empty() {
            let ai_cmd_ns = TrackNamespace::from_strings(
                &ai.cmd_ns.iter().map(|s| s.as_str()).collect::<Vec<_>>(),
            );
            client.subscribe_namespace(&ai_cmd_ns);

            let device_id_str = format!("{}", device_id);
            info!(
                "MoQ AI: subscribing to AI cmd responses on ns={} track=\"{}\"",
                ai_cmd_ns, device_id_str
            );
            let ai_cmd_track_name = FullTrackName::new(ai_cmd_ns, device_id_str.into_bytes());
            match block_on(client.subscribe(ai_cmd_track_name)) {
                Ok(sub) => {
                    *ai_cmd_subscription = Some(sub);
                    info!("MoQ AI: subscribed to AI cmd responses");
                }
                Err(e) => {
                    warn!("MoQ AI: failed to subscribe to AI cmd: {:?}", e);
                }
            }
        }
    } else {
        warn!("MoQ AI: AI not configured (missing namespace or language)");
    }

    success
}

/// Spawn the MoQ task in a separate thread.
/// PTT mode is automatically started when connected to the relay.
fn spawn_moq_task(
    wifi: Option<(
        esp_idf_svc::wifi::AsyncWifi<esp_idf_svc::wifi::EspWifi<'static>>,
        String,
        String,
    )>,
    initial_relay_url: Option<String>,
    cmd_rx: Receiver<MoqCommand>,
    event_tx: Sender<MoqEvent>,
) {
    use std::sync::mpsc::TryRecvError;
    use std::time::Instant;

    thread::Builder::new()
        .name("moq".to_string())
        .stack_size(16384)
        .spawn(move || {
            info!("MoQ task started");

            // WiFi state
            let mut wifi = wifi;
            let mut wifi_connected = false;
            let mut last_wifi_check = Instant::now();

            // Start WiFi (non-blocking: configure + start + trigger connect)
            if let Some((ref mut w, ref ssid, ref password)) = wifi {
                info!("net: starting WiFi for '{}'", ssid);
                if let Err(e) = start_wifi(w, ssid, password) {
                    warn!("net: WiFi start failed: {:?}", e);
                }
            }

            let mut client: Option<quicr::Client> = None;
            let mut relay_url: Option<String> = initial_relay_url;
            let mut last_reconnect_attempt: Option<Instant> = None;
            let mut ptt_ready = false;

            // Track configuration (updated via UpdateConfig command)
            let mut ptt_config: Option<PttConfig> = None;
            let mut ai_config: Option<AiConfig> = None;

            // Loopback mode (communicated from main loop)
            let mut loopback = NetLoopbackMode::Off;

            // PTT mode state (hactar-compatible)
            let mut ptt_pub_track: Option<std::sync::Arc<quicr::PublishTrack>> = None;
            let mut ai_pub_track: Option<std::sync::Arc<quicr::PublishTrack>> = None;
            let mut ptt_subscription: Option<Subscription> = None;
            let mut ai_audio_subscription: Option<Subscription> = None;
            let mut ai_cmd_subscription: Option<Subscription> = None;
            let mut ptt_group_id: u64 = 0;
            let mut ptt_object_id: u64 = 0;
            let mut ai_group_id: u64 = 0;
            let mut ai_object_id: u64 = 0;
            let mut active_ptt_channel = PttChannel::Ptt;
            let mut ptt_recv_count: u64 = 0;
            let mut ai_audio_recv_count: u64 = 0;
            let mut ai_cmd_recv_count: u64 = 0;
            let mut last_stats = Instant::now();

            loop {
                // Check for commands (non-blocking)
                match cmd_rx.try_recv() {
                    Ok(MoqCommand::SetRelayUrl(url)) => {
                        info!("MoQ: setting relay URL to {}", url);
                        // Disconnect existing client and reset PTT state
                        ptt_ready = false;
                        ptt_pub_track = None;
                        ai_pub_track = None;
                        ptt_subscription = None;
                        ai_audio_subscription = None;
                        ai_cmd_subscription = None;
                        if client.is_some() {
                            client = None;
                            let _ = event_tx.send(MoqEvent::Disconnected);
                        }

                        // Store URL for reconnection
                        relay_url = Some(url.clone());
                        last_reconnect_attempt = None;

                        match ClientBuilder::new()
                            .endpoint_id(MOQ_ENDPOINT_ID)
                            .connect_uri(&url)
                            .time_queue_max_duration(5000)
                            .tick_service_sleep_delay_us(30000)
                            .build()
                        {
                            Ok(c) => {
                                match block_on(c.connect()) {
                                    Ok(()) => {
                                        info!("MoQ: connected to {}", url);
                                        let _ = event_tx.send(MoqEvent::Connected);

                                        // Auto-start PTT mode on connect
                                        ptt_ready = setup_ptt_tracks(
                                            &c,
                                            ptt_config.as_ref(),
                                            ai_config.as_ref(),
                                            &mut ptt_pub_track,
                                            &mut ai_pub_track,
                                            &mut ptt_subscription,
                                            &mut ai_audio_subscription,
                                            &mut ai_cmd_subscription,
                                            &mut ptt_group_id,
                                            &mut ptt_object_id,
                                            &mut ai_group_id,
                                            &mut ai_object_id,
                                        );
                                        client = Some(c);
                                    }
                                    Err(e) => {
                                        warn!("MoQ: failed to connect: {:?}", e);
                                        let _ = event_tx.send(MoqEvent::Error {
                                            message: format!("{:?}", e),
                                        });
                                        last_reconnect_attempt = Some(Instant::now());
                                    }
                                }
                            }
                            Err(e) => {
                                warn!("MoQ: failed to create client: {:?}", e);
                                last_reconnect_attempt = Some(Instant::now());
                            }
                        }
                    }
                    Ok(MoqCommand::SetPttChannel(channel)) => {
                        active_ptt_channel = channel;
                        info!("MoQ PTT: active channel set to {:?}", channel);
                    }
                    Ok(MoqCommand::SetLoopback(mode)) => {
                        loopback = mode;
                        info!("MoQ: loopback mode set to {:?}", loopback);
                    }
                    Ok(MoqCommand::UpdateConfig { ptt, ai }) => {
                        info!(
                            "MoQ: updating track config (ptt={}, ai={})",
                            ptt.is_some(),
                            ai.is_some()
                        );
                        ptt_config = ptt;
                        ai_config = ai;

                        // Re-setup tracks if connected
                        if let Some(ref c) = client {
                            // Clear existing tracks
                            ptt_pub_track = None;
                            ai_pub_track = None;
                            ptt_subscription = None;
                            ai_audio_subscription = None;
                            ai_cmd_subscription = None;

                            // Re-setup with new config
                            ptt_ready = setup_ptt_tracks(
                                c,
                                ptt_config.as_ref(),
                                ai_config.as_ref(),
                                &mut ptt_pub_track,
                                &mut ai_pub_track,
                                &mut ptt_subscription,
                                &mut ai_audio_subscription,
                                &mut ai_cmd_subscription,
                                &mut ptt_group_id,
                                &mut ptt_object_id,
                                &mut ai_group_id,
                                &mut ai_object_id,
                            );
                            info!("MoQ: tracks reconfigured, ptt_ready={}", ptt_ready);
                        }
                    }
                    Ok(MoqCommand::AudioFrame { data }) => {
                        // Only publish if PTT is ready and loopback is not Raw
                        if ptt_ready && loopback != NetLoopbackMode::Raw {
                            // Data format: [channel_id][payload...]
                            // channel_id 0 = PTT, 1 = AI
                            if data.is_empty() {
                                warn!("MoQ PTT: received empty audio frame");
                                continue;
                            }
                            let channel_id = data[0];
                            let payload = &data[1..];
                            match ChannelId::try_from(channel_id) {
                                Ok(ChannelId::Ptt) => {
                                    if let Some(ref track) = ptt_pub_track {
                                        let headers =
                                            ObjectHeaders::new(ptt_group_id, ptt_object_id);
                                        let _ = track.publish(&headers, payload);
                                        ptt_object_id += 1;
                                    } else {
                                        warn!("MoQ PTT: ptt_pub_track is None");
                                    }
                                }
                                Ok(ChannelId::PttAi) => {
                                    if let Some(ref track) = ai_pub_track {
                                        let headers = ObjectHeaders::new(ai_group_id, ai_object_id);
                                        if ai_object_id == 0 {
                                            info!(
                                                "MoQ PTT: first AI publish with group_id={} (0x{:012x})",
                                                ai_group_id, ai_group_id
                                            );
                                        }
                                        let _ = track.publish(&headers, payload);
                                        ai_object_id += 1;
                                    } else {
                                        warn!("MoQ PTT: ai_pub_track is None");
                                    }
                                }
                                _ => {
                                    warn!("MoQ PTT: unknown channel_id {}", channel_id);
                                }
                            }
                        }
                    }
                    Ok(MoqCommand::SendChat { .. }) => {
                        // Chat not implemented
                    }
                    Err(TryRecvError::Empty) => {}
                    Err(TryRecvError::Disconnected) => {
                        info!("MoQ: command channel closed, exiting");
                        break;
                    }
                }

                // PTT subscription handling (when ready and not in Raw loopback)
                if ptt_ready && loopback != NetLoopbackMode::Raw {
                    // Drain PTT subscription (receive from other users)
                    if let Some(ref mut subscription) = ptt_subscription {
                        while let Ok(object) = subscription.try_recv() {
                            // Self-echo filter: only filter when loopback is Off
                            // When loopback is Moq, we want to hear our own audio via relay
                            if loopback == NetLoopbackMode::Off {
                                let dominated_by_self = object.headers.group_id == ptt_group_id
                                    && object.headers.object_id < ptt_object_id;
                                if dominated_by_self {
                                    continue;
                                }
                            }

                            ptt_recv_count += 1;
                            if ptt_recv_count <= 5 || ptt_recv_count % 100 == 0 {
                                info!(
                                    "MoQ PTT: recv ptt audio, group={} obj={} len={}",
                                    object.headers.group_id,
                                    object.headers.object_id,
                                    object.payload().len()
                                );
                            }
                            // Forward PTT audio to UI with channel_id prefix
                            let mut data = Vec::with_capacity(1 + object.payload().len());
                            data.push(ChannelId::Ptt as u8);
                            data.extend_from_slice(object.payload());
                            let _ = event_tx.send(MoqEvent::AudioReceived { data });
                        }
                    }

                    // Drain AI audio subscription (receive AI audio responses)
                    if let Some(ref mut subscription) = ai_audio_subscription {
                        while let Ok(object) = subscription.try_recv() {
                            ai_audio_recv_count += 1;
                            if ai_audio_recv_count <= 5 || ai_audio_recv_count % 100 == 0 {
                                info!(
                                    "MoQ PTT: recv AI audio, group={} obj={} len={}",
                                    object.headers.group_id,
                                    object.headers.object_id,
                                    object.payload().len()
                                );
                            }
                            // Forward AI audio to UI with channel_id prefix
                            let mut data = Vec::with_capacity(1 + object.payload().len());
                            data.push(ChannelId::PttAi as u8);
                            data.extend_from_slice(object.payload());
                            let _ = event_tx.send(MoqEvent::AudioReceived { data });
                        }
                    }

                    // Drain AI cmd subscription (receive AI command responses)
                    if let Some(ref mut subscription) = ai_cmd_subscription {
                        while let Ok(object) = subscription.try_recv() {
                            ai_cmd_recv_count += 1;
                            if ai_cmd_recv_count <= 5 || ai_cmd_recv_count % 100 == 0 {
                                info!(
                                    "MoQ PTT: recv AI cmd, group={} obj={} len={}",
                                    object.headers.group_id,
                                    object.headers.object_id,
                                    object.payload().len()
                                );
                            }
                            // Forward AI cmd to UI with ChatAi channel_id prefix
                            let mut data = Vec::with_capacity(1 + object.payload().len());
                            data.push(ChannelId::ChatAi as u8);
                            data.extend_from_slice(object.payload());
                            let _ = event_tx.send(MoqEvent::AudioReceived { data });
                        }
                    }

                    // Log stats every 2 seconds
                    if last_stats.elapsed() >= Duration::from_secs(2) {
                        let ai_audio_status = ai_audio_subscription.as_ref().map(|s| s.status());
                        let ai_cmd_status = ai_cmd_subscription.as_ref().map(|s| s.status());
                        info!(
                            "MoQ PTT: ptt_pub={} ptt_recv={} ai_pub={} ai_audio_recv={} ai_cmd_recv={} ai_audio={:?} ai_cmd={:?} active={:?} loopback={:?}",
                            ptt_object_id, ptt_recv_count, ai_object_id, ai_audio_recv_count, ai_cmd_recv_count, ai_audio_status, ai_cmd_status, active_ptt_channel, loopback
                        );
                        last_stats = Instant::now();
                    }
                }

                // WiFi monitoring (poll connection status every second)
                if let Some((ref mut w, ref ssid, ref _password)) = wifi {
                    if last_wifi_check.elapsed() >= Duration::from_secs(1) {
                        last_wifi_check = Instant::now();
                        let connected = w.is_connected().unwrap_or(false);
                        let up = w.is_up().unwrap_or(false);

                        if !wifi_connected && connected && up {
                            // Newly connected
                            info!("net: WiFi connected");
                            wifi_connected = true;
                            last_reconnect_attempt = None;
                            let _ = event_tx.send(MoqEvent::WifiConnected);
                        } else if wifi_connected && !connected {
                            // Disconnected
                            warn!("net: WiFi disconnected");
                            wifi_connected = false;

                            // Tear down MoQ (can't work without WiFi)
                            if client.is_some() {
                                client = None;
                                ptt_ready = false;
                                ptt_pub_track = None;
                                ai_pub_track = None;
                                ptt_subscription = None;
                                ai_audio_subscription = None;
                                ai_cmd_subscription = None;
                                let _ = event_tx.send(MoqEvent::Disconnected);
                            }
                            let _ = event_tx.send(MoqEvent::WifiDisconnected);

                            // Trigger reconnection (non-blocking)
                            info!("net: triggering WiFi reconnect to '{}'", ssid);
                            let _ = w.wifi_mut().connect();
                        }
                    }
                }

                // Reconnection logic: if WiFi is up and we have a URL but no client, try to reconnect
                if wifi_connected && client.is_none() {
                    if let Some(ref url) = relay_url {
                        let now = Instant::now();
                        let should_reconnect = last_reconnect_attempt
                            .map(|last| now.duration_since(last) >= Duration::from_secs(5))
                            .unwrap_or(true);

                        if should_reconnect {
                            info!("MoQ: attempting reconnection to {}", url);
                            last_reconnect_attempt = Some(now);

                            match ClientBuilder::new()
                                .endpoint_id(MOQ_ENDPOINT_ID)
                                .connect_uri(url)
                                .time_queue_max_duration(5000)
                                .tick_service_sleep_delay_us(30000)
                                .build()
                            {
                                Ok(c) => match block_on(c.connect()) {
                                    Ok(()) => {
                                        info!("MoQ: reconnected to {}", url);
                                        let _ = event_tx.send(MoqEvent::Connected);

                                        // Auto-start PTT mode on reconnect
                                        ptt_ready = setup_ptt_tracks(
                                            &c,
                                            ptt_config.as_ref(),
                                            ai_config.as_ref(),
                                            &mut ptt_pub_track,
                                            &mut ai_pub_track,
                                            &mut ptt_subscription,
                                            &mut ai_audio_subscription,
                                            &mut ai_cmd_subscription,
                                            &mut ptt_group_id,
                                            &mut ptt_object_id,
                                            &mut ai_group_id,
                                            &mut ai_object_id,
                                        );
                                        client = Some(c);
                                    }
                                    Err(e) => {
                                        warn!("MoQ: reconnect failed: {:?}", e);
                                    }
                                },
                                Err(e) => {
                                    warn!("MoQ: failed to create client for reconnect: {:?}", e);
                                }
                            }
                        }
                    }
                }

                thread::sleep(Duration::from_millis(1));
            }
        })
        .expect("failed to spawn MoQ thread");
}

// ============================================================================
// NVS Storage
// ============================================================================

/// NVS namespace for NET storage.
const NVS_NAMESPACE: &str = "net";

/// NVS-backed storage implementation
struct NvsStorage {
    nvs: Option<EspNvs<NvsDefault>>,
    wifi_ssids: heapless::Vec<WifiSsid, MAX_WIFI_SSIDS>,
    relay_url: String,
    language: String,
    channel: String,
    ai: String,
    logs_enabled: bool,
}

// NVS key names (max 15 chars)
const NVS_KEY_WIFI_SSIDS: &str = "wifi_ssids";
const NVS_KEY_RELAY_URL: &str = "relay_url";
const NVS_KEY_LOGS: &str = "logs_enabled";
const NVS_KEY_LANGUAGE: &str = "language";
const NVS_KEY_CHANNEL: &str = "channel";
const NVS_KEY_AI: &str = "ai";

impl NvsStorage {
    /// Load storage from NVS
    fn load(nvs: Option<EspNvs<NvsDefault>>) -> Self {
        let mut storage = Self {
            nvs,
            wifi_ssids: heapless::Vec::new(),
            relay_url: String::new(),
            language: String::new(),
            channel: String::new(),
            ai: String::new(),
            logs_enabled: true,
        };

        // Load WiFi SSIDs
        if let Some(ref nvs) = storage.nvs {
            let mut buf = [0u8; 512];
            match nvs.get_blob(NVS_KEY_WIFI_SSIDS, &mut buf) {
                Ok(Some(data)) => {
                    if let Ok((ssids, _)) =
                        serde_json_core::from_slice::<heapless::Vec<WifiSsid, MAX_WIFI_SSIDS>>(data)
                    {
                        storage.wifi_ssids = ssids;
                        info!(
                            "net: loaded {} WiFi SSIDs from NVS",
                            storage.wifi_ssids.len()
                        );
                    }
                }
                Ok(None) => {}
                Err(e) => {
                    warn!("net: failed to read WiFi SSIDs from NVS: {:?}", e);
                }
            }

            // Load relay URL
            let mut url_buf = [0u8; MAX_RELAY_URL_LEN];
            match nvs.get_blob(NVS_KEY_RELAY_URL, &mut url_buf) {
                Ok(Some(data)) => {
                    if let Ok(url) = core::str::from_utf8(data) {
                        storage.relay_url = url.to_string();
                        info!("net: loaded relay URL from NVS: {}", storage.relay_url);
                    }
                }
                Ok(None) => {}
                Err(e) => {
                    warn!("net: failed to read relay URL from NVS: {:?}", e);
                }
            }

            // Load logs_enabled
            match nvs.get_u8(NVS_KEY_LOGS) {
                Ok(Some(val)) => {
                    storage.logs_enabled = val != 0;
                    info!(
                        "net: loaded logs_enabled from NVS: {}",
                        storage.logs_enabled
                    );
                }
                Ok(None) => {}
                Err(e) => {
                    warn!("net: failed to read logs_enabled from NVS: {:?}", e);
                }
            }

            // Load language
            let mut lang_buf = [0u8; 32];
            match nvs.get_blob(NVS_KEY_LANGUAGE, &mut lang_buf) {
                Ok(Some(data)) => {
                    if let Ok(lang) = core::str::from_utf8(data) {
                        storage.language = lang.to_string();
                        info!("net: loaded language from NVS: {}", storage.language);
                    }
                }
                Ok(None) => {}
                Err(e) => {
                    warn!("net: failed to read language from NVS: {:?}", e);
                }
            }

            // Load channel (JSON array)
            let mut channel_buf = [0u8; 512];
            match nvs.get_blob(NVS_KEY_CHANNEL, &mut channel_buf) {
                Ok(Some(data)) => {
                    if let Ok(channel) = core::str::from_utf8(data) {
                        storage.channel = channel.to_string();
                        info!("net: loaded channel from NVS: {}", storage.channel);
                    }
                }
                Ok(None) => {}
                Err(e) => {
                    warn!("net: failed to read channel from NVS: {:?}", e);
                }
            }

            // Load AI config (JSON object)
            let mut ai_buf = [0u8; 1024];
            match nvs.get_blob(NVS_KEY_AI, &mut ai_buf) {
                Ok(Some(data)) => {
                    if let Ok(ai) = core::str::from_utf8(data) {
                        storage.ai = ai.to_string();
                        info!("net: loaded AI config from NVS");
                    }
                }
                Ok(None) => {}
                Err(e) => {
                    warn!("net: failed to read AI config from NVS: {:?}", e);
                }
            }
        }

        storage
    }

    /// Save storage to NVS
    fn save(&mut self) -> Result<(), esp_idf_svc::sys::EspError> {
        let Some(ref mut nvs) = self.nvs else {
            warn!("net: NVS not available, cannot save");
            return Ok(());
        };

        // Save WiFi SSIDs
        let mut wifi_buf = [0u8; 512];
        if let Ok(len) = serde_json_core::to_slice(&self.wifi_ssids, &mut wifi_buf) {
            nvs.set_blob(NVS_KEY_WIFI_SSIDS, &wifi_buf[..len])?;
            info!("net: saved {} WiFi SSIDs to NVS", self.wifi_ssids.len());
        }

        // Save relay URL
        if !self.relay_url.is_empty() {
            nvs.set_blob(NVS_KEY_RELAY_URL, self.relay_url.as_bytes())?;
            info!("net: saved relay URL to NVS");
        } else {
            // Remove the key if URL is empty
            let _ = nvs.remove(NVS_KEY_RELAY_URL);
        }

        // Save logs_enabled
        nvs.set_u8(NVS_KEY_LOGS, self.logs_enabled as u8)?;

        // Save language
        if !self.language.is_empty() {
            nvs.set_blob(NVS_KEY_LANGUAGE, self.language.as_bytes())?;
        } else {
            let _ = nvs.remove(NVS_KEY_LANGUAGE);
        }

        // Save channel
        if !self.channel.is_empty() {
            nvs.set_blob(NVS_KEY_CHANNEL, self.channel.as_bytes())?;
        } else {
            let _ = nvs.remove(NVS_KEY_CHANNEL);
        }

        // Save AI config
        if !self.ai.is_empty() {
            nvs.set_blob(NVS_KEY_AI, self.ai.as_bytes())?;
        } else {
            let _ = nvs.remove(NVS_KEY_AI);
        }

        Ok(())
    }

    fn add_wifi_ssid(&mut self, ssid: &str, password: &str) -> Result<(), ()> {
        if self.wifi_ssids.len() >= MAX_WIFI_SSIDS {
            return Err(());
        }

        let wifi = WifiSsid {
            ssid: ssid.to_string(),
            password: password.to_string(),
        };

        self.wifi_ssids.push(wifi).map_err(|_| ())?;
        Ok(())
    }

    fn clear(&mut self) {
        self.wifi_ssids.clear();
        self.relay_url.clear();
        self.language.clear();
        self.channel.clear();
        self.ai.clear();
        self.logs_enabled = true;
    }
}

// ============================================================================
// Main
// ============================================================================

fn main() {
    esp_idf_svc::sys::link_patches();
    esp_idf_svc::log::EspLogger::initialize_default();

    // Configure pthread to use PSRAM for thread stacks (like hactar firmware)
    // This must be done before spawning any threads
    unsafe {
        use esp_idf_svc::sys::{
            esp_pthread_cfg_t, esp_pthread_get_default_config, esp_pthread_set_cfg,
            MALLOC_CAP_8BIT, MALLOC_CAP_SPIRAM,
        };
        let mut cfg: esp_pthread_cfg_t = esp_pthread_get_default_config();
        cfg.stack_size = 32000; // 32KB stacks like hactar
        cfg.stack_alloc_caps = MALLOC_CAP_SPIRAM | MALLOC_CAP_8BIT;
        esp_pthread_set_cfg(&cfg);
    }

    info!("net: initializing");

    let peripherals = Peripherals::take().unwrap();
    let sys_loop = EspSystemEventLoop::take().unwrap();
    let nvs_partition = EspDefaultNvsPartition::take().unwrap();

    // Initialize LED - RGB on GPIO 38, 37, 36 (active low)
    let mut led_r = PinDriver::output(peripherals.pins.gpio38).unwrap();
    let mut led_g = PinDriver::output(peripherals.pins.gpio37).unwrap();
    let mut led_b = PinDriver::output(peripherals.pins.gpio36).unwrap();

    // LED colors: Red=default/no WiFi, Green=WiFi connected, Blue=MoQ connected
    set_led_color(&mut led_r, &mut led_g, &mut led_b, Color::Red);

    // Initialize UARTs
    let mgmt_uart_config = {
        let mut config = uart::config::Config::new()
            .baudrate(uart_config::MGMT_NET.baudrate.into())
            .data_bits(uart::config::DataBits::DataBits8);

        config = match uart_config::MGMT_NET.parity {
            uart_config::Parity::None => config.parity_none(),
            uart_config::Parity::Even => config.parity_even(),
        };

        config = match uart_config::MGMT_NET.stop_bits {
            uart_config::StopBits::One => config.stop_bits(uart::config::StopBits::STOP1),
            uart_config::StopBits::Two => config.stop_bits(uart::config::StopBits::STOP2),
        };

        config
    };

    let mgmt_uart = UartDriver::new(
        peripherals.uart0,
        peripherals.pins.gpio43,
        peripherals.pins.gpio44,
        Option::<GpioStub>::None,
        Option::<GpioStub>::None,
        &mgmt_uart_config,
    )
    .unwrap();

    let ui_uart_config = {
        let mut config = uart::config::Config::new()
            .baudrate(uart_config::UI_NET.baudrate.into())
            .data_bits(uart::config::DataBits::DataBits8);

        config = match uart_config::UI_NET.parity {
            uart_config::Parity::None => config.parity_none(),
            uart_config::Parity::Even => config.parity_even(),
        };

        config = match uart_config::UI_NET.stop_bits {
            uart_config::StopBits::One => config.stop_bits(uart::config::StopBits::STOP1),
            uart_config::StopBits::Two => config.stop_bits(uart::config::StopBits::STOP2),
        };

        config
    };

    let ui_uart = UartDriver::new(
        peripherals.uart1,
        peripherals.pins.gpio17,
        peripherals.pins.gpio18,
        Option::<GpioStub>::None,
        Option::<GpioStub>::None,
        &ui_uart_config,
    )
    .unwrap();

    info!("net: UARTs initialized");

    // Initialize WiFi (AsyncWifi for non-blocking connect/reconnect)
    let wifi = esp_idf_svc::wifi::EspWifi::new(
        peripherals.modem,
        sys_loop.clone(),
        Some(nvs_partition.clone()),
    )
    .unwrap();
    let timer_service = esp_idf_svc::timer::EspTaskTimerService::new().unwrap();
    let wifi = esp_idf_svc::wifi::AsyncWifi::wrap(wifi, sys_loop, timer_service).unwrap();

    // Open NVS for storage
    let nvs = match EspNvs::new(nvs_partition.clone(), NVS_NAMESPACE, true) {
        Ok(nvs) => Some(nvs),
        Err(e) => {
            warn!("net: failed to open NVS: {:?}", e);
            None
        }
    };

    // Load storage from NVS
    let mut storage = NvsStorage::load(nvs);
    info!(
        "net: loaded {} WiFi SSIDs from NVS",
        storage.wifi_ssids.len()
    );

    // Apply stored log level
    if !storage.logs_enabled {
        log::set_max_level(log::LevelFilter::Off);
    }

    // MoQ + WiFi task (combined thread handles WiFi connection, monitoring, and MoQ)
    let (moq_cmd_tx, moq_cmd_rx) = mpsc::channel::<MoqCommand>();
    let (moq_event_tx, moq_event_rx) = mpsc::channel::<MoqEvent>();

    let wifi_config = if !storage.wifi_ssids.is_empty() {
        let ssid = storage.wifi_ssids[0].ssid.clone();
        let password = storage.wifi_ssids[0].password.clone();
        Some((wifi, ssid, password))
    } else {
        None
    };
    let initial_relay_url = if !storage.relay_url.is_empty() {
        Some(storage.relay_url.clone())
    } else {
        None
    };
    spawn_moq_task(wifi_config, initial_relay_url, moq_cmd_rx, moq_event_tx);

    // Send initial track config to MoQ task
    let ptt_config = PttConfig::from_storage(&storage.language, &storage.channel);
    let ai_config = AiConfig::from_storage(&storage.language, &storage.ai);
    if ptt_config.is_some() || ai_config.is_some() {
        info!("net: sending initial track config to MoQ task");
        let _ = moq_cmd_tx.send(MoqCommand::UpdateConfig {
            ptt: ptt_config,
            ai: ai_config,
        });
    } else {
        warn!("net: no channel/AI config - device needs configuration via CTL");
    }

    // Loopback mode state
    let mut loopback = NetLoopbackMode::Off;

    // Per-channel jitter buffers
    let mut ptt_buffer = JitterBuffer::new();
    let mut ptt_ai_buffer = JitterBuffer::new();
    let mut last_buffer_tick = Instant::now();
    const BUFFER_TICK_INTERVAL: Duration = Duration::from_millis(20);

    info!("net: starting main loop");

    // TLV buffers
    let mut mgmt_rx_buf = [0u8; SYNC_WORD.len() + HEADER_SIZE + MAX_VALUE_SIZE];
    let mut ui_rx_buf = [0u8; SYNC_WORD.len() + HEADER_SIZE + MAX_VALUE_SIZE];
    let mut mgmt_rx_pos = 0usize;
    let mut ui_rx_pos = 0usize;

    loop {
        // Check MGMT UART for incoming data
        if let Some((msg_type, value)) =
            try_read_tlv(&mgmt_uart, &mut mgmt_rx_buf, &mut mgmt_rx_pos)
        {
            if let Ok(tlv_type) = CtlToNet::try_from(msg_type) {
                handle_mgmt_message(
                    tlv_type,
                    &value,
                    &mgmt_uart,
                    &ui_uart,
                    &mut storage,
                    &mut loopback,
                    &moq_cmd_tx,
                    &ptt_buffer,
                    &ptt_ai_buffer,
                );
            }
        }

        // Check UI UART for incoming data
        if let Some((msg_type, value)) = try_read_tlv(&ui_uart, &mut ui_rx_buf, &mut ui_rx_pos) {
            if let Ok(tlv_type) = UiToNet::try_from(msg_type) {
                handle_ui_message(
                    tlv_type,
                    &value,
                    &mgmt_uart,
                    &ui_uart,
                    loopback,
                    &moq_cmd_tx,
                );
            }
        }

        // MoQ/WiFi event handling
        use std::sync::mpsc::TryRecvError;
        match moq_event_rx.try_recv() {
            Ok(MoqEvent::Connected) => {
                info!("net: MoQ connected - PTT mode active");
                set_led_color(&mut led_r, &mut led_g, &mut led_b, Color::Blue);
                // Reset jitter buffers to clear any initial backlog
                ptt_buffer.reset();
                ptt_ai_buffer.reset();
            }
            Ok(MoqEvent::Disconnected) => {
                info!("net: MoQ disconnected");
                set_led_color(&mut led_r, &mut led_g, &mut led_b, Color::Green);
            }
            Ok(MoqEvent::WifiConnected) => {
                info!("net: WiFi connected");
                set_led_color(&mut led_r, &mut led_g, &mut led_b, Color::Green);
            }
            Ok(MoqEvent::WifiDisconnected) => {
                warn!("net: WiFi disconnected");
                set_led_color(&mut led_r, &mut led_g, &mut led_b, Color::Red);
            }
            Ok(MoqEvent::Error { message }) => {
                warn!("net: MoQ error: {}", message);
            }
            Ok(MoqEvent::ChatSent) => {}
            Ok(MoqEvent::ChatReceived { .. }) => {}
            Ok(MoqEvent::AudioReceived { data }) => {
                // Route received data based on channel_id
                if data.len() >= 2 {
                    let channel_id = data[0];
                    let payload = &data[1..];
                    match ChannelId::try_from(channel_id) {
                        Ok(ChannelId::Ptt) => {
                            if !ptt_buffer.push(payload) {
                                warn!("net: ptt buffer overrun");
                            }
                        }
                        Ok(ChannelId::PttAi) => {
                            if !ptt_ai_buffer.push(payload) {
                                warn!("net: ptt_ai buffer overrun");
                            }
                        }
                        Ok(ChannelId::ChatAi) => {
                            // AI commands: forward directly to UI (no jitter buffering)
                            write_tlv(&ui_uart, NetToUi::AudioFrame, &data);
                        }
                        _ => {
                            warn!("net: unknown channel_id {}", channel_id);
                        }
                    }
                }
            }
            Err(TryRecvError::Empty) => {}
            Err(TryRecvError::Disconnected) => {
                warn!("net: MoQ event channel closed");
            }
        }

        // Timer tick - pop from jitter buffers every 20ms
        if last_buffer_tick.elapsed() >= BUFFER_TICK_INTERVAL {
            last_buffer_tick = Instant::now();

            // Pop from each channel buffer and send to UI
            for (buffer, channel_id) in [
                (&mut ptt_buffer, ChannelId::Ptt),
                (&mut ptt_ai_buffer, ChannelId::PttAi),
            ] {
                if buffer.state() == JitterState::Playing || buffer.level() >= 5 {
                    if let Some(frame) = buffer.pop() {
                        // Prepend channel_id to the frame
                        let mut out = Vec::with_capacity(1 + frame.len());
                        out.push(channel_id as u8);
                        out.extend_from_slice(&frame);
                        write_tlv(&ui_uart, NetToUi::AudioFrame, &out);
                    }
                }
            }
        }

        // Small delay to prevent busy-waiting
        thread::sleep(Duration::from_millis(1));
    }
}

// GPIO stub type for UART driver (no CTS/RTS)
type GpioStub = esp_idf_svc::hal::gpio::Gpio0;

/// Set LED color (active low RGB LED)
fn set_led_color<R: OutputPin, G: OutputPin, B: OutputPin>(
    led_r: &mut PinDriver<'_, R, esp_idf_svc::hal::gpio::Output>,
    led_g: &mut PinDriver<'_, G, esp_idf_svc::hal::gpio::Output>,
    led_b: &mut PinDriver<'_, B, esp_idf_svc::hal::gpio::Output>,
    color: Color,
) {
    // Active low: set_low() turns LED on, set_high() turns LED off
    match color {
        Color::Black => {
            led_r.set_high().ok();
            led_g.set_high().ok();
            led_b.set_high().ok();
        }
        Color::Red => {
            led_r.set_low().ok();
            led_g.set_high().ok();
            led_b.set_high().ok();
        }
        Color::Green => {
            led_r.set_high().ok();
            led_g.set_low().ok();
            led_b.set_high().ok();
        }
        Color::Blue => {
            led_r.set_high().ok();
            led_g.set_high().ok();
            led_b.set_low().ok();
        }
        Color::Yellow => {
            led_r.set_low().ok();
            led_g.set_low().ok();
            led_b.set_high().ok();
        }
        Color::Cyan => {
            led_r.set_high().ok();
            led_g.set_low().ok();
            led_b.set_low().ok();
        }
        Color::Magenta => {
            led_r.set_low().ok();
            led_g.set_high().ok();
            led_b.set_low().ok();
        }
        Color::White => {
            led_r.set_low().ok();
            led_g.set_low().ok();
            led_b.set_low().ok();
        }
    }
}

/// Start WiFi (non-blocking: configure + start driver + trigger connect).
/// Connection completes asynchronously; poll `wifi_mut().is_connected()` to detect it.
fn start_wifi(
    wifi: &mut esp_idf_svc::wifi::AsyncWifi<esp_idf_svc::wifi::EspWifi<'static>>,
    ssid: &str,
    password: &str,
) -> Result<(), esp_idf_svc::sys::EspError> {
    use embedded_svc::wifi::{AuthMethod, ClientConfiguration, Configuration};

    let config = Configuration::Client(ClientConfiguration {
        ssid: ssid.try_into().unwrap(),
        bssid: None,
        auth_method: AuthMethod::WPA2Personal,
        password: password.try_into().unwrap(),
        channel: None,
        ..Default::default()
    });

    wifi.wifi_mut().set_configuration(&config)?;
    wifi.wifi_mut().start()?;
    wifi.wifi_mut().connect()?;

    Ok(())
}

/// Total frame size: sync (4) + header (6) + value
const FRAME_HEADER_SIZE: usize = SYNC_WORD.len() + HEADER_SIZE;

/// Try to read a TLV message from UART (non-blocking)
fn try_read_tlv(
    uart: &UartDriver,
    buf: &mut [u8],
    pos: &mut usize,
) -> Option<(u16, heapless::Vec<u8, MAX_VALUE_SIZE>)> {
    // Try to read available bytes
    let mut read_buf = [0u8; 512];
    let len = uart.read(&mut read_buf, 0).unwrap_or(0);
    if len > 0 {
        let space = buf.len() - *pos;
        let copy_len = len.min(space);
        buf[*pos..*pos + copy_len].copy_from_slice(&read_buf[..copy_len]);
        *pos += copy_len;
    }

    // Try to parse TLV
    if *pos >= FRAME_HEADER_SIZE {
        if buf[0..4] != SYNC_WORD {
            // Sync error - try to find sync word
            if let Some(idx) = buf[1..*pos].windows(4).position(|w| w == SYNC_WORD) {
                let new_start = idx + 1;
                buf.copy_within(new_start..*pos, 0);
                *pos -= new_start;
            } else {
                if *pos > 3 {
                    buf.copy_within(*pos - 3..*pos, 0);
                    *pos = 3;
                }
            }
            return None;
        }

        let msg_type = u16::from_le_bytes([buf[4], buf[5]]);
        let length = u32::from_le_bytes([buf[6], buf[7], buf[8], buf[9]]) as usize;

        if length > MAX_VALUE_SIZE {
            buf.copy_within(4..*pos, 0);
            *pos -= 4;
            return None;
        }

        let total_len = FRAME_HEADER_SIZE + length;
        if *pos >= total_len {
            let mut value = heapless::Vec::new();
            value
                .extend_from_slice(&buf[FRAME_HEADER_SIZE..total_len])
                .ok();
            buf.copy_within(total_len..*pos, 0);
            *pos -= total_len;
            return Some((msg_type, value));
        }
    }

    None
}

/// Write a TLV message to UART
fn write_tlv<T: Into<u16>>(uart: &UartDriver, msg_type: T, value: &[u8]) {
    let msg_type: u16 = msg_type.into();

    // Buffer entire TLV to write atomically (prevents log interleaving)
    let total_len = SYNC_WORD.len() + HEADER_SIZE + value.len();
    let mut buf = vec![0u8; total_len];

    buf[0..4].copy_from_slice(&SYNC_WORD);
    buf[4..6].copy_from_slice(&msg_type.to_le_bytes());
    buf[6..10].copy_from_slice(&(value.len() as u32).to_le_bytes());
    if !value.is_empty() {
        buf[10..].copy_from_slice(value);
    }

    uart.write(&buf).ok();
}

/// Handle message from MGMT chip
fn handle_mgmt_message(
    msg_type: CtlToNet,
    value: &[u8],
    mgmt_uart: &UartDriver,
    ui_uart: &UartDriver,
    storage: &mut NvsStorage,
    loopback: &mut NetLoopbackMode,
    moq_cmd_tx: &Sender<MoqCommand>,
    _ptt_buffer: &JitterBuffer,
    _ptt_ai_buffer: &JitterBuffer,
) {
    match msg_type {
        CtlToNet::Ping => {
            write_tlv(mgmt_uart, NetToCtl::Pong, value);
        }
        CtlToNet::CircularPing => {
            write_tlv(ui_uart, NetToUi::CircularPing, value);
        }
        CtlToNet::AddWifiSsid => {
            if let Ok((wifi, _)) = serde_json_core::from_slice::<WifiSsid>(value) {
                if storage.add_wifi_ssid(&wifi.ssid, &wifi.password).is_ok() {
                    if storage.save().is_ok() {
                        write_tlv(mgmt_uart, NetToCtl::Ack, &[]);
                    } else {
                        write_tlv(mgmt_uart, NetToCtl::Error, b"save");
                    }
                } else {
                    write_tlv(mgmt_uart, NetToCtl::Error, b"add");
                }
            } else {
                write_tlv(mgmt_uart, NetToCtl::Error, b"deserialize");
            }
        }
        CtlToNet::GetWifiSsids => {
            let mut buf = [0u8; 512];
            if let Ok(len) = serde_json_core::to_slice(&storage.wifi_ssids, &mut buf) {
                write_tlv(mgmt_uart, NetToCtl::WifiSsids, &buf[..len]);
            }
        }
        CtlToNet::ClearWifiSsids => {
            storage.wifi_ssids.clear();
            if storage.save().is_ok() {
                write_tlv(mgmt_uart, NetToCtl::Ack, &[]);
            } else {
                write_tlv(mgmt_uart, NetToCtl::Error, b"save");
            }
        }
        CtlToNet::GetRelayUrl => {
            write_tlv(mgmt_uart, NetToCtl::RelayUrl, storage.relay_url.as_bytes());
        }
        CtlToNet::SetRelayUrl => {
            if let Ok(url) = core::str::from_utf8(value) {
                storage.relay_url = url.to_string();
                if storage.save().is_ok() {
                    // Also trigger MoQ connection to new relay
                    let _ = moq_cmd_tx.send(MoqCommand::SetRelayUrl(url.to_string()));
                    write_tlv(mgmt_uart, NetToCtl::Ack, &[]);
                } else {
                    write_tlv(mgmt_uart, NetToCtl::Error, b"save");
                }
            } else {
                write_tlv(mgmt_uart, NetToCtl::Error, b"utf8");
            }
        }
        CtlToNet::SetLoopback => {
            let mode_byte = value.first().copied().unwrap_or(0);
            *loopback = NetLoopbackMode::try_from(mode_byte).unwrap_or(NetLoopbackMode::Off);
            // Notify MoQ task of loopback change
            let _ = moq_cmd_tx.send(MoqCommand::SetLoopback(*loopback));
            write_tlv(mgmt_uart, NetToCtl::Ack, &[]);
        }
        CtlToNet::GetLoopback => {
            write_tlv(mgmt_uart, NetToCtl::Loopback, &[*loopback as u8]);
        }
        CtlToNet::GetLogsEnabled => {
            write_tlv(
                mgmt_uart,
                NetToCtl::LogsEnabled,
                &[storage.logs_enabled as u8],
            );
        }
        CtlToNet::SetLogsEnabled => {
            let enabled = value.first().copied().unwrap_or(1) != 0;
            storage.logs_enabled = enabled;
            // Apply log level immediately
            if enabled {
                log::set_max_level(log::LevelFilter::Info);
            } else {
                log::set_max_level(log::LevelFilter::Off);
            }
            if storage.save().is_ok() {
                write_tlv(mgmt_uart, NetToCtl::Ack, &[]);
            } else {
                write_tlv(mgmt_uart, NetToCtl::Error, b"save");
            }
        }
        CtlToNet::ClearStorage => {
            storage.clear();
            if storage.save().is_ok() {
                write_tlv(mgmt_uart, NetToCtl::Ack, &[]);
            } else {
                write_tlv(mgmt_uart, NetToCtl::Error, b"save");
            }
        }
        CtlToNet::GetLanguage => {
            write_tlv(mgmt_uart, NetToCtl::Language, storage.language.as_bytes());
        }
        CtlToNet::SetLanguage => {
            if let Ok(lang) = core::str::from_utf8(value) {
                storage.language = lang.to_string();
                if storage.save().is_ok() {
                    // Update MoQ tracks with new language
                    let _ = moq_cmd_tx.send(MoqCommand::UpdateConfig {
                        ptt: PttConfig::from_storage(&storage.language, &storage.channel),
                        ai: AiConfig::from_storage(&storage.language, &storage.ai),
                    });
                    write_tlv(mgmt_uart, NetToCtl::Ack, &[]);
                } else {
                    write_tlv(mgmt_uart, NetToCtl::Error, b"save");
                }
            } else {
                write_tlv(mgmt_uart, NetToCtl::Error, b"utf8");
            }
        }
        CtlToNet::GetChannel => {
            write_tlv(mgmt_uart, NetToCtl::Channel, storage.channel.as_bytes());
        }
        CtlToNet::SetChannel => {
            if let Ok(channel) = core::str::from_utf8(value) {
                storage.channel = channel.to_string();
                if storage.save().is_ok() {
                    // Update MoQ tracks with new channel
                    let _ = moq_cmd_tx.send(MoqCommand::UpdateConfig {
                        ptt: PttConfig::from_storage(&storage.language, &storage.channel),
                        ai: AiConfig::from_storage(&storage.language, &storage.ai),
                    });
                    write_tlv(mgmt_uart, NetToCtl::Ack, &[]);
                } else {
                    write_tlv(mgmt_uart, NetToCtl::Error, b"save");
                }
            } else {
                write_tlv(mgmt_uart, NetToCtl::Error, b"utf8");
            }
        }
        CtlToNet::GetAi => {
            write_tlv(mgmt_uart, NetToCtl::Ai, storage.ai.as_bytes());
        }
        CtlToNet::SetAi => {
            if let Ok(ai_json) = core::str::from_utf8(value) {
                storage.ai = ai_json.to_string();
                if storage.save().is_ok() {
                    // Update MoQ tracks with new AI config
                    let _ = moq_cmd_tx.send(MoqCommand::UpdateConfig {
                        ptt: PttConfig::from_storage(&storage.language, &storage.channel),
                        ai: AiConfig::from_storage(&storage.language, &storage.ai),
                    });
                    write_tlv(mgmt_uart, NetToCtl::Ack, &[]);
                } else {
                    write_tlv(mgmt_uart, NetToCtl::Error, b"save");
                }
            } else {
                write_tlv(mgmt_uart, NetToCtl::Error, b"utf8");
            }
        }
        CtlToNet::BurnJtagEfuse => {
            // IRREVERSIBLE: Burn efuse to disable USB JTAG debugging
            // This matches hactar's implementation in efuse_burner.cc
            use core::ptr::addr_of_mut;
            use esp_idf_svc::sys::{
                esp_efuse_batch_write_begin, esp_efuse_batch_write_cancel,
                esp_efuse_batch_write_commit, esp_efuse_read_field_bit, esp_efuse_write_field_bit,
                ESP_EFUSE_DIS_USB_JTAG, ESP_OK,
            };

            unsafe {
                // Check if already burned
                if esp_efuse_read_field_bit(addr_of_mut!(ESP_EFUSE_DIS_USB_JTAG) as *mut _) {
                    info!("net: efuse for DIS_USB_JTAG is already burned");
                    write_tlv(mgmt_uart, NetToCtl::Ack, &[]);
                    return;
                }

                info!("net: starting efuse burning process to disable USB JTAG (IRREVERSIBLE!)");

                // Begin batch write
                let err = esp_efuse_batch_write_begin();
                if err != ESP_OK {
                    warn!("net: failed to start batch write: {}", err);
                    write_tlv(mgmt_uart, NetToCtl::Error, b"batch begin failed");
                    return;
                }

                // Write the efuse bit
                let err = esp_efuse_write_field_bit(addr_of_mut!(ESP_EFUSE_DIS_USB_JTAG) as *mut _);
                if err != ESP_OK {
                    warn!("net: failed to write efuse field: {}", err);
                    esp_efuse_batch_write_cancel();
                    write_tlv(mgmt_uart, NetToCtl::Error, b"write failed");
                    return;
                }

                // Commit the changes
                let err = esp_efuse_batch_write_commit();
                if err != ESP_OK {
                    warn!("net: failed to commit efuse changes: {}", err);
                    write_tlv(mgmt_uart, NetToCtl::Error, b"commit failed");
                    return;
                }

                info!("net: successfully burned efuse to disable USB JTAG");
                write_tlv(mgmt_uart, NetToCtl::Ack, &[]);
            }
        }
    }
}

/// Handle message from UI chip
fn handle_ui_message(
    msg_type: UiToNet,
    value: &[u8],
    mgmt_uart: &UartDriver,
    ui_uart: &UartDriver,
    loopback: NetLoopbackMode,
    moq_cmd_tx: &Sender<MoqCommand>,
) {
    match msg_type {
        UiToNet::CircularPing => {
            write_tlv(mgmt_uart, NetToCtl::CircularPing, value);
        }
        UiToNet::AudioFrame => {
            // channel_id (1 byte) + encrypted payload
            // Set PTT channel based on channel_id prefix
            if let Some(&ch) = value.first() {
                if ch == ChannelId::Ptt as u8 {
                    let _ = moq_cmd_tx.send(MoqCommand::SetPttChannel(PttChannel::Ptt));
                } else if ch == ChannelId::PttAi as u8 {
                    let _ = moq_cmd_tx.send(MoqCommand::SetPttChannel(PttChannel::Ai));
                }
            }
            if loopback == NetLoopbackMode::Raw {
                // Raw loopback - forward directly to UI without MoQ
                write_tlv(ui_uart, NetToUi::AudioFrame, value);
            } else {
                // Send to MoQ task (Off or Moq loopback)
                let _ = moq_cmd_tx.send(MoqCommand::AudioFrame {
                    data: value.to_vec(),
                });
            }
        }
    }
}

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
    config::{DeviceConfig, Language},
    net::{
        JitterBuffer, JitterState, WifiSsid, MAX_RELAY_URL_LEN,
        MAX_WIFI_SSIDS,
    },
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
    /// Reconfigure MoQ tracks (channel/language change).
    Reconfigure {
        ptt_namespace: Vec<String>,
        ptt_track_name: String,
        ai_pub_namespace: Vec<String>,
        ai_pub_track_name: String,
        ai_sub_namespace: Vec<String>,
        ai_sub_track_name: String,
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
// Config WebSocket Types
// ============================================================================

/// Events sent from the config WebSocket task back to the main loop.
enum ConfigEvent {
    /// New config JSON received from WebSocket.
    NewConfig(String),
    /// Token was refreshed successfully.
    TokenRefreshed {
        access_token: String,
        refresh_token: String,
    },
    /// Error occurred.
    Error(String),
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

/// Track parameters for MoQ setup (derived from config + language + channel).
#[derive(Clone)]
struct TrackParams {
    ptt_namespace: Vec<String>,
    ptt_track_name: String,
    ai_pub_namespace: Vec<String>,
    ai_pub_track_name: String,
    ai_sub_namespace: Vec<String>,
    ai_sub_track_name: String,
}

/// Build default (hardcoded) track parameters as fallback when no config exists.
fn default_track_params() -> TrackParams {
    let ns_prefix = vec![
        "moq://moq.ptt.arpa/v1".to_string(),
        "org/acme".to_string(),
        "store/1234".to_string(),
    ];
    let channel_name = "gardening";
    let track_name = "pcm_en_8khz_mono_i16";

    let mut ptt_ns = ns_prefix.clone();
    ptt_ns.push(format!("channel/{}", channel_name));
    ptt_ns.push("ptt".to_string());

    let mut ai_ns = ns_prefix;
    ai_ns.push("ai/audio".to_string());

    let device_id_str = format!("{}", get_device_id_from_mac());

    TrackParams {
        ptt_namespace: ptt_ns,
        ptt_track_name: track_name.to_string(),
        ai_pub_namespace: ai_ns.clone(),
        ai_pub_track_name: track_name.to_string(),
        ai_sub_namespace: ai_ns,
        ai_sub_track_name: device_id_str,
    }
}

/// Derive track parameters from a DeviceConfig, language, and selected channel.
fn track_params_from_config(
    config: &DeviceConfig,
    language: &str,
    selected_channel: &str,
) -> Option<TrackParams> {
    // Find the selected channel
    let channel = config
        .channels
        .iter()
        .find(|c| c.display_name == selected_channel)
        .or_else(|| config.channels.first())?;

    // Get language-specific tracks (fall back to "en")
    let lang_tracks = channel
        .languages
        .get(language)
        .or_else(|| channel.languages.get("en"))?;

    let device_id_str = format!("{}", get_device_id_from_mac());

    // Get AI tracks for this language
    let ai_lang = config
        .ai
        .languages
        .get(language)
        .or_else(|| config.ai.languages.get("en"))?;

    Some(TrackParams {
        ptt_namespace: lang_tracks.ptt.namespace.clone(),
        ptt_track_name: lang_tracks.ptt.name.clone(),
        ai_pub_namespace: ai_lang.namespace.clone(),
        ai_pub_track_name: ai_lang.name.clone(),
        ai_sub_namespace: config.ai.response_ns.clone(),
        ai_sub_track_name: device_id_str,
    })
}

/// Helper function to set up PTT tracks after connection.
/// Returns true if setup was successful.
fn setup_tracks_from_params(
    client: &quicr::Client,
    params: &TrackParams,
    ptt_pub_track: &mut Option<std::sync::Arc<quicr::PublishTrack>>,
    ai_pub_track: &mut Option<std::sync::Arc<quicr::PublishTrack>>,
    ptt_subscription: &mut Option<Subscription>,
    ai_subscription: &mut Option<Subscription>,
    ptt_group_id: &mut u64,
    ptt_object_id: &mut u64,
    ai_group_id: &mut u64,
    ai_object_id: &mut u64,
) -> bool {
    let ptt_ns_strs: Vec<&str> = params.ptt_namespace.iter().map(|s| s.as_str()).collect();
    let ptt_ns = TrackNamespace::from_strings(&ptt_ns_strs);
    client.publish_namespace(&ptt_ns);

    let ai_pub_ns_strs: Vec<&str> = params.ai_pub_namespace.iter().map(|s| s.as_str()).collect();
    let ai_pub_ns = TrackNamespace::from_strings(&ai_pub_ns_strs);
    client.publish_namespace(&ai_pub_ns);

    let device_id = get_device_id_from_mac();
    info!("device id: {}", device_id);

    // Create PTT publish track
    let ptt_track_name_full =
        FullTrackName::new(ptt_ns.clone(), params.ptt_track_name.clone().into_bytes());
    info!(
        "MoQ PTT: publish namespace={}, track={} (device_id={})",
        ptt_ns, params.ptt_track_name, device_id
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
        }
        Err(e) => {
            warn!("MoQ PTT: failed to create PTT publish track: {:?}", e);
            return false;
        }
    }

    // Create AI audio publish track
    let ai_pub_track_name =
        FullTrackName::new(ai_pub_ns.clone(), params.ai_pub_track_name.clone().into_bytes());
    match block_on(client.publish(ai_pub_track_name)) {
        Ok(track) => {
            *ai_pub_track = Some(track);
            *ai_group_id = device_id;
            *ai_object_id = 0;
            info!(
                "MoQ PTT: created AI audio publish track (group_id={})",
                device_id
            );
        }
        Err(e) => {
            warn!("MoQ PTT: failed to create AI audio publish track: {:?}", e);
        }
    }

    // Subscribe to PTT channel (receive from others)
    let ptt_sub_track_name =
        FullTrackName::new(ptt_ns.clone(), params.ptt_track_name.clone().into_bytes());
    info!(
        "MoQ PTT: subscribe namespace={}, track={}",
        ptt_ns, params.ptt_track_name
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

    // Subscribe to AI audio namespace
    let ai_sub_ns_strs: Vec<&str> = params.ai_sub_namespace.iter().map(|s| s.as_str()).collect();
    let ai_sub_ns = TrackNamespace::from_strings(&ai_sub_ns_strs);
    client.subscribe_namespace(&ai_sub_ns);

    // Subscribe to AI audio responses
    info!(
        "MoQ PTT: subscribing to AI responses on ns={} track=\"{}\" (device_id=0x{:012x})",
        ai_sub_ns, params.ai_sub_track_name, device_id
    );
    let ai_sub_track_name =
        FullTrackName::new(ai_sub_ns, params.ai_sub_track_name.clone().into_bytes());
    match block_on(client.subscribe(ai_sub_track_name)) {
        Ok(sub) => {
            *ai_subscription = Some(sub);
            info!("MoQ PTT: subscribed to AI audio responses");
        }
        Err(e) => {
            warn!("MoQ PTT: failed to subscribe to AI audio: {:?}", e);
        }
    }

    true
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
    initial_track_params: TrackParams,
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

            // Loopback mode (communicated from main loop)
            let mut loopback = NetLoopbackMode::Off;

            // PTT mode state (hactar-compatible)
            let mut ptt_pub_track: Option<std::sync::Arc<quicr::PublishTrack>> = None;
            let mut ai_pub_track: Option<std::sync::Arc<quicr::PublishTrack>> = None;
            let mut ptt_subscription: Option<Subscription> = None;
            let mut ai_subscription: Option<Subscription> = None;
            let mut ptt_group_id: u64 = 0;
            let mut ptt_object_id: u64 = 0;
            let mut ai_group_id: u64 = 0;
            let mut ai_object_id: u64 = 0;
            let mut active_ptt_channel = PttChannel::Ptt;
            let mut ptt_recv_count: u64 = 0;
            let mut ai_recv_count: u64 = 0;
            let mut last_stats = Instant::now();
            let mut track_params = initial_track_params;

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
                        ai_subscription = None;
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
                                        ptt_ready = setup_tracks_from_params(
                                            &c,
                                            &track_params,
                                            &mut ptt_pub_track,
                                            &mut ai_pub_track,
                                            &mut ptt_subscription,
                                            &mut ai_subscription,
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
                    Ok(MoqCommand::Reconfigure {
                        ptt_namespace,
                        ptt_track_name,
                        ai_pub_namespace,
                        ai_pub_track_name,
                        ai_sub_namespace,
                        ai_sub_track_name,
                    }) => {
                        info!("MoQ: reconfiguring tracks");
                        track_params = TrackParams {
                            ptt_namespace,
                            ptt_track_name,
                            ai_pub_namespace,
                            ai_pub_track_name,
                            ai_sub_namespace,
                            ai_sub_track_name,
                        };
                        // Tear down existing tracks
                        ptt_pub_track = None;
                        ai_pub_track = None;
                        ptt_subscription = None;
                        ai_subscription = None;
                        ptt_ready = false;
                        // Re-setup if connected
                        if let Some(ref c) = client {
                            ptt_ready = setup_tracks_from_params(
                                c,
                                &track_params,
                                &mut ptt_pub_track,
                                &mut ai_pub_track,
                                &mut ptt_subscription,
                                &mut ai_subscription,
                                &mut ptt_group_id,
                                &mut ptt_object_id,
                                &mut ai_group_id,
                                &mut ai_object_id,
                            );
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

                    // Drain AI subscription (receive AI responses)
                    if let Some(ref mut subscription) = ai_subscription {
                        while let Ok(object) = subscription.try_recv() {
                            ai_recv_count += 1;
                            if ai_recv_count <= 5 || ai_recv_count % 100 == 0 {
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

                    // Log stats every 2 seconds
                    if last_stats.elapsed() >= Duration::from_secs(2) {
                        let ai_sub_status = ai_subscription.as_ref().map(|s| s.status());
                        info!(
                            "MoQ PTT: ptt_pub={} ptt_recv={} ai_pub={} ai_recv={} ai_sub={:?} active={:?} loopback={:?}",
                            ptt_object_id, ptt_recv_count, ai_object_id, ai_recv_count, ai_sub_status, active_ptt_channel, loopback
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
                                ai_subscription = None;
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
                                        ptt_ready = setup_tracks_from_params(
                                            &c,
                                            &track_params,
                                            &mut ptt_pub_track,
                                            &mut ai_pub_track,
                                            &mut ptt_subscription,
                                            &mut ai_subscription,
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
// Config WebSocket Task
// ============================================================================

/// Spawn the config WebSocket task in a separate thread.
///
/// Connects to the config WebSocket URL with an Authorization header,
/// receives DeviceConfig JSON updates, and handles token refresh on
/// authentication failures.
fn spawn_config_task(
    config_url: String,
    access_token: String,
    refresh_token: String,
    token_url: String,
    event_tx: Sender<ConfigEvent>,
) {
    use esp_idf_svc::ws::client::{
        EspWebSocketClient, EspWebSocketClientConfig, WebSocketEventType,
    };

    thread::Builder::new()
        .name("config".to_string())
        .stack_size(8192)
        .spawn(move || {
            info!("config: task started, connecting to {}", config_url);

            let mut current_access_token = access_token;
            let mut current_refresh_token = refresh_token;
            let mut backoff_secs = 5u64;

            loop {
                // Build WebSocket config with Authorization header
                let auth_header = format!("Authorization: Bearer {}\r\n", current_access_token);
                let ws_config = EspWebSocketClientConfig {
                    headers: Some(&auth_header),
                    ..Default::default()
                };

                // Connect to WebSocket using callback-based API
                let event_tx_clone = event_tx.clone();
                let ws_result = EspWebSocketClient::new(
                    &config_url,
                    &ws_config,
                    Duration::from_secs(30),
                    move |event| {
                        if let Ok(event) = event {
                            match &event.event_type {
                                WebSocketEventType::Text(text) => {
                                    let _ = event_tx_clone
                                        .send(ConfigEvent::NewConfig(text.to_string()));
                                }
                                WebSocketEventType::Disconnected => {
                                    info!("config: WebSocket disconnected");
                                }
                                WebSocketEventType::Connected => {
                                    info!("config: WebSocket connected");
                                }
                                WebSocketEventType::Close(_) => {
                                    info!("config: WebSocket closed");
                                }
                                _ => {}
                            }
                        }
                    },
                );

                match ws_result {
                    Ok(_client) => {
                        info!("config: WebSocket client created, waiting for events");
                        backoff_secs = 5; // Reset backoff on successful connection

                        // Keep the client alive - ESP-IDF WebSocket client runs in its own task
                        // and delivers events via the callback. We just need to keep the client
                        // object alive. Sleep indefinitely.
                        loop {
                            thread::sleep(Duration::from_secs(60));
                        }
                    }
                    Err(e) => {
                        warn!("config: WebSocket connection failed: {:?}", e);
                        let _ = event_tx.send(ConfigEvent::Error(format!("{:?}", e)));

                        // Attempt token refresh if we have a token URL
                        if !token_url.is_empty() && !current_refresh_token.is_empty() {
                            info!("config: attempting token refresh");
                            match refresh_access_token(&token_url, &current_refresh_token) {
                                Ok((new_access, new_refresh)) => {
                                    info!("config: token refresh successful");
                                    let _ = event_tx.send(ConfigEvent::TokenRefreshed {
                                        access_token: new_access.clone(),
                                        refresh_token: new_refresh.clone(),
                                    });
                                    current_access_token = new_access;
                                    if !new_refresh.is_empty() {
                                        current_refresh_token = new_refresh;
                                    }
                                    backoff_secs = 5;
                                    continue; // Retry immediately with new token
                                }
                                Err(e) => {
                                    warn!("config: token refresh failed: {}", e);
                                }
                            }
                        }

                        // Exponential backoff
                        info!("config: retrying in {}s", backoff_secs);
                        thread::sleep(Duration::from_secs(backoff_secs));
                        backoff_secs = (backoff_secs * 2).min(60);
                    }
                }
            }
        })
        .expect("failed to spawn config thread");
}

/// Refresh the access token using an OAuth2 refresh_token grant.
fn refresh_access_token(
    token_url: &str,
    refresh_token: &str,
) -> Result<(String, String), String> {
    use embedded_svc::http::client::Client as HttpClient;
    use embedded_svc::io::Write as SvcWrite;
    use esp_idf_svc::http::client::{Configuration as HttpConfig, EspHttpConnection};

    let body = format!("grant_type=refresh_token&refresh_token={}", refresh_token);

    let http_config = HttpConfig {
        buffer_size: Some(2048),
        buffer_size_tx: Some(1024),
        ..Default::default()
    };

    let connection = EspHttpConnection::new(&http_config)
        .map_err(|e| format!("HTTP connection error: {:?}", e))?;

    let mut client = HttpClient::wrap(connection);

    let content_length = format!("{}", body.len());
    let headers = [
        ("Content-Type", "application/x-www-form-urlencoded"),
        ("Content-Length", content_length.as_str()),
    ];

    let mut request = client
        .post(token_url, &headers)
        .map_err(|e| format!("HTTP request error: {:?}", e))?;

    request
        .write_all(body.as_bytes())
        .map_err(|e| format!("HTTP write error: {:?}", e))?;

    request
        .flush()
        .map_err(|e| format!("HTTP flush error: {:?}", e))?;

    let mut response = request
        .submit()
        .map_err(|e| format!("HTTP submit error: {:?}", e))?;

    let status = response.status();
    if status != 200 {
        return Err(format!("Token refresh returned status {}", status));
    }

    let mut resp_buf = [0u8; 2048];
    let bytes_read = embedded_svc::utils::io::try_read_full(&mut response, &mut resp_buf)
        .map_err(|e| format!("HTTP read error: {:?}", e.0))?;

    let resp_str = core::str::from_utf8(&resp_buf[..bytes_read])
        .map_err(|_| "Invalid UTF-8 in token response".to_string())?;

    let parsed: serde_json::Value =
        serde_json::from_str(resp_str).map_err(|e| format!("JSON parse error: {}", e))?;

    let new_access = parsed["access_token"]
        .as_str()
        .ok_or("Missing access_token in response")?
        .to_string();

    let new_refresh = parsed
        .get("refresh_token")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    Ok((new_access, new_refresh))
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
    config_url: String,
    access_token: String,
    refresh_token: String,
    token_url: String,
    language: String,
    selected_channel: String,
    config_json: String,
}

// NVS key names (max 15 chars)
const NVS_KEY_WIFI_SSIDS: &str = "wifi_ssids";
const NVS_KEY_RELAY_URL: &str = "relay_url";
const NVS_KEY_CONFIG_URL: &str = "config_url";
const NVS_KEY_ACCESS_TOKEN: &str = "access_token";
const NVS_KEY_REFRESH_TOKEN: &str = "refresh_token";
const NVS_KEY_TOKEN_URL: &str = "token_url";
const NVS_KEY_LANGUAGE: &str = "language";
const NVS_KEY_SEL_CHANNEL: &str = "sel_channel";
const NVS_KEY_CONFIG_JSON: &str = "config_json";

/// Max size for config JSON blob in NVS
const MAX_CONFIG_JSON_LEN: usize = 20480;

impl NvsStorage {
    /// Load a UTF-8 string from NVS blob.
    fn load_string(nvs: &EspNvs<NvsDefault>, key: &str, max_len: usize) -> String {
        let mut buf = vec![0u8; max_len];
        match nvs.get_blob(key, &mut buf) {
            Ok(Some(data)) => {
                if let Ok(s) = core::str::from_utf8(data) {
                    info!("net: loaded {} from NVS ({}B)", key, s.len());
                    return s.to_string();
                }
            }
            Ok(None) => {}
            Err(e) => {
                warn!("net: failed to read {} from NVS: {:?}", key, e);
            }
        }
        String::new()
    }

    /// Save a UTF-8 string to NVS blob, or remove key if empty.
    fn save_string(
        nvs: &mut EspNvs<NvsDefault>,
        key: &str,
        value: &str,
    ) -> Result<(), esp_idf_svc::sys::EspError> {
        if !value.is_empty() {
            nvs.set_blob(key, value.as_bytes())?;
        } else {
            let _ = nvs.remove(key);
        }
        Ok(())
    }

    /// Load storage from NVS
    fn load(nvs: Option<EspNvs<NvsDefault>>) -> Self {
        let mut storage = Self {
            nvs,
            wifi_ssids: heapless::Vec::new(),
            relay_url: String::new(),
            config_url: String::new(),
            access_token: String::new(),
            refresh_token: String::new(),
            token_url: String::new(),
            language: "en".to_string(),
            selected_channel: String::new(),
            config_json: String::new(),
        };

        if let Some(ref nvs) = storage.nvs {
            // Load WiFi SSIDs
            let mut buf = [0u8; 512];
            match nvs.get_blob(NVS_KEY_WIFI_SSIDS, &mut buf) {
                Ok(Some(data)) => {
                    if let Ok(ssids) =
                        postcard::from_bytes::<heapless::Vec<WifiSsid, MAX_WIFI_SSIDS>>(data)
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

            storage.relay_url = Self::load_string(nvs, NVS_KEY_RELAY_URL, MAX_RELAY_URL_LEN);
            storage.config_url = Self::load_string(nvs, NVS_KEY_CONFIG_URL, MAX_RELAY_URL_LEN);
            storage.access_token = Self::load_string(nvs, NVS_KEY_ACCESS_TOKEN, 2048);
            storage.refresh_token = Self::load_string(nvs, NVS_KEY_REFRESH_TOKEN, 2048);
            storage.token_url = Self::load_string(nvs, NVS_KEY_TOKEN_URL, MAX_RELAY_URL_LEN);
            let lang = Self::load_string(nvs, NVS_KEY_LANGUAGE, 8);
            if !lang.is_empty() {
                storage.language = lang;
            }
            storage.selected_channel = Self::load_string(nvs, NVS_KEY_SEL_CHANNEL, 128);
            storage.config_json = Self::load_string(nvs, NVS_KEY_CONFIG_JSON, MAX_CONFIG_JSON_LEN);
        }

        storage
    }

    /// Save all storage to NVS.
    fn save(&mut self) -> Result<(), esp_idf_svc::sys::EspError> {
        let Some(ref mut nvs) = self.nvs else {
            warn!("net: NVS not available, cannot save");
            return Ok(());
        };

        // Save WiFi SSIDs
        if let Ok(serialized) = postcard::to_allocvec(&self.wifi_ssids) {
            nvs.set_blob(NVS_KEY_WIFI_SSIDS, &serialized)?;
            info!("net: saved {} WiFi SSIDs to NVS", self.wifi_ssids.len());
        }

        Self::save_string(nvs, NVS_KEY_RELAY_URL, &self.relay_url)?;
        Self::save_string(nvs, NVS_KEY_CONFIG_URL, &self.config_url)?;
        Self::save_string(nvs, NVS_KEY_ACCESS_TOKEN, &self.access_token)?;
        Self::save_string(nvs, NVS_KEY_REFRESH_TOKEN, &self.refresh_token)?;
        Self::save_string(nvs, NVS_KEY_TOKEN_URL, &self.token_url)?;
        Self::save_string(nvs, NVS_KEY_LANGUAGE, &self.language)?;
        Self::save_string(nvs, NVS_KEY_SEL_CHANNEL, &self.selected_channel)?;
        Self::save_string(nvs, NVS_KEY_CONFIG_JSON, &self.config_json)?;

        Ok(())
    }

    /// Save a single field to NVS without rewriting everything.
    fn save_field(&mut self, key: &str, value: &str) -> Result<(), esp_idf_svc::sys::EspError> {
        let Some(ref mut nvs) = self.nvs else {
            warn!("net: NVS not available, cannot save");
            return Ok(());
        };
        Self::save_string(nvs, key, value)
    }

    /// Parse stored config_json into DeviceConfig.
    fn parsed_config(&self) -> Option<DeviceConfig> {
        if self.config_json.is_empty() {
            return None;
        }
        serde_json::from_str(&self.config_json).ok()
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

    // Derive initial track params from stored config (or use defaults)
    let initial_track_params = storage
        .parsed_config()
        .and_then(|cfg| track_params_from_config(&cfg, &storage.language, &storage.selected_channel))
        .unwrap_or_else(default_track_params);

    spawn_moq_task(wifi_config, initial_relay_url, initial_track_params, moq_cmd_rx, moq_event_tx);

    // Config WebSocket task
    let (config_event_tx, config_event_rx) = mpsc::channel::<ConfigEvent>();
    let mut config_task_running = false;

    // Spawn config task if credentials are available
    if !storage.config_url.is_empty() && !storage.access_token.is_empty() {
        info!("net: spawning config task for {}", storage.config_url);
        spawn_config_task(
            storage.config_url.clone(),
            storage.access_token.clone(),
            storage.refresh_token.clone(),
            storage.token_url.clone(),
            config_event_tx.clone(),
        );
        config_task_running = true;
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

                // Spawn config task if SetConfigUrl/SetAccessToken provided credentials
                if !config_task_running
                    && (tlv_type == CtlToNet::SetConfigUrl
                        || tlv_type == CtlToNet::SetAccessToken)
                    && !storage.config_url.is_empty()
                    && !storage.access_token.is_empty()
                {
                    info!("net: spawning config task for {}", storage.config_url);
                    spawn_config_task(
                        storage.config_url.clone(),
                        storage.access_token.clone(),
                        storage.refresh_token.clone(),
                        storage.token_url.clone(),
                        config_event_tx.clone(),
                    );
                    config_task_running = true;
                }
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
                // Route received audio to appropriate jitter buffer based on channel_id
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

        // Config WebSocket event handling
        match config_event_rx.try_recv() {
            Ok(ConfigEvent::NewConfig(json)) => {
                info!("net: received config update ({}B)", json.len());
                match serde_json::from_str::<DeviceConfig>(&json) {
                    Ok(config) => {
                        // Store raw JSON in NVS
                        storage.config_json = json.clone();
                        let _ = storage.save_field(NVS_KEY_CONFIG_JSON, &json);

                        // Relay URL: update if changed
                        if config.relay_url != storage.relay_url {
                            info!(
                                "net: relay URL changed: {} -> {}",
                                storage.relay_url, config.relay_url
                            );
                            storage.relay_url = config.relay_url.clone();
                            let _ = storage.save_field(NVS_KEY_RELAY_URL, &config.relay_url);
                            let _ = moq_cmd_tx
                                .send(MoqCommand::SetRelayUrl(config.relay_url.clone()));
                        }

                        // WiFi networks: update if changed
                        let config_ssids: Vec<String> = config
                            .wifi_networks
                            .iter()
                            .map(|w| w.ssid.clone())
                            .collect();
                        let current_ssids: Vec<String> = storage
                            .wifi_ssids
                            .iter()
                            .map(|w| w.ssid.clone())
                            .collect();
                        if config_ssids != current_ssids {
                            info!("net: WiFi networks updated from config");
                            storage.wifi_ssids.clear();
                            for net in &config.wifi_networks {
                                let _ = storage.add_wifi_ssid(
                                    &net.ssid,
                                    net.password.as_deref().unwrap_or(""),
                                );
                            }
                            let _ = storage.save();
                        }

                        // Selected channel validity: fall back to first if not in config
                        if !storage.selected_channel.is_empty()
                            && !config
                                .channels
                                .iter()
                                .any(|c| c.display_name == storage.selected_channel)
                        {
                            if let Some(first) = config.channels.first() {
                                info!(
                                    "net: selected channel '{}' not in config, falling back to '{}'",
                                    storage.selected_channel, first.display_name
                                );
                                let channel_name = first.display_name.clone();
                                storage.selected_channel = channel_name.clone();
                                let _ = storage.save_field(
                                    NVS_KEY_SEL_CHANNEL,
                                    &channel_name,
                                );
                            }
                        }

                        // Track reconfiguration
                        if let Some(params) = track_params_from_config(
                            &config,
                            &storage.language,
                            &storage.selected_channel,
                        ) {
                            let _ = moq_cmd_tx.send(MoqCommand::Reconfigure {
                                ptt_namespace: params.ptt_namespace,
                                ptt_track_name: params.ptt_track_name,
                                ai_pub_namespace: params.ai_pub_namespace,
                                ai_pub_track_name: params.ai_pub_track_name,
                                ai_sub_namespace: params.ai_sub_namespace,
                                ai_sub_track_name: params.ai_sub_track_name,
                            });
                        }
                    }
                    Err(e) => {
                        warn!("net: failed to parse config JSON: {:?}", e);
                    }
                }
            }
            Ok(ConfigEvent::TokenRefreshed {
                access_token,
                refresh_token,
            }) => {
                info!("net: tokens refreshed, saving to NVS");
                storage.access_token = access_token.clone();
                let _ = storage.save_field(NVS_KEY_ACCESS_TOKEN, &access_token);
                if !refresh_token.is_empty() {
                    storage.refresh_token = refresh_token.clone();
                    let _ = storage.save_field(NVS_KEY_REFRESH_TOKEN, &refresh_token);
                }
            }
            Ok(ConfigEvent::Error(msg)) => {
                warn!("net: config task error: {}", msg);
            }
            Err(TryRecvError::Empty) => {}
            Err(TryRecvError::Disconnected) => {}
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

        let msg_type = u16::from_be_bytes([buf[4], buf[5]]);
        let length = u32::from_be_bytes([buf[6], buf[7], buf[8], buf[9]]) as usize;

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
    buf[4..6].copy_from_slice(&msg_type.to_be_bytes());
    buf[6..10].copy_from_slice(&(value.len() as u32).to_be_bytes());
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
    ptt_buffer: &JitterBuffer,
    ptt_ai_buffer: &JitterBuffer,
) {
    match msg_type {
        CtlToNet::Ping => {
            write_tlv(mgmt_uart, NetToCtl::Pong, value);
        }
        CtlToNet::CircularPing => {
            write_tlv(ui_uart, NetToUi::CircularPing, value);
        }
        CtlToNet::AddWifiSsid => {
            if let Ok(wifi) = postcard::from_bytes::<WifiSsid>(value) {
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
            if let Ok(serialized) = postcard::to_allocvec(&storage.wifi_ssids) {
                write_tlv(mgmt_uart, NetToCtl::WifiSsids, &serialized);
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
        CtlToNet::GetJitterStats => {
            let channel_id = value.first().copied().unwrap_or(0);
            let stats = match ChannelId::try_from(channel_id) {
                Ok(ChannelId::Ptt) => Some(ptt_buffer.stats()),
                Ok(ChannelId::PttAi) => Some(ptt_ai_buffer.stats()),
                _ => None,
            };
            if let Some(s) = stats {
                let info = link::JitterStatsInfo {
                    received: s.received as u32,
                    output: s.output as u32,
                    underruns: s.underruns as u32,
                    overruns: s.overruns as u32,
                    level: s.level as u16,
                    state: s.state,
                };
                let mut buf = [0u8; 64];
                if let Ok(serialized) = postcard::to_slice(&info, &mut buf) {
                    write_tlv(mgmt_uart, NetToCtl::JitterStats, serialized);
                } else {
                    write_tlv(mgmt_uart, NetToCtl::Error, b"serialize");
                }
            } else {
                write_tlv(mgmt_uart, NetToCtl::Error, b"invalid channel");
            }
        }
        CtlToNet::SetConfigUrl => {
            if let Ok(url) = core::str::from_utf8(value) {
                storage.config_url = url.to_string();
                if storage.save_field(NVS_KEY_CONFIG_URL, url).is_ok() {
                    write_tlv(mgmt_uart, NetToCtl::Ack, &[]);
                } else {
                    write_tlv(mgmt_uart, NetToCtl::Error, b"save");
                }
            } else {
                write_tlv(mgmt_uart, NetToCtl::Error, b"utf8");
            }
        }
        CtlToNet::SetAccessToken => {
            if let Ok(token) = core::str::from_utf8(value) {
                storage.access_token = token.to_string();
                if storage.save_field(NVS_KEY_ACCESS_TOKEN, token).is_ok() {
                    write_tlv(mgmt_uart, NetToCtl::Ack, &[]);
                } else {
                    write_tlv(mgmt_uart, NetToCtl::Error, b"save");
                }
            } else {
                write_tlv(mgmt_uart, NetToCtl::Error, b"utf8");
            }
        }
        CtlToNet::SetRefreshToken => {
            if let Ok(token) = core::str::from_utf8(value) {
                storage.refresh_token = token.to_string();
                if storage.save_field(NVS_KEY_REFRESH_TOKEN, token).is_ok() {
                    write_tlv(mgmt_uart, NetToCtl::Ack, &[]);
                } else {
                    write_tlv(mgmt_uart, NetToCtl::Error, b"save");
                }
            } else {
                write_tlv(mgmt_uart, NetToCtl::Error, b"utf8");
            }
        }
        CtlToNet::GetLanguage => {
            write_tlv(mgmt_uart, NetToCtl::Language, storage.language.as_bytes());
        }
        CtlToNet::SetLanguage => {
            if let Ok(lang) = core::str::from_utf8(value) {
                if Language::from_str_code(lang).is_some() {
                    storage.language = lang.to_string();
                    if storage.save_field(NVS_KEY_LANGUAGE, lang).is_ok() {
                        // Trigger reconfiguration if we have a config
                        if let Some(cfg) = storage.parsed_config() {
                            if let Some(params) = track_params_from_config(
                                &cfg,
                                &storage.language,
                                &storage.selected_channel,
                            ) {
                                let _ = moq_cmd_tx.send(MoqCommand::Reconfigure {
                                    ptt_namespace: params.ptt_namespace,
                                    ptt_track_name: params.ptt_track_name,
                                    ai_pub_namespace: params.ai_pub_namespace,
                                    ai_pub_track_name: params.ai_pub_track_name,
                                    ai_sub_namespace: params.ai_sub_namespace,
                                    ai_sub_track_name: params.ai_sub_track_name,
                                });
                            }
                        }
                        write_tlv(mgmt_uart, NetToCtl::Ack, &[]);
                    } else {
                        write_tlv(mgmt_uart, NetToCtl::Error, b"save");
                    }
                } else {
                    write_tlv(mgmt_uart, NetToCtl::Error, b"invalid language");
                }
            } else {
                write_tlv(mgmt_uart, NetToCtl::Error, b"utf8");
            }
        }
        CtlToNet::GetChannel => {
            if let Some(cfg) = storage.parsed_config() {
                // Find the selected channel (or first)
                let channel = cfg
                    .channels
                    .iter()
                    .find(|c| c.display_name == storage.selected_channel)
                    .or_else(|| cfg.channels.first());
                if let Some(ch) = channel {
                    let json = format!(
                        "{{\"id\":\"{}\",\"display_name\":\"{}\"}}",
                        ch.id, ch.display_name
                    );
                    write_tlv(mgmt_uart, NetToCtl::ChannelInfo, json.as_bytes());
                } else {
                    write_tlv(mgmt_uart, NetToCtl::Error, b"no channels");
                }
            } else {
                write_tlv(mgmt_uart, NetToCtl::Error, b"no config");
            }
        }
        CtlToNet::SetChannel => {
            if let Ok(name) = core::str::from_utf8(value) {
                if let Some(cfg) = storage.parsed_config() {
                    if cfg.channels.iter().any(|c| c.display_name == name) {
                        storage.selected_channel = name.to_string();
                        if storage.save_field(NVS_KEY_SEL_CHANNEL, name).is_ok() {
                            // Trigger reconfiguration
                            if let Some(params) = track_params_from_config(
                                &cfg,
                                &storage.language,
                                &storage.selected_channel,
                            ) {
                                let _ = moq_cmd_tx.send(MoqCommand::Reconfigure {
                                    ptt_namespace: params.ptt_namespace,
                                    ptt_track_name: params.ptt_track_name,
                                    ai_pub_namespace: params.ai_pub_namespace,
                                    ai_pub_track_name: params.ai_pub_track_name,
                                    ai_sub_namespace: params.ai_sub_namespace,
                                    ai_sub_track_name: params.ai_sub_track_name,
                                });
                            }
                            write_tlv(mgmt_uart, NetToCtl::Ack, &[]);
                        } else {
                            write_tlv(mgmt_uart, NetToCtl::Error, b"save");
                        }
                    } else {
                        write_tlv(mgmt_uart, NetToCtl::Error, b"channel not found");
                    }
                } else {
                    write_tlv(mgmt_uart, NetToCtl::Error, b"no config");
                }
            } else {
                write_tlv(mgmt_uart, NetToCtl::Error, b"utf8");
            }
        }
        CtlToNet::SetTokenUrl => {
            if let Ok(url) = core::str::from_utf8(value) {
                storage.token_url = url.to_string();
                if storage.save_field(NVS_KEY_TOKEN_URL, url).is_ok() {
                    write_tlv(mgmt_uart, NetToCtl::Ack, &[]);
                } else {
                    write_tlv(mgmt_uart, NetToCtl::Error, b"save");
                }
            } else {
                write_tlv(mgmt_uart, NetToCtl::Error, b"utf8");
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

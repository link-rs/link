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
    net::{
        ChannelConfig, JitterBuffer, JitterState, WifiSsid, MAX_CHANNELS, MAX_RELAY_URL_LEN,
        MAX_WIFI_SSIDS,
    },
    uart_config, ChannelId, Color, MgmtToNet, NetToMgmt, NetToUi, UiToNet, HEADER_SIZE,
    MAX_VALUE_SIZE, SYNC_WORD,
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
    /// Run clock mode - publish timestamps every second.
    RunClock,
    /// Run benchmark mode - publish at target FPS.
    RunBenchmark { fps: u32, payload_size: u32 },
    /// Send a chat message.
    SendChat { message: String },
    /// Stop the current mode.
    StopMode,
    /// Run MoQ loopback mode - publish audio to MoQ and subscribe to same track.
    RunMoqLoopback,
    /// Run MoQ publish mode - publish audio to MoQ without subscribing.
    RunPublish,
    /// Run PTT mode - interoperable with hactar devices.
    RunPtt,
    /// Audio frame to publish (used in MoQ loopback, publish, and PTT modes).
    /// For PTT mode: channel_id byte determines which track to publish on.
    AudioFrame { data: Vec<u8> },
    /// Set active PTT channel for publishing.
    SetPttChannel(PttChannel),
}

/// Events sent from the MoQ task back to the main loop.
#[allow(dead_code)]
enum MoqEvent {
    /// Connected to relay.
    Connected,
    /// Disconnected from relay.
    Disconnected,
    /// Mode started.
    ModeStarted,
    /// Mode stopped.
    ModeStopped,
    /// Error occurred.
    Error { message: String },
    /// Chat message sent successfully.
    ChatSent,
    /// Chat message received.
    ChatReceived { message: String },
    /// Audio frame received from MoQ subscription (for loopback mode).
    AudioReceived { data: Vec<u8> },
}

// ============================================================================
// MoQ Configuration (stored in main loop)
// ============================================================================

/// Runtime MoQ configuration.
struct MoqConfig {
    /// Target FPS for benchmark mode (0 = burst mode).
    benchmark_fps: u32,
    /// Payload size for benchmark mode.
    benchmark_payload_size: u32,
}

impl Default for MoqConfig {
    fn default() -> Self {
        Self {
            benchmark_fps: 50,
            benchmark_payload_size: 640,
        }
    }
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

/// Current mode the MoQ task is running.
#[derive(Clone, Copy, PartialEq, Eq, Default)]
enum MoqMode {
    #[default]
    Idle,
    Clock,
    Benchmark {
        fps: u32,
        payload_size: u32,
    },
    /// MoQ loopback - publish audio to MoQ and subscribe to same track.
    MoqLoopback,
    /// MoQ publish - publish audio to MoQ without subscribing.
    Publish,
    /// PTT mode - interoperable with hactar devices.
    Ptt,
}

/// Spawn the MoQ task in a separate thread.
fn spawn_moq_task(cmd_rx: Receiver<MoqCommand>, event_tx: Sender<MoqEvent>) {
    use std::sync::mpsc::TryRecvError;
    use std::time::Instant;

    thread::Builder::new()
        .name("moq".to_string())
        .stack_size(16384)
        .spawn(move || {
            info!("MoQ task started");

            let mut client: Option<quicr::Client> = None;
            let mut relay_url: Option<String> = None;
            let mut last_reconnect_attempt: Option<Instant> = None;
            let mut mode = MoqMode::Idle;

            // Clock mode state
            let mut clock_track: Option<std::sync::Arc<quicr::PublishTrack>> = None;
            let mut clock_group_id: u64 = 0;
            let mut last_clock_publish: Option<Instant> = None;

            // Benchmark mode state
            let mut benchmark_track: Option<std::sync::Arc<quicr::PublishTrack>> = None;
            let mut benchmark_group_id: u64 = 0;
            let mut last_benchmark_publish: Option<Instant> = None;
            let mut last_benchmark_report: Option<Instant> = None;
            let mut benchmark_packets_sent: u64 = 0;

            // MoQ loopback mode state
            let mut loopback_pub_track: Option<std::sync::Arc<quicr::PublishTrack>> = None;
            let mut loopback_subscription: Option<Subscription> = None;
            let mut loopback_group_id: u64 = 0;
            let mut loopback_object_id: u64 = 0;
            let mut loopback_recv_count: u64 = 0;
            let mut last_loopback_stats = Instant::now();

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

            loop {
                // Check for commands (non-blocking)
                match cmd_rx.try_recv() {
                    Ok(MoqCommand::SetRelayUrl(url)) => {
                        info!("MoQ: setting relay URL to {}", url);
                        // Stop any running mode and disconnect existing client
                        mode = MoqMode::Idle;
                        clock_track = None;
                        benchmark_track = None;
                        loopback_pub_track = None;
                        loopback_subscription = None;
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
                                        client = Some(c);
                                        let _ = event_tx.send(MoqEvent::Connected);
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
                    Ok(MoqCommand::RunClock) => {
                        if let Some(ref c) = client {
                            if mode == MoqMode::Idle {
                                info!("MoQ: starting clock mode");
                                let namespace = TrackNamespace::from_strings(&["hactar", "clock"]);
                                c.publish_namespace(&namespace);
                                let track_name =
                                    FullTrackName::from_strings(&["hactar", "clock"], "time");
                                match block_on(c.publish(track_name)) {
                                    Ok(track) => {
                                        clock_track = Some(track);
                                        clock_group_id = 0;
                                        last_clock_publish = None;
                                        mode = MoqMode::Clock;
                                        let _ = event_tx.send(MoqEvent::ModeStarted);
                                    }
                                    Err(e) => {
                                        warn!("MoQ: failed to create clock track: {:?}", e);
                                        let _ = event_tx.send(MoqEvent::Error {
                                            message: format!("{:?}", e),
                                        });
                                    }
                                }
                            }
                        } else {
                            warn!("MoQ: cannot start clock mode - not connected");
                            let _ = event_tx.send(MoqEvent::Error {
                                message: "not connected".to_string(),
                            });
                        }
                    }
                    Ok(MoqCommand::RunBenchmark { fps, payload_size }) => {
                        if let Some(ref c) = client {
                            if mode == MoqMode::Idle {
                                info!(
                                    "MoQ: starting benchmark mode (fps={}, size={})",
                                    fps, payload_size
                                );
                                let namespace =
                                    TrackNamespace::from_strings(&["hactar", "benchmark"]);
                                c.publish_namespace(&namespace);
                                let track_name =
                                    FullTrackName::from_strings(&["hactar", "benchmark"], "data");
                                match block_on(c.publish(track_name)) {
                                    Ok(track) => {
                                        benchmark_track = Some(track);
                                        benchmark_group_id = 0;
                                        last_benchmark_publish = None;
                                        last_benchmark_report = Some(Instant::now());
                                        benchmark_packets_sent = 0;
                                        mode = MoqMode::Benchmark { fps, payload_size };
                                        let _ = event_tx.send(MoqEvent::ModeStarted);
                                    }
                                    Err(e) => {
                                        warn!("MoQ: failed to create benchmark track: {:?}", e);
                                        let _ = event_tx.send(MoqEvent::Error {
                                            message: format!("{:?}", e),
                                        });
                                    }
                                }
                            }
                        } else {
                            warn!("MoQ: cannot start benchmark mode - not connected");
                            let _ = event_tx.send(MoqEvent::Error {
                                message: "not connected".to_string(),
                            });
                        }
                    }
                    Ok(MoqCommand::StopMode) => {
                        if mode != MoqMode::Idle {
                            info!("MoQ: stopping mode");
                            mode = MoqMode::Idle;
                            clock_track = None;
                            benchmark_track = None;
                            loopback_pub_track = None;
                            loopback_subscription = None;
                            ptt_pub_track = None;
                            ai_pub_track = None;
                            ptt_subscription = None;
                            ai_subscription = None;
                            let _ = event_tx.send(MoqEvent::ModeStopped);
                        }
                    }
                    Ok(MoqCommand::RunMoqLoopback) => {
                        if let Some(ref c) = client {
                            if mode == MoqMode::Idle {
                                info!("MoQ: starting MoQ loopback mode");
                                let namespace =
                                    TrackNamespace::from_strings(&["hactar", "loopback"]);
                                c.publish_namespace(&namespace);

                                // Create publish track
                                let pub_track_name =
                                    FullTrackName::from_strings(&["hactar", "loopback"], "audio");
                                match block_on(c.publish(pub_track_name.clone())) {
                                    Ok(track) => {
                                        loopback_pub_track = Some(track);
                                        loopback_group_id = 0;
                                        loopback_object_id = 0;

                                        // Create subscribe track to same namespace/track
                                        match block_on(c.subscribe(pub_track_name)) {
                                            Ok(sub_track) => {
                                                loopback_subscription = Some(sub_track);
                                                mode = MoqMode::MoqLoopback;
                                                let _ = event_tx.send(MoqEvent::ModeStarted);
                                            }
                                            Err(e) => {
                                                warn!(
                                                    "MoQ: failed to create loopback subscribe track: {:?}",
                                                    e
                                                );
                                                loopback_pub_track = None;
                                                let _ = event_tx.send(MoqEvent::Error {
                                                    message: format!("{:?}", e),
                                                });
                                            }
                                        }
                                    }
                                    Err(e) => {
                                        warn!(
                                            "MoQ: failed to create loopback publish track: {:?}",
                                            e
                                        );
                                        let _ = event_tx.send(MoqEvent::Error {
                                            message: format!("{:?}", e),
                                        });
                                    }
                                }
                            }
                        } else {
                            warn!("MoQ: cannot start MoQ loopback mode - not connected");
                            let _ = event_tx.send(MoqEvent::Error {
                                message: "not connected".to_string(),
                            });
                        }
                    }
                    Ok(MoqCommand::RunPublish) => {
                        if let Some(ref c) = client {
                            if mode == MoqMode::Idle {
                                info!("MoQ: starting publish mode");
                                let namespace =
                                    TrackNamespace::from_strings(&["hactar", "loopback"]);
                                c.publish_namespace(&namespace);

                                // Create publish track (same as loopback, but no subscription)
                                let pub_track_name =
                                    FullTrackName::from_strings(&["hactar", "loopback"], "audio");
                                match block_on(c.publish(pub_track_name)) {
                                    Ok(track) => {
                                        loopback_pub_track = Some(track);
                                        loopback_group_id = 0;
                                        loopback_object_id = 0;
                                        mode = MoqMode::Publish;
                                        let _ = event_tx.send(MoqEvent::ModeStarted);
                                    }
                                    Err(e) => {
                                        warn!("MoQ: failed to create publish track: {:?}", e);
                                        let _ = event_tx.send(MoqEvent::Error {
                                            message: format!("{:?}", e),
                                        });
                                    }
                                }
                            }
                        } else {
                            warn!("MoQ: cannot start publish mode - not connected");
                            let _ = event_tx.send(MoqEvent::Error {
                                message: "not connected".to_string(),
                            });
                        }
                    }
                    Ok(MoqCommand::RunPtt) => {
                        if let Some(ref c) = client {
                            if mode == MoqMode::Idle {
                                info!("MoQ: starting PTT mode (hactar-compatible)");

                                // Hactar track naming:
                                // Namespace prefix: moq://moq.ptt.arpa/v1/org/acme/store/1234
                                // PTT channel: .../channel/<name>/ptt + track pcm_en_8khz_mono_i16
                                // AI audio pub: .../ai/audio + track pcm_en_8khz_mono_i16
                                // AI audio sub: .../ai/audio + track <device_id>

                                let ns_prefix = &[
                                    "moq://moq.ptt.arpa/v1",
                                    "org/acme",
                                    "store/1234",
                                ];
                                let channel_name = "gardening"; // TODO: make configurable
                                let track_name = "pcm_en_8khz_mono_i16";

                                // Register PTT namespace for publishing
                                let ptt_ns = TrackNamespace::from_strings(&[
                                    ns_prefix[0], ns_prefix[1], ns_prefix[2],
                                    &format!("channel/{}", channel_name),
                                    "ptt",
                                ]);
                                c.publish_namespace(&ptt_ns);

                                // Register AI audio namespace for publishing
                                let ai_ns = TrackNamespace::from_strings(&[
                                    ns_prefix[0], ns_prefix[1], ns_prefix[2],
                                    "ai/audio",
                                ]);
                                c.publish_namespace(&ai_ns);

                                // Create PTT publish track
                                let ptt_track_name = FullTrackName::new(ptt_ns.clone(), track_name.as_bytes());
                                info!("MoQ PTT: publish namespace={}, track={}", ptt_ns, track_name);
                                match block_on(c.publish(ptt_track_name)) {
                                    Ok(track) => {
                                        ptt_pub_track = Some(track);
                                        ptt_group_id = 0;
                                        ptt_object_id = 0;
                                        info!("MoQ PTT: created PTT publish track");
                                    }
                                    Err(e) => {
                                        warn!("MoQ PTT: failed to create PTT publish track: {:?}", e);
                                    }
                                }

                                // Create AI audio publish track
                                let ai_pub_track_name = FullTrackName::new(ai_ns.clone(), track_name.as_bytes());
                                // Get device ID from MAC for AI group_id (janet uses this to route responses)
                                let device_id = get_device_id_from_mac();

                                match block_on(c.publish(ai_pub_track_name)) {
                                    Ok(track) => {
                                        ai_pub_track = Some(track);
                                        ai_group_id = device_id;
                                        ai_object_id = 0;
                                        info!("MoQ PTT: created AI audio publish track (group_id={})", device_id);
                                    }
                                    Err(e) => {
                                        warn!("MoQ PTT: failed to create AI audio publish track: {:?}", e);
                                    }
                                }

                                // Subscribe to PTT channel (receive from others)
                                let ptt_sub_track_name = FullTrackName::new(ptt_ns.clone(), track_name.as_bytes());
                                info!("MoQ PTT: subscribe namespace={}, track={}", ptt_ns, track_name);
                                match block_on(c.subscribe(ptt_sub_track_name)) {
                                    Ok(sub) => {
                                        ptt_subscription = Some(sub);
                                        info!("MoQ PTT: subscribed to PTT channel");
                                    }
                                    Err(e) => {
                                        warn!("MoQ PTT: failed to subscribe to PTT channel: {:?}", e);
                                    }
                                }

                                // Subscribe to AI audio responses (using device ID as track name)
                                // Device ID is derived from MAC address to match hactar/janet
                                // (already computed above for ai_group_id)
                                let device_id_str = format!("{}", device_id);
                                info!("MoQ PTT: subscribing to AI responses on track {}", device_id_str);
                                let ai_sub_track_name = FullTrackName::new(ai_ns, device_id_str.into_bytes());
                                match block_on(c.subscribe(ai_sub_track_name)) {
                                    Ok(sub) => {
                                        ai_subscription = Some(sub);
                                        info!("MoQ PTT: subscribed to AI audio responses");
                                    }
                                    Err(e) => {
                                        warn!("MoQ PTT: failed to subscribe to AI audio: {:?}", e);
                                    }
                                }

                                mode = MoqMode::Ptt;
                                active_ptt_channel = PttChannel::Ptt;
                                let _ = event_tx.send(MoqEvent::ModeStarted);
                            }
                        } else {
                            warn!("MoQ: cannot start PTT mode - not connected");
                            let _ = event_tx.send(MoqEvent::Error {
                                message: "not connected".to_string(),
                            });
                        }
                    }
                    Ok(MoqCommand::SetPttChannel(channel)) => {
                        active_ptt_channel = channel;
                        info!("MoQ PTT: active channel set to {:?}", channel);
                    }
                    Ok(MoqCommand::AudioFrame { data }) => {
                        // Publish based on current mode
                        match mode {
                            MoqMode::MoqLoopback | MoqMode::Publish => {
                                if let Some(ref track) = loopback_pub_track {
                                    let headers = ObjectHeaders::new(loopback_group_id, loopback_object_id);
                                    if let Err(e) = track.publish(&headers, &data) {
                                        warn!("MoQ loopback: publish failed at object {}: {:?}", loopback_object_id, e);
                                    }
                                    loopback_object_id += 1;
                                }
                            }
                            MoqMode::Ptt => {
                                // In PTT mode, route based on channel_id byte in data
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
                                            let headers = ObjectHeaders::new(ptt_group_id, ptt_object_id);
                                            let _ = track.publish(&headers, payload);
                                            ptt_object_id += 1;
                                        } else {
                                            warn!("MoQ PTT: ptt_pub_track is None");
                                        }
                                    }
                                    Ok(ChannelId::PttAi) => {
                                        if let Some(ref track) = ai_pub_track {
                                            let headers = ObjectHeaders::new(ai_group_id, ai_object_id);
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
                            _ => {}
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

                // Run mode-specific logic
                match mode {
                    MoqMode::Idle => {}
                    MoqMode::Clock => {
                        if let Some(ref track) = clock_track {
                            let now = Instant::now();
                            let should_publish = last_clock_publish
                                .map(|last| now.duration_since(last) >= Duration::from_secs(1))
                                .unwrap_or(true);

                            if should_publish {
                                let timestamp = std::time::SystemTime::now()
                                    .duration_since(std::time::UNIX_EPOCH)
                                    .unwrap_or_default();
                                let payload = format!(
                                    "{}.{:03}",
                                    timestamp.as_secs(),
                                    timestamp.subsec_millis()
                                );

                                let headers = ObjectHeaders::new(clock_group_id, 0);
                                let _ = track.publish(&headers, payload.as_bytes());
                                clock_group_id += 1;
                                last_clock_publish = Some(now);
                            }
                        }
                    }
                    MoqMode::Benchmark { fps, payload_size } => {
                        if let Some(ref track) = benchmark_track {
                            let now = Instant::now();

                            // Determine if we should publish based on FPS
                            let interval_us = if fps == 0 { 0 } else { 1_000_000 / fps as u64 };
                            let should_publish = if fps == 0 {
                                true // Burst mode
                            } else {
                                last_benchmark_publish
                                    .map(|last| {
                                        now.duration_since(last).as_micros() as u64 >= interval_us
                                    })
                                    .unwrap_or(true)
                            };

                            if should_publish {
                                // Create payload (heap allocated)
                                let payload: Vec<u8> = (0..payload_size as usize)
                                    .map(|i| (i & 0xFF) as u8)
                                    .collect();

                                let headers = ObjectHeaders::new(benchmark_group_id, 0);
                                let _ = track.publish(&headers, &payload);

                                benchmark_group_id += 1;
                                benchmark_packets_sent += 1;
                                last_benchmark_publish = Some(now);
                            }

                            // Report stats every second
                            if let Some(last_report) = last_benchmark_report {
                                if now.duration_since(last_report) >= Duration::from_secs(1) {
                                    let elapsed = now.duration_since(last_report).as_secs_f64();
                                    let actual_fps = benchmark_packets_sent as f64 / elapsed;
                                    let throughput_kbps =
                                        (benchmark_packets_sent as f64 * payload_size as f64 * 8.0)
                                            / elapsed
                                            / 1000.0;
                                    info!(
                                        "MoQ benchmark: {:.1} fps, {:.1} kbps",
                                        actual_fps, throughput_kbps
                                    );
                                    last_benchmark_report = Some(now);
                                    benchmark_packets_sent = 0;
                                }
                            }
                        }
                    }
                    MoqMode::MoqLoopback => {
                        // Drain all ready objects from subscription
                        if let Some(ref mut subscription) = loopback_subscription {
                            while let Ok(object) = subscription.try_recv() {
                                loopback_recv_count += 1;
                                let _ = event_tx.send(MoqEvent::AudioReceived {
                                    data: object.payload().to_vec(),
                                });
                            }
                        }

                        // Log stats every 2 seconds
                        if last_loopback_stats.elapsed() >= Duration::from_secs(2) {
                            let sub_status = loopback_subscription
                                .as_ref()
                                .map(|s| format!("{:?}", s.status()));
                            let pub_status = loopback_pub_track
                                .as_ref()
                                .map(|t| format!("{:?}", t.status()));
                            info!(
                                "MoQ loopback: pub={} ({:?}), recv={} ({:?})",
                                loopback_object_id, pub_status, loopback_recv_count, sub_status
                            );
                            last_loopback_stats = Instant::now();
                        }
                    }
                    MoqMode::Publish => {
                        // Log stats every 2 seconds
                        if last_loopback_stats.elapsed() >= Duration::from_secs(2) {
                            let pub_status = loopback_pub_track
                                .as_ref()
                                .map(|t| format!("{:?}", t.status()));
                            info!(
                                "MoQ publish: pub={} ({:?})",
                                loopback_object_id, pub_status
                            );
                            last_loopback_stats = Instant::now();
                        }
                    }
                    MoqMode::Ptt => {
                        // Drain PTT subscription (receive from other users)
                        // Note: We filter out objects we published ourselves by checking
                        // if the object_id matches what we recently published
                        if let Some(ref mut subscription) = ptt_subscription {
                            while let Ok(object) = subscription.try_recv() {
                                // Skip objects that appear to be our own (same group, object < our counter)
                                // This is a heuristic - proper fix would be tracking publisher ID
                                let dominated_by_self = object.headers.group_id == ptt_group_id
                                    && object.headers.object_id < ptt_object_id;
                                if dominated_by_self {
                                    // Likely our own echo, skip it
                                    continue;
                                }

                                ptt_recv_count += 1;
                                if ptt_recv_count <= 5 || ptt_recv_count % 100 == 0 {
                                    info!("MoQ PTT: recv ptt audio, group={} obj={} len={}",
                                        object.headers.group_id, object.headers.object_id, object.payload().len());
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
                                // Forward AI audio to UI with channel_id prefix
                                let mut data = Vec::with_capacity(1 + object.payload().len());
                                data.push(ChannelId::PttAi as u8);
                                data.extend_from_slice(object.payload());
                                let _ = event_tx.send(MoqEvent::AudioReceived { data });
                            }
                        }

                        // Log stats every 2 seconds
                        if last_loopback_stats.elapsed() >= Duration::from_secs(2) {
                            info!(
                                "MoQ PTT: ptt_pub={} ptt_recv={} ai_pub={} ai_recv={} active={:?}",
                                ptt_object_id, ptt_recv_count, ai_object_id, ai_recv_count, active_ptt_channel
                            );
                            last_loopback_stats = Instant::now();
                        }
                    }
                }

                // Reconnection logic: if we have a URL but no client, try to reconnect
                if client.is_none() {
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
                                        client = Some(c);
                                        let _ = event_tx.send(MoqEvent::Connected);
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
    channels: heapless::Vec<ChannelConfig, MAX_CHANNELS>,
}

// NVS key names (max 15 chars)
const NVS_KEY_WIFI_SSIDS: &str = "wifi_ssids";
const NVS_KEY_RELAY_URL: &str = "relay_url";
const NVS_KEY_CHANNELS: &str = "channels";

impl NvsStorage {
    /// Load storage from NVS
    fn load(nvs: Option<EspNvs<NvsDefault>>) -> Self {
        let mut storage = Self {
            nvs,
            wifi_ssids: heapless::Vec::new(),
            relay_url: String::new(),
            channels: heapless::Vec::new(),
        };

        // Load WiFi SSIDs
        if let Some(ref nvs) = storage.nvs {
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

            // Load channel configurations
            let mut channel_buf = [0u8; 512];
            match nvs.get_blob(NVS_KEY_CHANNELS, &mut channel_buf) {
                Ok(Some(data)) => {
                    if let Ok(channels) =
                        postcard::from_bytes::<heapless::Vec<ChannelConfig, MAX_CHANNELS>>(data)
                    {
                        storage.channels = channels;
                        info!(
                            "net: loaded {} channel configs from NVS",
                            storage.channels.len()
                        );
                    }
                }
                Ok(None) => {}
                Err(e) => {
                    warn!("net: failed to read channel configs from NVS: {:?}", e);
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
        if let Ok(serialized) = postcard::to_allocvec(&self.wifi_ssids) {
            nvs.set_blob(NVS_KEY_WIFI_SSIDS, &serialized)?;
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

        // Save channel configurations
        if !self.channels.is_empty() {
            if let Ok(serialized) = postcard::to_allocvec(&self.channels) {
                nvs.set_blob(NVS_KEY_CHANNELS, &serialized)?;
                info!("net: saved {} channel configs to NVS", self.channels.len());
            }
        } else {
            let _ = nvs.remove(NVS_KEY_CHANNELS);
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

    /// Get configuration for a specific channel.
    fn get_channel_config(&self, channel_id: u8) -> Option<&ChannelConfig> {
        self.channels.iter().find(|c| c.channel_id == channel_id)
    }

    /// Set configuration for a channel.
    /// Replaces existing config for that channel_id or adds new one.
    fn set_channel_config(&mut self, config: ChannelConfig) -> Result<(), ()> {
        if let Some(existing) = self.channels.iter_mut().find(|c| c.channel_id == config.channel_id)
        {
            *existing = config;
        } else {
            self.channels.push(config).map_err(|_| ())?;
        }
        Ok(())
    }

    /// Clear all channel configurations.
    fn clear_channel_configs(&mut self) {
        self.channels.clear();
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

    // Initialize WiFi
    let wifi = esp_idf_svc::wifi::EspWifi::new(
        peripherals.modem,
        sys_loop.clone(),
        Some(nvs_partition.clone()),
    )
    .unwrap();
    let mut wifi = esp_idf_svc::wifi::BlockingWifi::wrap(wifi, sys_loop).unwrap();

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

    // MoQ channels and task
    let (moq_cmd_tx, moq_cmd_rx) = mpsc::channel::<MoqCommand>();
    let (moq_event_tx, moq_event_rx) = mpsc::channel::<MoqEvent>();
    spawn_moq_task(moq_cmd_rx, moq_event_tx);

    // Loopback mode state
    let mut loopback = false;

    // MoQ configuration
    let mut moq_config = MoqConfig::default();

    // Per-channel jitter buffers
    let mut ptt_buffer = JitterBuffer::new();
    let mut ptt_ai_buffer = JitterBuffer::new();
    let mut last_buffer_tick = Instant::now();
    const BUFFER_TICK_INTERVAL: Duration = Duration::from_millis(20);

    // Try to connect to WiFi if we have credentials
    if !storage.wifi_ssids.is_empty() {
        let wifi_ssid = &storage.wifi_ssids[0];
        info!("net: connecting to WiFi '{}'", wifi_ssid.ssid);

        if let Err(e) = connect_wifi(&mut wifi, &wifi_ssid.ssid, &wifi_ssid.password) {
            warn!("net: WiFi connect failed: {:?}", e);
        } else {
            info!("net: WiFi connected");
            set_led_color(&mut led_r, &mut led_g, &mut led_b, Color::Green);

            // Connect to MoQ relay if URL is stored
            if !storage.relay_url.is_empty() {
                info!("net: connecting to MoQ relay: {}", storage.relay_url);
                let _ = moq_cmd_tx.send(MoqCommand::SetRelayUrl(storage.relay_url.clone()));
            }
        }
    }

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
            if let Ok(tlv_type) = MgmtToNet::try_from(msg_type) {
                handle_mgmt_message(
                    tlv_type,
                    &value,
                    &mgmt_uart,
                    &ui_uart,
                    &mut storage,
                    &mut loopback,
                    &mut moq_config,
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

        // MoQ event handling
        use std::sync::mpsc::TryRecvError;
        match moq_event_rx.try_recv() {
            Ok(MoqEvent::Connected) => {
                info!("net: MoQ connected");
                set_led_color(&mut led_r, &mut led_g, &mut led_b, Color::Blue);
            }
            Ok(MoqEvent::Disconnected) => {
                info!("net: MoQ disconnected");
                set_led_color(&mut led_r, &mut led_g, &mut led_b, Color::Green);
            }
            Ok(MoqEvent::ModeStarted) => {
                info!("net: MoQ mode started");
                // Reset jitter buffers to clear any initial backlog
                ptt_buffer.reset();
                ptt_ai_buffer.reset();
            }
            Ok(MoqEvent::ModeStopped) => {
                info!("net: MoQ mode stopped");
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

/// Connect to WiFi network
fn connect_wifi(
    wifi: &mut esp_idf_svc::wifi::BlockingWifi<esp_idf_svc::wifi::EspWifi<'static>>,
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

    wifi.set_configuration(&config)?;
    wifi.start()?;
    wifi.connect()?;
    wifi.wait_netif_up()?;

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
    msg_type: MgmtToNet,
    value: &[u8],
    mgmt_uart: &UartDriver,
    ui_uart: &UartDriver,
    storage: &mut NvsStorage,
    loopback: &mut bool,
    moq: &mut MoqConfig,
    moq_cmd_tx: &Sender<MoqCommand>,
    ptt_buffer: &JitterBuffer,
    ptt_ai_buffer: &JitterBuffer,
) {
    match msg_type {
        MgmtToNet::Ping => {
            write_tlv(mgmt_uart, NetToMgmt::Pong, value);
        }
        MgmtToNet::CircularPing => {
            write_tlv(ui_uart, NetToUi::CircularPing, value);
        }
        MgmtToNet::AddWifiSsid => {
            if let Ok(wifi) = postcard::from_bytes::<WifiSsid>(value) {
                if storage.add_wifi_ssid(&wifi.ssid, &wifi.password).is_ok() {
                    if storage.save().is_ok() {
                        write_tlv(mgmt_uart, NetToMgmt::Ack, &[]);
                    } else {
                        write_tlv(mgmt_uart, NetToMgmt::Error, b"save");
                    }
                } else {
                    write_tlv(mgmt_uart, NetToMgmt::Error, b"add");
                }
            } else {
                write_tlv(mgmt_uart, NetToMgmt::Error, b"deserialize");
            }
        }
        MgmtToNet::GetWifiSsids => {
            if let Ok(serialized) = postcard::to_allocvec(&storage.wifi_ssids) {
                write_tlv(mgmt_uart, NetToMgmt::WifiSsids, &serialized);
            }
        }
        MgmtToNet::ClearWifiSsids => {
            storage.wifi_ssids.clear();
            if storage.save().is_ok() {
                write_tlv(mgmt_uart, NetToMgmt::Ack, &[]);
            } else {
                write_tlv(mgmt_uart, NetToMgmt::Error, b"save");
            }
        }
        MgmtToNet::GetRelayUrl => {
            write_tlv(mgmt_uart, NetToMgmt::RelayUrl, storage.relay_url.as_bytes());
        }
        MgmtToNet::SetRelayUrl => {
            if let Ok(url) = core::str::from_utf8(value) {
                storage.relay_url = url.to_string();
                if storage.save().is_ok() {
                    // Also trigger MoQ connection to new relay
                    let _ = moq_cmd_tx.send(MoqCommand::SetRelayUrl(url.to_string()));
                    write_tlv(mgmt_uart, NetToMgmt::Ack, &[]);
                } else {
                    write_tlv(mgmt_uart, NetToMgmt::Error, b"save");
                }
            } else {
                write_tlv(mgmt_uart, NetToMgmt::Error, b"utf8");
            }
        }
        MgmtToNet::WsSend | MgmtToNet::WsEchoTest | MgmtToNet::WsSpeedTest => {
            // WebSocket not implemented in ESP-IDF version
            write_tlv(mgmt_uart, NetToMgmt::Error, b"not implemented");
        }
        MgmtToNet::SetLoopback => {
            let enabled = value.first().copied().unwrap_or(0) != 0;
            *loopback = enabled;
            write_tlv(mgmt_uart, NetToMgmt::Ack, &[]);
        }
        MgmtToNet::GetLoopback => {
            write_tlv(mgmt_uart, NetToMgmt::Loopback, &[*loopback as u8]);
        }
        // MoQ commands
        MgmtToNet::GetBenchmarkFps => {
            write_tlv(
                mgmt_uart,
                NetToMgmt::BenchmarkFps,
                &moq.benchmark_fps.to_le_bytes(),
            );
        }
        MgmtToNet::SetBenchmarkFps => {
            if value.len() >= 4 {
                moq.benchmark_fps = u32::from_le_bytes([value[0], value[1], value[2], value[3]]);
                write_tlv(mgmt_uart, NetToMgmt::Ack, &[]);
            } else {
                write_tlv(mgmt_uart, NetToMgmt::Error, b"invalid fps");
            }
        }
        MgmtToNet::GetBenchmarkPayloadSize => {
            write_tlv(
                mgmt_uart,
                NetToMgmt::BenchmarkPayloadSize,
                &moq.benchmark_payload_size.to_le_bytes(),
            );
        }
        MgmtToNet::SetBenchmarkPayloadSize => {
            if value.len() >= 4 {
                moq.benchmark_payload_size =
                    u32::from_le_bytes([value[0], value[1], value[2], value[3]]);
                write_tlv(mgmt_uart, NetToMgmt::Ack, &[]);
            } else {
                write_tlv(mgmt_uart, NetToMgmt::Error, b"invalid size");
            }
        }
        MgmtToNet::RunClock => {
            let _ = moq_cmd_tx.send(MoqCommand::RunClock);
            write_tlv(mgmt_uart, NetToMgmt::Ack, &[]);
        }
        MgmtToNet::RunBenchmark => {
            let _ = moq_cmd_tx.send(MoqCommand::RunBenchmark {
                fps: moq.benchmark_fps,
                payload_size: moq.benchmark_payload_size,
            });
            write_tlv(mgmt_uart, NetToMgmt::Ack, &[]);
        }
        MgmtToNet::StopMode => {
            let _ = moq_cmd_tx.send(MoqCommand::StopMode);
            write_tlv(mgmt_uart, NetToMgmt::Ack, &[]);
        }
        MgmtToNet::SendChatMessage => {
            // Chat not implemented yet
            write_tlv(mgmt_uart, NetToMgmt::Error, b"not implemented");
        }
        MgmtToNet::RunMoqLoopback => {
            let _ = moq_cmd_tx.send(MoqCommand::RunMoqLoopback);
            write_tlv(mgmt_uart, NetToMgmt::Ack, &[]);
        }
        MgmtToNet::RunPublish => {
            let _ = moq_cmd_tx.send(MoqCommand::RunPublish);
            write_tlv(mgmt_uart, NetToMgmt::Ack, &[]);
        }
        MgmtToNet::RunPtt => {
            let _ = moq_cmd_tx.send(MoqCommand::RunPtt);
            write_tlv(mgmt_uart, NetToMgmt::Ack, &[]);
        }
        // Channel configuration commands
        MgmtToNet::GetChannelConfig => {
            let channel_id = value.first().copied().unwrap_or(0);
            if let Some(config) = storage.get_channel_config(channel_id) {
                if let Ok(serialized) = postcard::to_allocvec(config) {
                    write_tlv(mgmt_uart, NetToMgmt::ChannelConfig, &serialized);
                } else {
                    write_tlv(mgmt_uart, NetToMgmt::Error, b"serialize");
                }
            } else {
                // Return default config for unconfigured channel
                let default_config = ChannelConfig {
                    channel_id,
                    enabled: false,
                    relay_url: heapless::String::new(),
                };
                if let Ok(serialized) = postcard::to_allocvec(&default_config) {
                    write_tlv(mgmt_uart, NetToMgmt::ChannelConfig, &serialized);
                } else {
                    write_tlv(mgmt_uart, NetToMgmt::Error, b"serialize");
                }
            }
        }
        MgmtToNet::SetChannelConfig => {
            if let Ok(config) = postcard::from_bytes::<ChannelConfig>(value) {
                if storage.set_channel_config(config).is_ok() {
                    if storage.save().is_ok() {
                        write_tlv(mgmt_uart, NetToMgmt::Ack, &[]);
                    } else {
                        write_tlv(mgmt_uart, NetToMgmt::Error, b"save");
                    }
                } else {
                    write_tlv(mgmt_uart, NetToMgmt::Error, b"set");
                }
            } else {
                write_tlv(mgmt_uart, NetToMgmt::Error, b"deserialize");
            }
        }
        MgmtToNet::GetAllChannelConfigs => {
            if let Ok(serialized) = postcard::to_allocvec(&storage.channels) {
                write_tlv(mgmt_uart, NetToMgmt::AllChannelConfigs, &serialized);
            } else {
                write_tlv(mgmt_uart, NetToMgmt::Error, b"serialize");
            }
        }
        MgmtToNet::ClearChannelConfigs => {
            storage.clear_channel_configs();
            if storage.save().is_ok() {
                write_tlv(mgmt_uart, NetToMgmt::Ack, &[]);
            } else {
                write_tlv(mgmt_uart, NetToMgmt::Error, b"save");
            }
        }
        MgmtToNet::GetJitterStats => {
            let channel_id = value.first().copied().unwrap_or(0);
            let stats = match ChannelId::try_from(channel_id) {
                Ok(ChannelId::Ptt) => Some(ptt_buffer.stats()),
                Ok(ChannelId::PttAi) => Some(ptt_ai_buffer.stats()),
                _ => None,
            };
            if let Some(s) = stats {
                // Serialize: received(4) + output(4) + underruns(4) + overruns(4) + level(2) + state(1) = 19 bytes
                let mut buf = [0u8; 19];
                buf[0..4].copy_from_slice(&s.received.to_le_bytes());
                buf[4..8].copy_from_slice(&s.output.to_le_bytes());
                buf[8..12].copy_from_slice(&s.underruns.to_le_bytes());
                buf[12..16].copy_from_slice(&s.overruns.to_le_bytes());
                buf[16..18].copy_from_slice(&(s.level as u16).to_le_bytes());
                buf[18] = s.state as u8;
                write_tlv(mgmt_uart, NetToMgmt::JitterStats, &buf);
            } else {
                write_tlv(mgmt_uart, NetToMgmt::Error, b"invalid channel");
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
    loopback: bool,
    moq_cmd_tx: &Sender<MoqCommand>,
) {
    match msg_type {
        UiToNet::CircularPing => {
            write_tlv(mgmt_uart, NetToMgmt::CircularPing, value);
        }
        UiToNet::AudioFrameA => {
            // Button A = PTT channel
            let _ = moq_cmd_tx.send(MoqCommand::SetPttChannel(PttChannel::Ptt));
            if loopback {
                // Local loopback - forward directly to UI
                write_tlv(ui_uart, NetToUi::AudioFrame, value);
            } else {
                // Send to MoQ task with channel_id prefix
                let mut data = Vec::with_capacity(1 + value.len());
                data.push(ChannelId::Ptt as u8);
                data.extend_from_slice(value);
                let _ = moq_cmd_tx.send(MoqCommand::AudioFrame { data });
            }
        }
        UiToNet::AudioFrameB => {
            // Button B = AI channel
            let _ = moq_cmd_tx.send(MoqCommand::SetPttChannel(PttChannel::Ai));
            if loopback {
                // Local loopback - forward directly to UI
                write_tlv(ui_uart, NetToUi::AudioFrame, value);
            } else {
                // Send to MoQ task with channel_id prefix
                let mut data = Vec::with_capacity(1 + value.len());
                data.push(ChannelId::PttAi as u8);
                data.extend_from_slice(value);
                let _ = moq_cmd_tx.send(MoqCommand::AudioFrame { data });
            }
        }
        UiToNet::AudioFrame => {
            // New hactar format: channel_id (1 byte) + encrypted payload
            // Handle same as legacy for now
            if loopback {
                write_tlv(ui_uart, NetToUi::AudioFrame, value);
            } else {
                let _ = moq_cmd_tx.send(MoqCommand::AudioFrame {
                    data: value.to_vec(),
                });
            }
        }
    }
}

//! NET chip firmware using ESP-IDF.
//!
//! This is the ESP-IDF version of the NET chip firmware, providing:
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
    net::{WifiSsid, MAX_RELAY_URL_LEN, MAX_WIFI_SSIDS},
    uart_config, Color, MgmtToNet, NetToMgmt, NetToUi, UiToNet, HEADER_SIZE, MAX_VALUE_SIZE,
    SYNC_WORD,
};
use log::{info, warn};
use quicr::{ClientBuilder, FullTrackName, ObjectHeaders, TrackNamespace};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::Duration;

// ============================================================================
// MoQ Command/Event Types
// ============================================================================

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
}

/// Spawn the MoQ task in a separate thread.
fn spawn_moq_task(cmd_rx: Receiver<MoqCommand>, event_tx: Sender<MoqEvent>) {
    use std::sync::mpsc::TryRecvError;
    use std::time::Instant;

    thread::Builder::new()
        .name("moq".to_string())
        .stack_size(32768)
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

            loop {
                // Check for commands (non-blocking)
                match cmd_rx.try_recv() {
                    Ok(MoqCommand::SetRelayUrl(url)) => {
                        info!("MoQ: setting relay URL to {}", url);
                        // Stop any running mode and disconnect existing client
                        mode = MoqMode::Idle;
                        clock_track = None;
                        benchmark_track = None;
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
                            .build()
                        {
                            Ok(c) => {
                                info!("MoQ: client created, connecting...");
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
                            let _ = event_tx.send(MoqEvent::ModeStopped);
                        }
                    }
                    Ok(cmd) => {
                        info!("MoQ: received command: {:?}", std::mem::discriminant(&cmd));
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

                                info!(
                                    "MoQ clock: published {} (ns={}, name={}, alias={:?})",
                                    payload,
                                    track.track_name().namespace,
                                    String::from_utf8_lossy(&track.track_name().name),
                                    track.track_alias()
                                );
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

// TEMPORARILY DISABLED: Full MoQ task implementation
/*
/// Run the MoQ task in a separate thread.
///
/// This spawns a thread that handles MoQ connection and operations using
/// ESP-IDF's native async support.
fn spawn_moq_task(_cmd_rx: Receiver<MoqCommand>, _event_tx: Sender<MoqEvent>) {
    // TEMPORARILY DISABLED: quicr may be causing pre-main crash
    info!("MoQ task DISABLED for debugging");
    thread::Builder::new()
        .name("moq".to_string())
        .stack_size(32768)
        .spawn(move || {
            info!("MoQ task started");

            // Run the async MoQ client loop
            block_on(moq_task_loop(cmd_rx, event_tx));
        })
        .expect("failed to spawn MoQ thread");
}
*/

// TEMPORARILY DISABLED: All quicr-dependent code commented out for debugging
/*
/// Async MoQ task loop - handles connection, reconnection, and modes.
async fn moq_task_loop(cmd_rx: Receiver<MoqCommand>, event_tx: Sender<MoqEvent>) {
    let mut relay_url: Option<std::string::String> = None;
    let mut client: Option<quicr::Client> = None;
    let mut current_mode = MoqMode::Idle;
    let stop_flag = Arc::new(AtomicBool::new(false));

    loop {
        // Check for commands (non-blocking)
        match cmd_rx.try_recv() {
            Ok(MoqCommand::SetRelayUrl(url)) => {
                if relay_url.as_ref() != Some(&url) {
                    info!("MoQ: relay URL set to {}", url);
                    relay_url = Some(url.clone());

                    // Disconnect existing client if any
                    if let Some(ref c) = client {
                        let _ = c.disconnect().await;
                        client = None;
                        let _ = event_tx.send(MoqEvent::Disconnected);
                    }

                    // Try to connect with new URL
                    match create_and_connect_client(&url).await {
                        Ok(c) => {
                            info!("MoQ: connected to {}", url);
                            client = Some(c);
                            let _ = event_tx.send(MoqEvent::Connected);
                        }
                        Err(e) => {
                            error!("MoQ: failed to connect: {:?}", e);
                            let _ = event_tx.send(MoqEvent::Error {
                                message: format!("{:?}", e)
                            });
                        }
                    }
                }
            }
            Ok(MoqCommand::RunClock) => {
                if client.is_some() && current_mode == MoqMode::Idle {
                    info!("MoQ: starting clock mode");
                    current_mode = MoqMode::Clock;
                    stop_flag.store(false, Ordering::SeqCst);
                    let _ = event_tx.send(MoqEvent::ModeStarted { mode: MoqExampleType::Clock });
                } else if client.is_none() {
                    let _ = event_tx.send(MoqEvent::Error {
                        message: "not connected".to_string()
                    });
                }
            }
            Ok(MoqCommand::RunBenchmark { fps, payload_size }) => {
                if client.is_some() && current_mode == MoqMode::Idle {
                    info!("MoQ: starting benchmark mode (fps={}, size={})", fps, payload_size);
                    current_mode = MoqMode::Benchmark { fps, payload_size };
                    stop_flag.store(false, Ordering::SeqCst);
                    let _ = event_tx.send(MoqEvent::ModeStarted { mode: MoqExampleType::Benchmark });
                } else if client.is_none() {
                    let _ = event_tx.send(MoqEvent::Error {
                        message: "not connected".to_string()
                    });
                }
            }
            Ok(MoqCommand::SendChat { message }) => {
                // TODO: Implement chat message sending
                info!("MoQ: would send chat: {}", message);
                let _ = event_tx.send(MoqEvent::ChatSent);
            }
            Ok(MoqCommand::StopMode) => {
                if current_mode != MoqMode::Idle {
                    info!("MoQ: stopping current mode");
                    stop_flag.store(true, Ordering::SeqCst);
                    current_mode = MoqMode::Idle;
                    let _ = event_tx.send(MoqEvent::ModeStopped);
                }
            }
            Err(TryRecvError::Empty) => {}
            Err(TryRecvError::Disconnected) => {
                info!("MoQ: command channel closed, exiting");
                break;
            }
        }

        // Run current mode if active
        if let Some(ref c) = client {
            match current_mode {
                MoqMode::Idle => {
                    // Just yield
                    sleep_ms(10).await;
                }
                MoqMode::Clock => {
                    if let Err(e) = run_clock_tick(c, &stop_flag).await {
                        error!("MoQ clock error: {:?}", e);
                        current_mode = MoqMode::Idle;
                        let _ = event_tx.send(MoqEvent::ModeStopped);
                    }
                }
                MoqMode::Benchmark { fps, payload_size } => {
                    if let Err(e) = run_benchmark_tick(c, fps, payload_size, &stop_flag).await {
                        error!("MoQ benchmark error: {:?}", e);
                        current_mode = MoqMode::Idle;
                        let _ = event_tx.send(MoqEvent::ModeStopped);
                    }
                }
            }
        } else {
            // Not connected, just yield
            sleep_ms(100).await;

            // Try to reconnect if we have a URL
            if let Some(ref url) = relay_url {
                match create_and_connect_client(url).await {
                    Ok(c) => {
                        info!("MoQ: reconnected to {}", url);
                        client = Some(c);
                        let _ = event_tx.send(MoqEvent::Connected);
                    }
                    Err(_) => {
                        // Will retry on next loop
                    }
                }
            }
        }
    }
}

/// Create and connect a quicr client.
async fn create_and_connect_client(relay_url: &str) -> Result<quicr::Client, quicr::Error> {
    let client = ClientBuilder::new()
        .endpoint_id(MOQ_ENDPOINT_ID)
        .connect_uri(relay_url)
        .build()?;

    client.connect().await?;
    Ok(client)
}

/// State for clock mode (persists across ticks).
/// SAFETY: Only accessed from MoQ task thread.
static mut CLOCK_STATE: ClockState = ClockState::new();

struct ClockState {
    track: Option<std::sync::Arc<quicr::PublishTrack>>,
    group_id: u64,
    last_publish: Option<std::time::Instant>,
}

impl ClockState {
    const fn new() -> Self {
        Self {
            track: None,
            group_id: 0,
            last_publish: None,
        }
    }
}

/// Run one tick of clock mode.
#[allow(static_mut_refs)]
async fn run_clock_tick(
    client: &quicr::Client,
    stop_flag: &AtomicBool,
) -> Result<(), quicr::Error> {
    if stop_flag.load(Ordering::SeqCst) {
        return Ok(());
    }

    // SAFETY: Only accessed from MoQ task thread
    let state = unsafe { &mut CLOCK_STATE };

    // Initialize track if needed
    if state.track.is_none() {
        let track_name = FullTrackName::from_strings(&["hactar", "clock"], "time");
        let namespace = TrackNamespace::from_strings(&["hactar", "clock"]);
        client.publish_namespace(&namespace);
        state.track = Some(client.publish(track_name).await?);
        info!("MoQ clock: track registered");
    }

    // Publish every second
    let now = std::time::Instant::now();
    let should_publish = state.last_publish
        .map(|last| now.duration_since(last) >= Duration::from_secs(1))
        .unwrap_or(true);

    if should_publish {
        if let Some(ref track) = state.track {
            let timestamp = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default();
            let payload = format!("{}.{:03}", timestamp.as_secs(), timestamp.subsec_millis());

            let headers = ObjectHeaders::new(state.group_id, 0);
            let _ = track.publish(&headers, payload.as_bytes());

            state.group_id += 1;
            state.last_publish = Some(now);
        }
    }

    // Small yield
    sleep_ms(10).await;
    Ok(())
}

/// State for benchmark mode.
static mut BENCHMARK_STATE: BenchmarkState = BenchmarkState::new();

struct BenchmarkState {
    track: Option<std::sync::Arc<quicr::PublishTrack>>,
    group_id: u64,
    last_publish: Option<std::time::Instant>,
    last_report: Option<std::time::Instant>,
    packets_sent: u64,
}

impl BenchmarkState {
    const fn new() -> Self {
        Self {
            track: None,
            group_id: 0,
            last_publish: None,
            last_report: None,
            packets_sent: 0,
        }
    }
}

/// Run one tick of benchmark mode.
#[allow(static_mut_refs)]
async fn run_benchmark_tick(
    client: &quicr::Client,
    fps: u32,
    payload_size: u32,
    stop_flag: &AtomicBool,
) -> Result<(), quicr::Error> {
    if stop_flag.load(Ordering::SeqCst) {
        return Ok(());
    }

    // SAFETY: Only accessed from MoQ task thread
    let state = unsafe { &mut BENCHMARK_STATE };

    // Initialize track if needed
    if state.track.is_none() {
        let track_name = FullTrackName::from_strings(&["hactar", "benchmark"], "data");
        let namespace = TrackNamespace::from_strings(&["hactar", "benchmark"]);
        client.publish_namespace(&namespace);
        state.track = Some(client.publish(track_name).await?);
        state.last_report = Some(std::time::Instant::now());
        info!("MoQ benchmark: track registered");
    }

    let now = std::time::Instant::now();

    // Determine if we should publish based on FPS
    let interval_us = if fps == 0 { 0 } else { 1_000_000 / fps as u64 };
    let should_publish = if fps == 0 {
        true // Burst mode
    } else {
        state.last_publish
            .map(|last| now.duration_since(last).as_micros() as u64 >= interval_us)
            .unwrap_or(true)
    };

    if should_publish {
        if let Some(ref track) = state.track {
            // Create payload
            let payload: Vec<u8> = (0..payload_size as usize)
                .map(|i| (i & 0xFF) as u8)
                .collect();

            let headers = ObjectHeaders::new(state.group_id, 0);
            let _ = track.publish(&headers, &payload);

            state.group_id += 1;
            state.packets_sent += 1;
            state.last_publish = Some(now);
        }
    }

    // Report stats every second
    if let Some(last_report) = state.last_report {
        if now.duration_since(last_report) >= Duration::from_secs(1) {
            let elapsed = now.duration_since(last_report).as_secs_f64();
            let actual_fps = state.packets_sent as f64 / elapsed;
            let throughput_kbps = (state.packets_sent as f64 * payload_size as f64 * 8.0) / elapsed / 1000.0;
            info!("MoQ benchmark: {:.1} fps, {:.1} kbps", actual_fps, throughput_kbps);
            state.last_report = Some(now);
            state.packets_sent = 0;
        }
    }

    // Small yield
    sleep_ms(1).await;
    Ok(())
}
*/ // END TEMPORARILY DISABLED quicr code

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
}

// NVS key names (max 15 chars)
const NVS_KEY_WIFI_SSIDS: &str = "wifi_ssids";
const NVS_KEY_RELAY_URL: &str = "relay_url";

impl NvsStorage {
    /// Load storage from NVS
    fn load(nvs: Option<EspNvs<NvsDefault>>) -> Self {
        let mut storage = Self {
            nvs,
            wifi_ssids: heapless::Vec::new(),
            relay_url: String::new(),
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
                            "net-idf: loaded {} WiFi SSIDs from NVS",
                            storage.wifi_ssids.len()
                        );
                    }
                }
                Ok(None) => {
                    info!("net-idf: no WiFi SSIDs in NVS");
                }
                Err(e) => {
                    warn!("net-idf: failed to read WiFi SSIDs from NVS: {:?}", e);
                }
            }

            // Load relay URL
            let mut url_buf = [0u8; MAX_RELAY_URL_LEN];
            match nvs.get_blob(NVS_KEY_RELAY_URL, &mut url_buf) {
                Ok(Some(data)) => {
                    if let Ok(url) = core::str::from_utf8(data) {
                        storage.relay_url = url.to_string();
                        info!("net-idf: loaded relay URL from NVS: {}", storage.relay_url);
                    }
                }
                Ok(None) => {
                    info!("net-idf: no relay URL in NVS");
                }
                Err(e) => {
                    warn!("net-idf: failed to read relay URL from NVS: {:?}", e);
                }
            }
        }

        storage
    }

    /// Save storage to NVS
    fn save(&mut self) -> Result<(), esp_idf_svc::sys::EspError> {
        let Some(ref mut nvs) = self.nvs else {
            warn!("net-idf: NVS not available, cannot save");
            return Ok(());
        };

        // Save WiFi SSIDs
        if let Ok(serialized) = postcard::to_allocvec(&self.wifi_ssids) {
            nvs.set_blob(NVS_KEY_WIFI_SSIDS, &serialized)?;
            info!("net-idf: saved {} WiFi SSIDs to NVS", self.wifi_ssids.len());
        }

        // Save relay URL
        if !self.relay_url.is_empty() {
            nvs.set_blob(NVS_KEY_RELAY_URL, self.relay_url.as_bytes())?;
            info!("net-idf: saved relay URL to NVS");
        } else {
            // Remove the key if URL is empty
            let _ = nvs.remove(NVS_KEY_RELAY_URL);
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
}

// ============================================================================
// Main
// ============================================================================

fn main() {
    esp_idf_svc::sys::link_patches();
    esp_idf_svc::log::EspLogger::initialize_default();

    info!("net-idf: initializing");

    let peripherals = Peripherals::take().unwrap();
    let sys_loop = EspSystemEventLoop::take().unwrap();
    let nvs_partition = EspDefaultNvsPartition::take().unwrap();

    // Initialize LED - RGB on GPIO 38, 37, 36 (active low)
    let mut led_r = PinDriver::output(peripherals.pins.gpio38).unwrap();
    let mut led_g = PinDriver::output(peripherals.pins.gpio37).unwrap();
    let mut led_b = PinDriver::output(peripherals.pins.gpio36).unwrap();

    // Start with red LED (WiFi not connected)
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

    info!("net-idf: UARTs initialized");

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
            warn!("net-idf: failed to open NVS: {:?}", e);
            None
        }
    };

    // Load storage from NVS
    let mut storage = NvsStorage::load(nvs);
    info!(
        "net-idf: loaded {} WiFi SSIDs from NVS",
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

    // Try to connect to WiFi if we have credentials
    if !storage.wifi_ssids.is_empty() {
        let wifi_ssid = &storage.wifi_ssids[0];
        info!("net-idf: connecting to WiFi '{}'", wifi_ssid.ssid);

        if let Err(e) = connect_wifi(&mut wifi, &wifi_ssid.ssid, &wifi_ssid.password) {
            warn!("net-idf: WiFi connect failed: {:?}", e);
        } else {
            info!("net-idf: WiFi connected");
            set_led_color(&mut led_r, &mut led_g, &mut led_b, Color::Yellow);

            // Connect to MoQ relay if URL is stored
            if !storage.relay_url.is_empty() {
                info!("net-idf: connecting to MoQ relay: {}", storage.relay_url);
                let _ = moq_cmd_tx.send(MoqCommand::SetRelayUrl(storage.relay_url.clone()));
            }
        }
    }

    info!("net-idf: starting main loop");

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
                );
            }
        }

        // Check UI UART for incoming data
        if let Some((msg_type, value)) = try_read_tlv(&ui_uart, &mut ui_rx_buf, &mut ui_rx_pos) {
            if let Ok(tlv_type) = UiToNet::try_from(msg_type) {
                handle_ui_message(tlv_type, &value, &mgmt_uart, &ui_uart, loopback);
            }
        }

        // MoQ event handling
        use std::sync::mpsc::TryRecvError;
        match moq_event_rx.try_recv() {
            Ok(MoqEvent::Connected) => {
                info!("net-idf: MoQ connected");
                set_led_color(&mut led_r, &mut led_g, &mut led_b, Color::Green);
            }
            Ok(MoqEvent::Disconnected) => {
                info!("net-idf: MoQ disconnected");
                set_led_color(&mut led_r, &mut led_g, &mut led_b, Color::Yellow);
            }
            Ok(MoqEvent::ModeStarted) => {
                info!("net-idf: MoQ mode started");
            }
            Ok(MoqEvent::ModeStopped) => {
                info!("net-idf: MoQ mode stopped");
            }
            Ok(MoqEvent::Error { message }) => {
                warn!("net-idf: MoQ error: {}", message);
            }
            Ok(MoqEvent::ChatSent) => {
                info!("net-idf: chat sent");
            }
            Ok(MoqEvent::ChatReceived { message }) => {
                info!("net-idf: chat received: {}", message);
            }
            Err(TryRecvError::Empty) => {}
            Err(TryRecvError::Disconnected) => {
                warn!("net-idf: MoQ event channel closed");
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
    let mut read_buf = [0u8; 64];
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
    uart.write(&SYNC_WORD).ok();
    let mut header = [0u8; HEADER_SIZE];
    header[0..2].copy_from_slice(&msg_type.to_be_bytes());
    header[2..6].copy_from_slice(&(value.len() as u32).to_be_bytes());
    uart.write(&header).ok();
    if !value.is_empty() {
        uart.write(value).ok();
    }
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
        // MoQ commands (relay URL uses storage - there's only one relay type)
        MgmtToNet::GetMoqRelayUrl => {
            write_tlv(
                mgmt_uart,
                NetToMgmt::MoqRelayUrl,
                storage.relay_url.as_bytes(),
            );
        }
        MgmtToNet::SetMoqRelayUrl => {
            if let Ok(url) = core::str::from_utf8(value) {
                storage.relay_url = url.to_string();
                if storage.save().is_ok() {
                    // Trigger connection to new relay
                    let _ = moq_cmd_tx.send(MoqCommand::SetRelayUrl(url.to_string()));
                    write_tlv(mgmt_uart, NetToMgmt::Ack, &[]);
                } else {
                    write_tlv(mgmt_uart, NetToMgmt::Error, b"save");
                }
            } else {
                write_tlv(mgmt_uart, NetToMgmt::Error, b"utf8");
            }
        }
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
    }
}

/// Handle message from UI chip
fn handle_ui_message(
    msg_type: UiToNet,
    value: &[u8],
    mgmt_uart: &UartDriver,
    ui_uart: &UartDriver,
    loopback: bool,
) {
    match msg_type {
        UiToNet::CircularPing => {
            write_tlv(mgmt_uart, NetToMgmt::CircularPing, value);
        }
        UiToNet::AudioFrameA | UiToNet::AudioFrameB => {
            if loopback {
                write_tlv(ui_uart, NetToUi::AudioFrame, value);
            }
        }
    }
}

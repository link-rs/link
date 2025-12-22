#![no_std]
#![no_main]

extern crate alloc;

use defmt::{info, warn};
use embassy_executor::Spawner;
use embassy_net::{tcp::TcpSocket, Runner, StackResources};
use embassy_futures::select::{select, Either};
use embassy_sync::{blocking_mutex::raw::CriticalSectionRawMutex, channel::Channel};
use embassy_time::{Duration, Timer};
use embedded_tls::{Aes128GcmSha256, NoVerify, TlsConfig, TlsConnection, TlsContext};
use embedded_websocket_embedded_io::{
    framer_async::{Framer, FramerError, ReadResult},
    WebSocketClient, WebSocketCloseStatusCode, WebSocketOptions, WebSocketSendMessageType,
};
use esp_bootloader_esp_idf::partitions;
use esp_hal::{
    clock::CpuClock,
    gpio::{Input, InputConfig, Level, Output, OutputConfig, Pull},
    rng::Rng,
    timer::timg::TimerGroup,
    uart::{
        Config, CtsConfig, HwFlowControl, Parity, RtsConfig, RxConfig, StopBits, SwFlowControl,
        Uart,
    },
};
use esp_radio::wifi::{
    ClientConfig, Config as WifiConfig, ModeConfig, ScanConfig, WifiController, WifiDevice,
    WifiEvent, WifiStaState,
};
use esp_storage::FlashStorage;
use heapless::Vec;
use link::net::{
    EchoTestResult, NetStorage, WsCommand, WsEvent, ECHO_TEST_PACKET_COUNT, MAX_RELAY_URL_LEN,
};
use rand_core::{CryptoRng, RngCore};
use static_cell::StaticCell;

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    defmt::error!("panic: {:?}", info);
    loop {}
}

esp_bootloader_esp_idf::esp_app_desc!();

macro_rules! mk_static {
    ($t:ty, $val:expr) => {{
        static STATIC_CELL: StaticCell<$t> = StaticCell::new();
        STATIC_CELL.uninit().write($val)
    }};
}

/// Channel for sending commands to the WebSocket task.
static WS_CMD_CHANNEL: StaticCell<Channel<CriticalSectionRawMutex, WsCommand, 4>> =
    StaticCell::new();

/// Channel for receiving events from the WebSocket task.
static WS_EVENT_CHANNEL: StaticCell<Channel<CriticalSectionRawMutex, WsEvent, 4>> =
    StaticCell::new();

#[esp_rtos::main]
async fn main(spawner: Spawner) {
    rtt_target::rtt_init_defmt!();

    info!("net: initializing");

    let config = esp_hal::Config::default().with_cpu_clock(CpuClock::max());
    let peripherals = esp_hal::init(config);

    esp_alloc::heap_allocator!(size: 72 * 1024);

    let timg0 = TimerGroup::new(peripherals.TIMG0);
    esp_rtos::start(timg0.timer0);

    let flow_ctl_disabled = HwFlowControl {
        cts: CtsConfig::Disabled,
        rts: RtsConfig::Disabled,
    };

    // UART to MGMT (UART0: GPIO43 TX, GPIO44 RX, 115200 8N1)
    let mgmt_config = Config::default()
        .with_baudrate(115200)
        .with_parity(Parity::None)
        .with_rx(RxConfig::default().with_fifo_full_threshold(1))
        .with_sw_flow_ctrl(SwFlowControl::Disabled)
        .with_hw_flow_ctrl(flow_ctl_disabled);
    let mgmt_uart = Uart::new(peripherals.UART0, mgmt_config)
        .unwrap()
        .with_tx(peripherals.GPIO43)
        .with_rx(peripherals.GPIO44)
        .into_async();
    let (from_mgmt, to_mgmt) = mgmt_uart.split();

    // UART to UI (UART1: GPIO17 TX, GPIO18 RX, 460800 8N2)
    let ui_config = Config::default()
        .with_baudrate(460800)
        .with_stop_bits(StopBits::_2)
        .with_rx(RxConfig::default().with_fifo_full_threshold(1))
        .with_hw_flow_ctrl(flow_ctl_disabled);
    let ui_uart = Uart::new(peripherals.UART1, ui_config)
        .unwrap()
        .with_tx(peripherals.GPIO17)
        .with_rx(peripherals.GPIO18)
        .into_async();
    let (from_ui, to_ui) = ui_uart.split();

    // Signal pins for MGMT synchronization (not yet used)
    let _signal_to_mgmt = Output::new(peripherals.GPIO15, Level::Low, OutputConfig::default());
    let _signal_from_mgmt = Input::new(
        peripherals.GPIO16,
        InputConfig::default().with_pull(Pull::Down),
    );

    // RGB LED
    let led = (
        Output::new(peripherals.GPIO38, Level::High, OutputConfig::default()),
        Output::new(peripherals.GPIO37, Level::High, OutputConfig::default()),
        Output::new(peripherals.GPIO36, Level::High, OutputConfig::default()),
    );

    // Flash storage for NET settings
    let mut flash = FlashStorage::new();
    let mut pt_buf = [0u8; partitions::PARTITION_TABLE_MAX_LEN];
    let pt = partitions::read_partition_table(&mut flash, &mut pt_buf)
        .expect("Failed to read partition table");
    let nvs = pt
        .find_partition(partitions::PartitionType::Data(
            partitions::DataPartitionSubType::Nvs,
        ))
        .expect("Failed to find NVS partition")
        .expect("NVS partition not found");
    let flash_offset = nvs.offset();
    info!("net: NVS partition at offset {:#x}", flash_offset);

    // Initialize WiFi
    let radio = esp_radio::init().expect("radio init");
    let radio = mk_static!(esp_radio::Controller<'static>, radio);
    let (controller, interfaces) =
        esp_radio::wifi::new(radio, peripherals.WIFI, WifiConfig::default()).expect("wifi init");

    // Initialize network stack
    let mut rng = EspRng(Rng::new());
    let seed = rng.next_u64();
    let (stack, runner) = embassy_net::new(
        interfaces.sta,
        embassy_net::Config::dhcpv4(Default::default()),
        mk_static!(StackResources<3>, StackResources::<3>::new()),
        seed,
    );

    // Initialize channels
    let ws_cmd_channel = WS_CMD_CHANNEL.init(Channel::new());
    let ws_event_channel = WS_EVENT_CHANNEL.init(Channel::new());

    // Spawn WiFi and network tasks
    // wifi_task reads SSIDs from storage on each reconnect attempt
    spawner.spawn(wifi_task(controller, flash_offset)).ok();
    spawner.spawn(net_task(runner)).ok();
    spawner
        .spawn(ws_task(
            stack,
            ws_cmd_channel.receiver(),
            ws_event_channel.sender(),
            rng,
        ))
        .ok();

    // Run the main App logic
    link::net::App::new(
        to_mgmt,
        from_mgmt,
        to_ui,
        from_ui,
        led,
        flash,
        flash_offset,
        ws_cmd_channel.sender(),
        ws_event_channel.receiver(),
    )
    .run()
    .await;
}

/// ESP32 hardware RNG wrapper
struct EspRng(Rng);

impl RngCore for EspRng {
    fn next_u32(&mut self) -> u32 {
        self.0.random()
    }
    fn next_u64(&mut self) -> u64 {
        (self.0.random() as u64) << 32 | self.0.random() as u64
    }
    fn fill_bytes(&mut self, dest: &mut [u8]) {
        for chunk in dest.chunks_mut(4) {
            let rand = self.0.random().to_le_bytes();
            chunk.copy_from_slice(&rand[..chunk.len()]);
        }
    }
    fn try_fill_bytes(&mut self, dest: &mut [u8]) -> Result<(), rand_core::Error> {
        self.fill_bytes(dest);
        Ok(())
    }
}

impl CryptoRng for EspRng {}

#[embassy_executor::task]
async fn wifi_task(mut controller: WifiController<'static>, flash_offset: u32) {
    info!("wifi: task started");

    // Start controller in client mode (needed for scanning)
    if !matches!(controller.is_started(), Ok(true)) {
        let config = ModeConfig::Client(ClientConfig::default());
        controller.set_config(&config).unwrap();
        controller.start_async().await.unwrap();
    }

    loop {
        // If connected, wait for disconnection
        if esp_radio::wifi::sta_state() == WifiStaState::Connected {
            controller.wait_for_event(WifiEvent::StaDisconnected).await;
            warn!("wifi: disconnected");
            Timer::after(Duration::from_secs(5)).await;
        }

        // Read WiFi credentials from storage on each reconnect attempt
        let storage = NetStorage::new(FlashStorage::new(), flash_offset);
        let wifi_ssids = storage.get_wifi_ssids().clone();
        drop(storage);

        if wifi_ssids.is_empty() {
            info!("wifi: no SSIDs configured, waiting...");
            Timer::after(Duration::from_secs(10)).await;
            continue;
        }

        // Scan for available networks
        info!("wifi: scanning...");
        let scan_result = controller
            .scan_with_config_async(ScanConfig::default())
            .await;

        let networks = match scan_result {
            Ok(networks) => networks,
            Err(e) => {
                warn!("wifi: scan failed: {:?}", e);
                Timer::after(Duration::from_secs(5)).await;
                continue;
            }
        };

        info!("wifi: found {} networks", networks.len());

        // Find matching SSIDs from our stored list, sorted by signal strength
        let mut best_match: Option<(&str, &str, i8)> = None; // (ssid, password, rssi)
        for network in &networks {
            for wifi in wifi_ssids.iter() {
                if network.ssid.as_str() == wifi.ssid.as_str() {
                    let dominated = best_match
                        .map(|(_, _, rssi)| network.signal_strength <= rssi)
                        .unwrap_or(false);
                    if !dominated {
                        best_match =
                            Some((wifi.ssid.as_str(), wifi.password.as_str(), network.signal_strength));
                    }
                }
            }
        }

        let Some((ssid, password, rssi)) = best_match else {
            info!("wifi: no matching networks found, rescanning in 10s");
            Timer::after(Duration::from_secs(10)).await;
            continue;
        };

        info!("wifi: connecting to '{}' (rssi: {})", ssid, rssi);

        // Configure and connect
        let config = ModeConfig::Client(
            ClientConfig::default()
                .with_ssid(ssid.try_into().unwrap())
                .with_password(password.try_into().unwrap()),
        );
        controller.set_config(&config).unwrap();

        match controller.connect_async().await {
            Ok(_) => {
                info!("wifi: connected to '{}'", ssid);
            }
            Err(e) => {
                warn!("wifi: connect failed: {:?}", e);
                Timer::after(Duration::from_secs(5)).await;
            }
        }
    }
}

#[embassy_executor::task]
async fn net_task(mut runner: Runner<'static, WifiDevice<'static>>) {
    runner.run().await
}

#[embassy_executor::task]
async fn ws_task(
    stack: embassy_net::Stack<'static>,
    cmd_rx: embassy_sync::channel::Receiver<'static, CriticalSectionRawMutex, WsCommand, 4>,
    event_tx: embassy_sync::channel::Sender<'static, CriticalSectionRawMutex, WsEvent, 4>,
    mut rng: EspRng,
) {
    info!("ws: task started, waiting for URL");

    // Current relay URL (empty until we receive a Connect command)
    let mut current_url: heapless::String<MAX_RELAY_URL_LEN> = heapless::String::new();

    loop {
        // Wait for WiFi to be ready
        while !stack.is_link_up() {
            // Check for commands while waiting
            if let Ok(cmd) = cmd_rx.try_receive() {
                match cmd {
                    WsCommand::Connect(url) => {
                        info!("ws: received URL while waiting for WiFi");
                        current_url = url;
                    }
                    WsCommand::Send(_) => {}  // Drop audio before connected
                    WsCommand::EchoTest => {
                        info!("ws: ignoring echo test while disconnected");
                        // Send empty result to indicate test couldn't run
                        event_tx
                            .send(WsEvent::EchoTestResult(EchoTestResult {
                                sent: 0,
                                received: 0,
                                buffered_output: 0,
                                raw_jitter_us: Vec::new(),
                                buffered_jitter_us: Vec::new(),
                                underruns: 0,
                            }))
                            .await;
                    }
                }
            }
            Timer::after(Duration::from_millis(100)).await;
        }

        // Wait for DHCP
        while stack.config_v4().is_none() {
            Timer::after(Duration::from_millis(100)).await;
        }

        // If no URL configured, wait for one
        if current_url.is_empty() {
            info!("ws: waiting for relay URL");
            loop {
                match cmd_rx.receive().await {
                    WsCommand::Connect(url) => {
                        current_url = url;
                        break;
                    }
                    WsCommand::Send(_) => {}  // Drop audio before connected
                    WsCommand::EchoTest => {
                        info!("ws: ignoring echo test without URL");
                        // Send empty result to indicate test couldn't run
                        event_tx
                            .send(WsEvent::EchoTestResult(EchoTestResult {
                                sent: 0,
                                received: 0,
                                buffered_output: 0,
                                raw_jitter_us: Vec::new(),
                                buffered_jitter_us: Vec::new(),
                                underruns: 0,
                            }))
                            .await;
                    }
                }
            }
        }

        // Parse URL to extract host and path (as owned values to avoid borrow issues)
        let (host, port, path): (heapless::String<64>, u16, heapless::String<64>) =
            match parse_wss_url(&current_url) {
                Some((h, p, pa)) => {
                    let Ok(host) = heapless::String::try_from(h) else {
                        warn!("ws: host too long");
                        current_url.clear();
                        continue;
                    };
                    let Ok(path) = heapless::String::try_from(pa) else {
                        warn!("ws: path too long");
                        current_url.clear();
                        continue;
                    };
                    (host, p, path)
                }
                None => {
                    warn!("ws: invalid URL: {}", current_url.as_str());
                    current_url.clear();
                    continue;
                }
            };

        info!(
            "ws: connecting to {}:{}{}",
            host.as_str(),
            port,
            path.as_str()
        );

        // DNS lookup
        let addr = match stack
            .dns_query(host.as_str(), embassy_net::dns::DnsQueryType::A)
            .await
        {
            Ok(addrs) if !addrs.is_empty() => addrs[0],
            _ => {
                warn!("ws: DNS failed for {}", host.as_str());
                event_tx.send(WsEvent::Disconnected).await;
                Timer::after(Duration::from_secs(5)).await;
                continue;
            }
        };

        // TCP connect
        let mut rx_buf = [0u8; 4096];
        let mut tx_buf = [0u8; 4096];
        let mut socket = TcpSocket::new(stack, &mut rx_buf, &mut tx_buf);
        socket.set_timeout(Some(Duration::from_secs(30)));

        if socket.connect((addr, port)).await.is_err() {
            warn!("ws: TCP connect failed");
            event_tx.send(WsEvent::Disconnected).await;
            Timer::after(Duration::from_secs(5)).await;
            continue;
        }
        info!("ws: TCP connected");

        // TLS handshake
        let mut tls_read = [0u8; 16640];
        let mut tls_write = [0u8; 16640];
        let tls_config = TlsConfig::new().with_server_name(host.as_str());
        let mut tls: TlsConnection<_, Aes128GcmSha256> =
            TlsConnection::new(socket, &mut tls_read, &mut tls_write);

        if tls
            .open::<_, NoVerify>(TlsContext::new(&tls_config, &mut rng))
            .await
            .is_err()
        {
            warn!("ws: TLS handshake failed");
            event_tx.send(WsEvent::Disconnected).await;
            Timer::after(Duration::from_secs(5)).await;
            continue;
        }
        info!("ws: TLS connected");

        // WebSocket handshake
        let mut ws_buf = [0u8; 1024];
        let websocket = WebSocketClient::new_client(&mut rng);
        let mut framer = Framer::new(websocket);
        let options = WebSocketOptions {
            path: path.as_str(),
            host: host.as_str(),
            origin: host.as_str(),
            sub_protocols: None,
            additional_headers: None,
        };
        if framer
            .connect(&mut tls, &mut ws_buf, &options)
            .await
            .is_err()
        {
            warn!("ws: WebSocket connect failed");
            event_tx.send(WsEvent::Disconnected).await;
            Timer::after(Duration::from_secs(5)).await;
            continue;
        }

        info!("ws: connected");
        event_tx.send(WsEvent::Connected).await;

        // Main WebSocket loop using select! for concurrent read/write
        let mut should_reconnect = false;
        let mut connection_broken = false;

        loop {
            match select(framer.read(&mut tls, &mut ws_buf), cmd_rx.receive()).await {
                Either::First(read_result) => {
                    // Frame received from server (or None if more data needed)
                    match read_result {
                        Some(Ok(ReadResult::Binary(data))) => {
                            info!("ws: received {} bytes", data.len());
                            if let Ok(payload) = Vec::try_from(data) {
                                event_tx.send(WsEvent::Received(payload)).await;
                            }
                        }
                        Some(Ok(ReadResult::Text(text))) => {
                            info!("ws: received text: {}", text);
                            if let Ok(payload) = Vec::try_from(text.as_bytes()) {
                                event_tx.send(WsEvent::Received(payload)).await;
                            }
                        }
                        Some(Ok(ReadResult::Close(close_msg))) => {
                            let code = match close_msg.status_code {
                                WebSocketCloseStatusCode::NormalClosure => 1000,
                                WebSocketCloseStatusCode::EndpointUnavailable => 1001,
                                WebSocketCloseStatusCode::ProtocolError => 1002,
                                WebSocketCloseStatusCode::InvalidMessageType => 1003,
                                WebSocketCloseStatusCode::Reserved => 1004,
                                WebSocketCloseStatusCode::Empty => 1005,
                                WebSocketCloseStatusCode::InvalidPayloadData => 1007,
                                WebSocketCloseStatusCode::PolicyViolation => 1008,
                                WebSocketCloseStatusCode::MessageTooBig => 1009,
                                WebSocketCloseStatusCode::MandatoryExtension => 1010,
                                WebSocketCloseStatusCode::InternalServerError => 1011,
                                WebSocketCloseStatusCode::TlsHandshake => 1015,
                                WebSocketCloseStatusCode::Custom(v) => v,
                            };
                            if let Ok(reason) = core::str::from_utf8(close_msg.reason) {
                                warn!("ws: received close frame: code={}, reason={}", code, reason);
                            } else {
                                warn!("ws: received close frame: code={}", code);
                            }
                            // Framer automatically sends close reply
                            break;
                        }
                        Some(Ok(ReadResult::Ping(_))) | Some(Ok(ReadResult::Pong(_))) => {
                            // Ping/Pong handled automatically by framer
                        }
                        Some(Err(e)) => {
                            match e {
                                FramerError::Io(_) => warn!("ws: read error: I/O error"),
                                FramerError::FrameTooLarge(size) => {
                                    warn!("ws: read error: frame too large ({})", size)
                                }
                                FramerError::Utf8(_) => warn!("ws: read error: UTF-8 decode error"),
                                FramerError::HttpHeader(_) => {
                                    warn!("ws: read error: HTTP header error")
                                }
                                FramerError::WebSocket(_) => {
                                    warn!("ws: read error: WebSocket protocol error")
                                }
                                FramerError::Disconnected => warn!("ws: read error: disconnected"),
                                FramerError::RxBufferTooSmall(size) => {
                                    warn!("ws: read error: rx buffer too small (need {})", size)
                                }
                            }
                            connection_broken = true;
                            break;
                        }
                        None => {
                            // Connection closed or need more data
                        }
                    }
                }
                Either::Second(cmd) => {
                    // Command received from application
                    match cmd {
                        WsCommand::Connect(url) => {
                            info!("ws: received new URL, reconnecting");
                            current_url = url;
                            should_reconnect = true;
                            break;
                        }
                        WsCommand::Send(data) => {
                            info!("ws: sending {} bytes", data.len());
                            match framer
                                .write(
                                    &mut tls,
                                    &mut ws_buf,
                                    WebSocketSendMessageType::Binary,
                                    true,
                                    &data,
                                )
                                .await
                            {
                                Ok(_) => {}
                                Err(e) => {
                                    match e {
                                        FramerError::Io(_) => warn!("ws: write failed: I/O error"),
                                        FramerError::FrameTooLarge(size) => {
                                            warn!("ws: write failed: frame too large ({})", size)
                                        }
                                        FramerError::Utf8(_) => {
                                            warn!("ws: write failed: UTF-8 error")
                                        }
                                        FramerError::HttpHeader(_) => {
                                            warn!("ws: write failed: HTTP header error")
                                        }
                                        FramerError::WebSocket(_) => {
                                            warn!("ws: write failed: WebSocket protocol error")
                                        }
                                        FramerError::Disconnected => {
                                            warn!("ws: write failed: disconnected")
                                        }
                                        FramerError::RxBufferTooSmall(size) => {
                                            warn!("ws: write failed: buffer too small (need {})", size)
                                        }
                                    }
                                    connection_broken = true;
                                    break;
                                }
                            }
                        }
                        WsCommand::EchoTest => {
                            info!("ws: starting echo test");
                            let (result, connection_ok) =
                                run_echo_test(&mut framer, &mut tls, &mut ws_buf).await;
                            info!(
                                "ws: echo test complete: sent={}, received={}",
                                result.sent, result.received
                            );
                            event_tx.send(WsEvent::EchoTestResult(result)).await;
                            if !connection_ok {
                                warn!("ws: connection failed during echo test, reconnecting");
                                connection_broken = true;
                                break;
                            }
                        }
                    }
                }
            }
        }

        // Clean up - only try to close if connection is still healthy
        // (the framer library panics on I/O errors in close)
        if !connection_broken {
            let _ = framer
                .close(
                    &mut tls,
                    &mut ws_buf,
                    WebSocketCloseStatusCode::NormalClosure,
                    None,
                )
                .await;
        }

        if !should_reconnect {
            event_tx.send(WsEvent::Disconnected).await;
            Timer::after(Duration::from_secs(5)).await;
        }
    }
}

/// Run the WebSocket echo test with jitter buffer analysis.
///
/// Sends ECHO_TEST_PACKET_COUNT packets (640 bytes each) at 20ms intervals (50 fps),
/// receives echoed packets through a jitter buffer, and records:
/// - Raw jitter: inter-arrival times before the buffer
/// - Buffered jitter: inter-departure times after the buffer
///
/// Returns (result, connection_ok). If connection_ok is false, the caller
/// should break out of the main loop to trigger reconnection.
async fn run_echo_test<'a, S>(
    framer: &mut Framer<&'a mut EspRng, embedded_websocket_embedded_io::Client>,
    tls: &mut TlsConnection<'_, S, Aes128GcmSha256>,
    ws_buf: &mut [u8],
) -> (EchoTestResult, bool)
where
    S: embedded_io_async::Read + embedded_io_async::Write + Unpin,
{
    use embassy_time::Instant;
    use link::net::JitterBuffer;

    // Packet payload: 640 bytes (FRAME_SIZE equivalent)
    const PACKET_SIZE: usize = 640;
    let packet = [0xAAu8; PACKET_SIZE];

    let mut sent: u8 = 0;
    let mut received: u8 = 0;
    let mut raw_jitter_us: Vec<u32, ECHO_TEST_PACKET_COUNT> = Vec::new();
    let mut last_recv_time: Option<Instant> = None;
    let mut connection_ok = true;

    // Jitter buffer for smoothing output
    let mut jitter_buffer: JitterBuffer<PACKET_SIZE> = JitterBuffer::new();

    // Phase 1: Send packets at 20ms intervals (50 fps) while also receiving
    let mut next_send_time = Instant::now();
    let send_interval = Duration::from_millis(20);

    info!("ws: echo test phase 1 - sending {} packets", ECHO_TEST_PACKET_COUNT);

    while (sent as usize) < ECHO_TEST_PACKET_COUNT {
        // Time to send next packet?
        if Instant::now() >= next_send_time {
            match framer
                .write(tls, ws_buf, WebSocketSendMessageType::Binary, true, &packet)
                .await
            {
                Ok(_) => {
                    sent += 1;
                    if sent == 1 || sent % 10 == 0 {
                        info!("ws: echo test sent {} packets", sent);
                    }
                    next_send_time = Instant::now() + send_interval;
                }
                Err(_) => {
                    warn!("ws: echo test send failed at packet {}", sent);
                    connection_ok = false;
                    break;
                }
            }
        }

        // Drain all available packets until next send time
        loop {
            let now = Instant::now();
            if now >= next_send_time {
                break; // Time to send again
            }
            let timeout = next_send_time.saturating_duration_since(now);
            match embassy_time::with_timeout(timeout, framer.read(tls, ws_buf)).await {
                Ok(Some(Ok(ReadResult::Binary(data)))) => {
                    let recv_time = Instant::now();
                    // Record raw jitter (before buffer)
                    if let Some(last) = last_recv_time {
                        let delta_us = recv_time.duration_since(last).as_micros() as u32;
                        let _ = raw_jitter_us.push(delta_us);
                    }
                    last_recv_time = Some(recv_time);
                    received += 1;
                    // Push into jitter buffer
                    jitter_buffer.push(data);
                }
                Ok(Some(Ok(_))) => {
                    // Other frame types, ignore but keep draining
                }
                Ok(Some(Err(_))) => {
                    warn!("ws: echo test receive error");
                    connection_ok = false;
                    break;
                }
                Ok(None) => {
                    // Framer needs more data, keep reading
                }
                Err(_) => {
                    // Timeout - time to send next packet
                    break;
                }
            }
        }
        if !connection_ok {
            break;
        }
    }

    // Phase 2: Wait for remaining responses (up to 2 seconds) - only if connection still ok
    if connection_ok {
        info!("ws: echo test phase 2 - waiting for remaining packets");
        let deadline = Instant::now() + Duration::from_secs(2);
        while (received as usize) < (sent as usize) && Instant::now() < deadline {
            let timeout = deadline.saturating_duration_since(Instant::now());
            match embassy_time::with_timeout(timeout, framer.read(tls, ws_buf)).await {
                Ok(Some(Ok(ReadResult::Binary(data)))) => {
                    let recv_time = Instant::now();
                    // Record raw jitter (before buffer)
                    if let Some(last) = last_recv_time {
                        let delta_us = recv_time.duration_since(last).as_micros() as u32;
                        let _ = raw_jitter_us.push(delta_us);
                    }
                    last_recv_time = Some(recv_time);
                    received += 1;
                    // Push into jitter buffer
                    jitter_buffer.push(data);
                }
                Ok(Some(Ok(_))) => {
                    // Other frame types, ignore but keep waiting
                }
                Ok(Some(Err(_))) => {
                    warn!("ws: echo test phase 2 receive error");
                    connection_ok = false;
                    break;
                }
                Ok(None) => {
                    // Framer needs more data, keep reading
                }
                Err(_) => {
                    // Timeout expired
                    info!("ws: echo test phase 2 timeout, received {}/{}", received, sent);
                    break;
                }
            }
        }
    }

    // Phase 3: Simulate playback - pop from buffer at 20ms intervals and measure jitter
    info!("ws: echo test phase 3 - measuring buffered jitter");
    let mut buffered_jitter_us: Vec<u32, ECHO_TEST_PACKET_COUNT> = Vec::new();
    let mut buffered_output: u8 = 0;
    let mut last_pop_time: Option<Instant> = None;
    let mut next_pop_time = Instant::now();
    let pop_interval = Duration::from_millis(20);

    // Pop all frames from buffer at steady 20ms rate
    loop {
        // Wait until next pop time
        let now = Instant::now();
        if now < next_pop_time {
            Timer::after(next_pop_time - now).await;
        }

        match jitter_buffer.pop() {
            Some(_frame) => {
                let pop_time = Instant::now();
                if let Some(last) = last_pop_time {
                    let delta_us = pop_time.duration_since(last).as_micros() as u32;
                    let _ = buffered_jitter_us.push(delta_us);
                }
                last_pop_time = Some(pop_time);
                buffered_output += 1;
                next_pop_time = Instant::now() + pop_interval;
            }
            None => {
                // Buffer empty or still buffering
                if jitter_buffer.level() == 0 && buffered_output > 0 {
                    // Buffer drained, we're done
                    break;
                }
                // Still buffering or underrun, wait a bit and retry
                Timer::after(Duration::from_millis(5)).await;
                // Safety: don't wait forever if nothing received
                if buffered_output == 0 && received == 0 {
                    break;
                }
            }
        }
    }

    let stats = jitter_buffer.stats();
    info!(
        "ws: echo test complete - raw received: {}, buffered output: {}, underruns: {}",
        received, buffered_output, stats.underruns
    );

    (
        EchoTestResult {
            sent,
            received,
            buffered_output,
            raw_jitter_us,
            buffered_jitter_us,
            underruns: stats.underruns as u8,
        },
        connection_ok,
    )
}

/// Parse a wss:// URL into (host, port, path)
fn parse_wss_url(url: &str) -> Option<(&str, u16, &str)> {
    let url = url
        .strip_prefix("wss://")
        .or_else(|| url.strip_prefix("ws://"))?;

    let (host_port, path) = match url.find('/') {
        Some(idx) => (&url[..idx], &url[idx..]),
        None => (url, "/"),
    };

    let (host, port) = match host_port.find(':') {
        Some(idx) => {
            let port: u16 = host_port[idx + 1..].parse().ok()?;
            (&host_port[..idx], port)
        }
        None => (host_port, 443),
    };

    Some((host, port, path))
}

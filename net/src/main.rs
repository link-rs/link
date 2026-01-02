#![no_std]
#![no_main]

extern crate alloc;

use defmt::{info, warn};
use edge_http::ws as http_ws;
use edge_ws::{FrameHeader, FrameType};
use embassy_executor::Spawner;
use embassy_futures::select::{select, Either};
use embassy_net::{tcp::TcpSocket, Runner, StackResources};
use embassy_sync::{blocking_mutex::raw::CriticalSectionRawMutex, channel::Channel};
use embassy_time::{Duration, Timer};
use embedded_io_async::Write as AsyncWrite;
use embedded_tls::{Aes128GcmSha256, NoVerify, TlsConfig, TlsConnection, TlsContext};
use esp_bootloader_esp_idf::partitions;
use esp_hal::{
    clock::CpuClock,
    gpio::{Level, Output, OutputConfig},
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
    EchoTestResult, NetStorage, SpeedTestResult, WsCommand, WsEvent, ECHO_TEST_PACKET_COUNT,
    MAX_RELAY_URL_LEN,
};
use rand_core::{CryptoRng, RngCore};
use static_cell::StaticCell;

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    defmt::error!("panic: {:?}", info);
    loop {}
}

esp_bootloader_esp_idf::esp_app_desc!();

/// Convert centralized UART config to ESP32 HAL config.
fn uart_config_to_esp32(cfg: link::uart_config::Config, flow_ctl: &HwFlowControl) -> Config {
    use link::uart_config::{Parity as P, StopBits as S};
    Config::default()
        .with_baudrate(cfg.baudrate)
        .with_parity(match cfg.parity {
            P::None => Parity::None,
            P::Even => Parity::Even,
        })
        .with_stop_bits(match cfg.stop_bits {
            S::One => StopBits::_1,
            S::Two => StopBits::_2,
        })
        .with_rx(RxConfig::default().with_fifo_full_threshold(1))
        .with_sw_flow_ctrl(SwFlowControl::Disabled)
        .with_hw_flow_ctrl(flow_ctl.clone())
}

macro_rules! singleton {
    ($t:ty, $val:expr) => {{
        static STATIC_CELL: StaticCell<$t> = StaticCell::new();
        STATIC_CELL.uninit().write($val)
    }};
}

/// Channel for sending commands to the WebSocket task.
/// Sized for ~320ms of audio at 50fps (16 frames)
static WS_CMD_CHANNEL: StaticCell<Channel<CriticalSectionRawMutex, WsCommand, 16>> =
    StaticCell::new();

/// Channel for receiving events from the WebSocket task.
/// Sized for ~320ms of audio at 50fps (16 frames)
static WS_EVENT_CHANNEL: StaticCell<Channel<CriticalSectionRawMutex, WsEvent, 16>> =
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

    // UART configs from centralized definitions
    let mgmt_config = uart_config_to_esp32(link::uart_config::MGMT_NET, &flow_ctl_disabled);
    let mgmt_uart = Uart::new(peripherals.UART0, mgmt_config)
        .unwrap()
        .with_tx(peripherals.GPIO43)
        .with_rx(peripherals.GPIO44)
        .into_async();
    let (from_mgmt, to_mgmt) = mgmt_uart.split();

    let ui_config = uart_config_to_esp32(link::uart_config::UI_NET, &flow_ctl_disabled);
    let ui_uart = Uart::new(peripherals.UART1, ui_config)
        .unwrap()
        .with_tx(peripherals.GPIO17)
        .with_rx(peripherals.GPIO18)
        .into_async();
    let (from_ui, to_ui) = ui_uart.split();

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
    let radio = singleton!(esp_radio::Controller<'static>, radio);
    let (controller, interfaces) =
        esp_radio::wifi::new(radio, peripherals.WIFI, WifiConfig::default()).expect("wifi init");

    // Initialize network stack
    let mut rng = EspRng(Rng::new());
    let seed = rng.next_u64();
    let (stack, runner) = embassy_net::new(
        interfaces.sta,
        embassy_net::Config::dhcpv4(Default::default()),
        singleton!(StackResources<3>, StackResources::<3>::new()),
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

    // Run the main event loop
    link::net::run(
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
                        best_match = Some((
                            wifi.ssid.as_str(),
                            wifi.password.as_str(),
                            network.signal_strength,
                        ));
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
    cmd_rx: embassy_sync::channel::Receiver<'static, CriticalSectionRawMutex, WsCommand, 16>,
    event_tx: embassy_sync::channel::Sender<'static, CriticalSectionRawMutex, WsEvent, 16>,
    mut rng: EspRng,
) {
    info!("ws: task started, waiting for URL");

    // Current relay URL (empty until we receive a Connect command)
    let mut current_url: heapless::String<MAX_RELAY_URL_LEN> = heapless::String::new();
    let mut wifi_was_connected = false;

    loop {
        // Wait for WiFi to be ready
        while !stack.is_link_up() {
            // Send WiFi disconnected event if we were previously connected
            if wifi_was_connected {
                wifi_was_connected = false;
                event_tx.send(WsEvent::WifiDisconnected).await;
            }
            // Check for commands while waiting
            if let Ok(cmd) = cmd_rx.try_receive() {
                match cmd {
                    WsCommand::Connect(url) => {
                        info!("ws: received URL while waiting for WiFi");
                        current_url = url;
                    }
                    WsCommand::Send(_) => {} // Drop audio before connected
                    WsCommand::EchoTest => {
                        info!("ws: ignoring echo test while disconnected");
                        event_tx.send(WsEvent::EchoTestResult(None)).await;
                    }
                    WsCommand::SpeedTest => {
                        info!("ws: ignoring speed test while disconnected");
                        event_tx.send(WsEvent::SpeedTestResult(None)).await;
                    }
                }
            }
            Timer::after(Duration::from_millis(100)).await;
        }

        // Wait for DHCP
        while stack.config_v4().is_none() {
            Timer::after(Duration::from_millis(100)).await;
        }

        // Send WiFi connected event
        if !wifi_was_connected {
            wifi_was_connected = true;
            event_tx.send(WsEvent::WifiConnected).await;
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
                    WsCommand::Send(_) => {} // Drop audio before connected
                    WsCommand::EchoTest => {
                        info!("ws: ignoring echo test without URL");
                        event_tx.send(WsEvent::EchoTestResult(None)).await;
                    }
                    WsCommand::SpeedTest => {
                        info!("ws: ignoring speed test without URL");
                        event_tx.send(WsEvent::SpeedTestResult(None)).await;
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

        // WebSocket handshake over TLS
        // Note: edge_http::io::client::Connection requires TcpConnect and manages its own
        // TCP connection. Since we already have a TLS stream, we use the lower-level
        // http_ws helpers (upgrade_request_headers, is_upgrade_accepted) and format the
        // HTTP request ourselves.
        let mut ws_read_buf = [0u8; 2048];

        // Generate nonce and build upgrade request
        let mut nonce = [0u8; http_ws::NONCE_LEN];
        rng.fill_bytes(&mut nonce);

        let mut request_buf = [0u8; 512];
        let request_len =
            build_ws_upgrade_request(path.as_str(), host.as_str(), &nonce, &mut request_buf);

        if tls.write_all(&request_buf[..request_len]).await.is_err() {
            warn!("ws: failed to send upgrade request");
            event_tx.send(WsEvent::Disconnected).await;
            Timer::after(Duration::from_secs(5)).await;
            continue;
        }
        if tls.flush().await.is_err() {
            warn!("ws: failed to flush upgrade request");
            event_tx.send(WsEvent::Disconnected).await;
            Timer::after(Duration::from_secs(5)).await;
            continue;
        }
        info!("ws: sent upgrade request ({} bytes)", request_len);

        // Read HTTP response
        let mut response_buf = [0u8; 1024];
        let mut response_len = 0;
        loop {
            if response_len >= response_buf.len() {
                warn!("ws: response too large");
                event_tx.send(WsEvent::Disconnected).await;
                Timer::after(Duration::from_secs(5)).await;
                break;
            }
            match tls
                .read(&mut response_buf[response_len..response_len + 1])
                .await
            {
                Ok(0) => {
                    warn!("ws: connection closed during handshake");
                    event_tx.send(WsEvent::Disconnected).await;
                    Timer::after(Duration::from_secs(5)).await;
                    break;
                }
                Ok(_) => {
                    response_len += 1;
                    // Check for end of headers (CRLFCRLF)
                    if response_len >= 4
                        && &response_buf[response_len - 4..response_len] == b"\r\n\r\n"
                    {
                        break;
                    }
                }
                Err(_) => {
                    warn!("ws: failed to read response");
                    event_tx.send(WsEvent::Disconnected).await;
                    Timer::after(Duration::from_secs(5)).await;
                    break;
                }
            }
        }
        // Check if we broke out due to error
        if response_len < 4 || &response_buf[response_len - 4..response_len] != b"\r\n\r\n" {
            continue;
        }
        info!("ws: received response ({} bytes)", response_len);

        // Parse HTTP response - extract status code and headers
        let response_str = match core::str::from_utf8(&response_buf[..response_len]) {
            Ok(s) => s,
            Err(_) => {
                warn!("ws: invalid UTF-8 in response");
                event_tx.send(WsEvent::Disconnected).await;
                Timer::after(Duration::from_secs(5)).await;
                continue;
            }
        };
        info!("ws: response: {}", response_str);

        // Parse status line: "HTTP/1.1 101 Switching Protocols\r\n"
        let mut lines = response_str.lines();
        let status_line = match lines.next() {
            Some(line) => line,
            None => {
                warn!("ws: empty response");
                event_tx.send(WsEvent::Disconnected).await;
                Timer::after(Duration::from_secs(5)).await;
                continue;
            }
        };
        let status_code: u16 = {
            let parts: heapless::Vec<&str, 3> = status_line.splitn(3, ' ').collect();
            if parts.len() < 2 {
                warn!("ws: invalid status line");
                event_tx.send(WsEvent::Disconnected).await;
                Timer::after(Duration::from_secs(5)).await;
                continue;
            }
            match parts[1].parse() {
                Ok(code) => code,
                Err(_) => {
                    warn!("ws: invalid status code");
                    event_tx.send(WsEvent::Disconnected).await;
                    Timer::after(Duration::from_secs(5)).await;
                    continue;
                }
            }
        };

        // Parse headers into a vec of (name, value) tuples
        let mut response_headers: heapless::Vec<(&str, &str), 16> = heapless::Vec::new();
        for line in lines {
            if line.is_empty() {
                break;
            }
            if let Some(colon_idx) = line.find(':') {
                let name = line[..colon_idx].trim();
                let value = line[colon_idx + 1..].trim();
                let _ = response_headers.push((name, value));
            }
        }

        // Validate WebSocket upgrade
        let mut accept_buf = [0u8; http_ws::MAX_BASE64_KEY_RESPONSE_LEN];
        if !http_ws::is_upgrade_accepted(
            status_code,
            response_headers.iter().copied(),
            &nonce,
            &mut accept_buf,
        ) {
            warn!(
                "ws: WebSocket upgrade not accepted (status={})",
                status_code
            );
            event_tx.send(WsEvent::Disconnected).await;
            Timer::after(Duration::from_secs(5)).await;
            continue;
        }

        info!("ws: connected");
        event_tx.send(WsEvent::Connected).await;

        // Main WebSocket loop using select for concurrent read/write
        let mut should_reconnect = false;
        let mut connection_broken = false;

        // CANCELLATION SAFETY: We use FrameHeader::recv() in the select instead of
        // edge_ws::io::recv(). This is important because io::recv() has two await points
        // (header then payload), and if cancelled between them, the stream corrupts.
        // By reading only the header (2-14 bytes) in the select, and reading the payload
        // afterward, we minimize the cancellation window to just the small header read.

        loop {
            match select(FrameHeader::recv(&mut tls), cmd_rx.receive()).await {
                Either::First(header_result) => {
                    // Header received - now read payload outside of select (cannot be cancelled)
                    let header = match header_result {
                        Ok(h) => h,
                        Err(e) => {
                            warn!("ws: header read error: {:?}", e);
                            connection_broken = true;
                            break;
                        }
                    };

                    // Read payload - this is outside the select, so it cannot be cancelled
                    let payload = match header.recv_payload(&mut tls, &mut ws_read_buf).await {
                        Ok(p) => p,
                        Err(e) => {
                            warn!("ws: payload read error: {:?}", e);
                            connection_broken = true;
                            break;
                        }
                    };
                    let len = payload.len();

                    // Handle frame based on type
                    match header.frame_type {
                        FrameType::Binary(_fragmented) => {
                            info!("ws: received {} bytes", len);
                            if let Ok(data) = Vec::try_from(&ws_read_buf[..len]) {
                                event_tx.send(WsEvent::Received(data)).await;
                            }
                        }
                        FrameType::Text(_fragmented) => {
                            if let Ok(text) = core::str::from_utf8(&ws_read_buf[..len]) {
                                info!("ws: received text: {}", text);
                            }
                            if let Ok(data) = Vec::try_from(&ws_read_buf[..len]) {
                                event_tx.send(WsEvent::Received(data)).await;
                            }
                        }
                        FrameType::Close => {
                            // Parse close code if present
                            let code = if len >= 2 {
                                u16::from_be_bytes([ws_read_buf[0], ws_read_buf[1]])
                            } else {
                                1000
                            };
                            if len > 2 {
                                if let Ok(reason) = core::str::from_utf8(&ws_read_buf[2..len]) {
                                    warn!(
                                        "ws: received close frame: code={}, reason={}",
                                        code, reason
                                    );
                                } else {
                                    warn!("ws: received close frame: code={}", code);
                                }
                            } else {
                                warn!("ws: received close frame: code={}", code);
                            }
                            // Send close frame back
                            let _ = edge_ws::io::send(
                                &mut tls,
                                FrameType::Close,
                                Some(rng.next_u32()),
                                &ws_read_buf[..len],
                            )
                            .await;
                            let _ = tls.flush().await;
                            break;
                        }
                        FrameType::Ping => {
                            // Respond with Pong containing the same payload
                            if edge_ws::io::send(
                                &mut tls,
                                FrameType::Pong,
                                Some(rng.next_u32()),
                                &ws_read_buf[..len],
                            )
                            .await
                            .is_err()
                            {
                                warn!("ws: failed to send pong");
                                connection_broken = true;
                                break;
                            }
                            let _ = tls.flush().await;
                        }
                        FrameType::Pong => {
                            // Ignore pong frames
                        }
                        FrameType::Continue(_) => {
                            // Continuation frames - we don't handle fragmentation currently
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
                            match edge_ws::io::send(
                                &mut tls,
                                FrameType::Binary(false),
                                Some(rng.next_u32()),
                                &data,
                            )
                            .await
                            {
                                Ok(_) => {
                                    if tls.flush().await.is_err() {
                                        warn!("ws: flush failed");
                                        connection_broken = true;
                                        break;
                                    }
                                }
                                Err(e) => {
                                    warn!("ws: write failed: {:?}", e);
                                    connection_broken = true;
                                    break;
                                }
                            }
                        }
                        WsCommand::EchoTest => {
                            info!("ws: starting echo test");
                            let (result, connection_ok) =
                                run_echo_test(&mut tls, &mut ws_read_buf, &mut rng).await;
                            info!(
                                "ws: echo test complete: sent={}, received={}",
                                result.sent, result.received
                            );
                            event_tx.send(WsEvent::EchoTestResult(Some(result))).await;
                            if !connection_ok {
                                warn!("ws: connection failed during echo test, reconnecting");
                                connection_broken = true;
                                break;
                            }
                        }
                        WsCommand::SpeedTest => {
                            info!("ws: starting speed test");
                            let (result, connection_ok) =
                                run_speed_test(&mut tls, &mut ws_read_buf, &mut rng).await;
                            info!(
                                "ws: speed test complete: sent={}, received={}, send_time={}ms, recv_time={}ms",
                                result.sent, result.received, result.send_time_ms, result.recv_time_ms
                            );
                            event_tx.send(WsEvent::SpeedTestResult(Some(result))).await;
                            if !connection_ok {
                                warn!("ws: connection failed during speed test, reconnecting");
                                connection_broken = true;
                                break;
                            }
                        }
                    }
                }
            }
        }

        // Clean up - only try to close if connection is still healthy
        if !connection_broken {
            // Send close frame with normal closure code (1000)
            let close_payload = 1000u16.to_be_bytes();
            let _ = edge_ws::io::send(
                &mut tls,
                FrameType::Close,
                Some(rng.next_u32()),
                &close_payload,
            )
            .await;
        }

        if !should_reconnect {
            event_tx.send(WsEvent::Disconnected).await;
            Timer::after(Duration::from_secs(5)).await;
        }
    }
}

// NOTE: Both echo test and speed test serve different purposes:
// - Echo test: Sends at 20ms intervals (like audio), measures jitter through jitter buffer
// - Speed test: Sends as fast as possible, measures raw throughput
// Both are useful for diagnosing different types of network issues.

/// Run the WebSocket echo test with jitter buffer analysis.
///
/// Sends ECHO_TEST_PACKET_COUNT packets (640 bytes each) at 20ms intervals (50 fps),
/// receives echoed packets through a jitter buffer, and records:
/// - Raw jitter: inter-arrival times before the buffer
/// - Buffered jitter: inter-departure times after the buffer
///
/// Returns (result, connection_ok). If connection_ok is false, the caller
/// should break out of the main loop to trigger reconnection.
async fn run_echo_test<S>(
    tls: &mut TlsConnection<'_, S, Aes128GcmSha256>,
    ws_read_buf: &mut [u8],
    rng: &mut EspRng,
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

    info!(
        "ws: echo test phase 1 - sending {} packets",
        ECHO_TEST_PACKET_COUNT
    );

    while (sent as usize) < ECHO_TEST_PACKET_COUNT {
        // Time to send next packet?
        if Instant::now() >= next_send_time {
            match edge_ws::io::send(
                &mut *tls,
                FrameType::Binary(false),
                Some(rng.next_u32()),
                &packet,
            )
            .await
            {
                Ok(_) => {
                    if tls.flush().await.is_err() {
                        warn!("ws: echo test flush failed at packet {}", sent);
                        connection_ok = false;
                        break;
                    }
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
            match embassy_time::with_timeout(timeout, edge_ws::io::recv(&mut *tls, ws_read_buf))
                .await
            {
                Ok(Ok((FrameType::Binary(_), len))) => {
                    let recv_time = Instant::now();
                    // Record raw jitter (before buffer)
                    if let Some(last) = last_recv_time {
                        let delta_us = recv_time.duration_since(last).as_micros() as u32;
                        let _ = raw_jitter_us.push(delta_us);
                    }
                    last_recv_time = Some(recv_time);
                    received += 1;
                    // Push into jitter buffer
                    jitter_buffer.push(&ws_read_buf[..len]);
                }
                Ok(Ok(_)) => {
                    // Other frame types, ignore but keep draining
                }
                Ok(Err(_)) => {
                    warn!("ws: echo test receive error");
                    connection_ok = false;
                    break;
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
            match embassy_time::with_timeout(timeout, edge_ws::io::recv(&mut *tls, ws_read_buf))
                .await
            {
                Ok(Ok((FrameType::Binary(_), len))) => {
                    let recv_time = Instant::now();
                    // Record raw jitter (before buffer)
                    if let Some(last) = last_recv_time {
                        let delta_us = recv_time.duration_since(last).as_micros() as u32;
                        let _ = raw_jitter_us.push(delta_us);
                    }
                    last_recv_time = Some(recv_time);
                    received += 1;
                    // Push into jitter buffer
                    jitter_buffer.push(&ws_read_buf[..len]);
                }
                Ok(Ok(_)) => {
                    // Other frame types, ignore but keep waiting
                }
                Ok(Err(_)) => {
                    warn!("ws: echo test phase 2 receive error");
                    connection_ok = false;
                    break;
                }
                Err(_) => {
                    // Timeout expired
                    info!(
                        "ws: echo test phase 2 timeout, received {}/{}",
                        received, sent
                    );
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

/// Run the WebSocket speed test.
///
/// Sends 50 packets as fast as possible (no delays), then waits up to 2 seconds
/// to receive responses. Returns timing information.
async fn run_speed_test<S>(
    tls: &mut TlsConnection<'_, S, Aes128GcmSha256>,
    ws_read_buf: &mut [u8],
    rng: &mut EspRng,
) -> (SpeedTestResult, bool)
where
    S: embedded_io_async::Read + embedded_io_async::Write + Unpin,
{
    use embassy_time::Instant;

    const PACKET_COUNT: usize = 50;
    const PACKET_SIZE: usize = 640;
    let packet = [0xBBu8; PACKET_SIZE];

    let mut sent: u8 = 0;
    let mut received: u8 = 0;
    let mut connection_ok = true;

    // Phase 1: Send all packets as fast as possible
    info!("ws: speed test - sending {} packets", PACKET_COUNT);
    let send_start = Instant::now();

    for i in 0..PACKET_COUNT {
        match edge_ws::io::send(
            &mut *tls,
            FrameType::Binary(false),
            Some(rng.next_u32()),
            &packet,
        )
        .await
        {
            Ok(_) => {
                if tls.flush().await.is_err() {
                    warn!("ws: speed test flush failed at packet {}", i);
                    connection_ok = false;
                    break;
                }
                sent += 1;
                if (i + 1) % 10 == 0 {
                    info!("ws: speed test sent {} packets", i + 1);
                }
            }
            Err(_) => {
                warn!("ws: speed test send failed at packet {}", i);
                connection_ok = false;
                break;
            }
        }
    }

    let send_time = Instant::now().duration_since(send_start);
    let send_time_ms = send_time.as_millis() as u32;
    info!("ws: speed test sent {} packets in {}ms", sent, send_time_ms);

    // Phase 2: Receive responses (up to 2 seconds or all packets received)
    let recv_start = Instant::now();
    let deadline = recv_start + Duration::from_secs(2);

    if connection_ok {
        info!("ws: speed test - waiting for responses");
        while (received as usize) < (sent as usize) && Instant::now() < deadline {
            let timeout = deadline.saturating_duration_since(Instant::now());
            match embassy_time::with_timeout(timeout, edge_ws::io::recv(&mut *tls, ws_read_buf))
                .await
            {
                Ok(Ok((FrameType::Binary(_), _len))) => {
                    received += 1;
                    if received % 10 == 0 {
                        info!("ws: speed test received {} packets", received);
                    }
                }
                Ok(Ok(_)) => {
                    // Other frame types, ignore
                }
                Ok(Err(_)) => {
                    warn!("ws: speed test receive error");
                    connection_ok = false;
                    break;
                }
                Err(_) => {
                    // Timeout expired
                    info!("ws: speed test timeout, received {}/{}", received, sent);
                    break;
                }
            }
        }
    }

    let recv_time = Instant::now().duration_since(recv_start);
    let recv_time_ms = recv_time.as_millis() as u32;
    info!(
        "ws: speed test received {} packets in {}ms",
        received, recv_time_ms
    );

    (
        SpeedTestResult {
            sent,
            received,
            send_time_ms,
            recv_time_ms,
        },
        connection_ok,
    )
}

/// Parse a wss:// or ws:// URL into (host, port, path)
fn parse_wss_url(url: &str) -> Option<(&str, u16, &str)> {
    let parsed = url_lite::Url::parse(url).ok()?;

    // Verify scheme is ws or wss
    let schema = parsed.schema?;
    if schema != "wss" && schema != "ws" {
        return None;
    }

    let host = parsed.host?;
    let port = parsed
        .port
        .and_then(|p| p.parse().ok())
        .unwrap_or(if schema == "wss" { 443 } else { 80 });
    let path = parsed.path.unwrap_or("/");

    Some((host, port, path))
}

/// Build a WebSocket upgrade HTTP request.
///
/// Formats the HTTP/1.1 upgrade request with headers from edge_http::ws.
/// Returns the number of bytes written to the buffer.
fn build_ws_upgrade_request(
    path: &str,
    host: &str,
    nonce: &[u8; http_ws::NONCE_LEN],
    buf: &mut [u8],
) -> usize {
    let mut key_buf = [0u8; http_ws::MAX_BASE64_KEY_LEN];
    let headers = http_ws::upgrade_request_headers(
        Some(host),
        Some(host),
        None, // Use default WebSocket version "13"
        nonce,
        &mut key_buf,
    );

    let mut len = 0;

    // Request line: "GET {path} HTTP/1.1\r\n"
    let parts: &[&[u8]] = &[b"GET ", path.as_bytes(), b" HTTP/1.1\r\n"];
    for part in parts {
        buf[len..len + part.len()].copy_from_slice(part);
        len += part.len();
    }

    // Headers: "{name}: {value}\r\n"
    for (name, value) in &headers {
        if !name.is_empty() {
            buf[len..len + name.len()].copy_from_slice(name.as_bytes());
            len += name.len();
            buf[len..len + 2].copy_from_slice(b": ");
            len += 2;
            buf[len..len + value.len()].copy_from_slice(value.as_bytes());
            len += value.len();
            buf[len..len + 2].copy_from_slice(b"\r\n");
            len += 2;
        }
    }

    // Final CRLF
    buf[len..len + 2].copy_from_slice(b"\r\n");
    len += 2;

    len
}

//! NET chip WiFi + WebSocket prototype

#![no_std]
#![no_main]

extern crate alloc;

use defmt::{info, warn};
use embassy_executor::Spawner;
use embassy_net::{tcp::TcpSocket, Runner, StackResources};
use embassy_time::{Duration, Timer};
use embedded_tls::{Aes128GcmSha256, NoVerify, TlsConfig, TlsConnection, TlsContext};
use embedded_websocket_embedded_io::framer_async::ReadResult;
use embedded_websocket_embedded_io::{
    framer_async::Framer, WebSocketClient, WebSocketCloseStatusCode, WebSocketOptions,
    WebSocketSendMessageType,
};
use esp_hal::{
    clock::CpuClock,
    gpio::{Level, Output, OutputConfig},
    rng::Rng,
    timer::timg::TimerGroup,
};
use esp_radio::wifi::{
    ClientConfig, Config as WifiConfig, ModeConfig, WifiController, WifiDevice, WifiEvent,
    WifiStaState,
};
use link::shared::{Color, Led};
use rand_core::{CryptoRng, RngCore};
use static_cell::StaticCell;

const SSID: &str = "Verizon_FTN33P";
const PASSWORD: &str = "dyer3-hasp-aye";
const WS_HOST: &str = "echo.websocket.org";
const WS_PORT: u16 = 443;

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

#[esp_rtos::main]
async fn main(spawner: Spawner) {
    rtt_target::rtt_init_defmt!();
    info!("NET chip starting");

    // Initialize hardware
    let config = esp_hal::Config::default().with_cpu_clock(CpuClock::max());
    let peripherals = esp_hal::init(config);
    esp_alloc::heap_allocator!(size: 72 * 1024);

    let timg0 = TimerGroup::new(peripherals.TIMG0);
    esp_rtos::start(timg0.timer0);

    // LED: Red = no IP, Yellow = TLS, Green = WS connected, Blue = echo received
    let mut led = Led::new(
        Output::new(peripherals.GPIO38, Level::High, OutputConfig::default()),
        Output::new(peripherals.GPIO37, Level::High, OutputConfig::default()),
        Output::new(peripherals.GPIO36, Level::High, OutputConfig::default()),
    );
    led.set(Color::Red);

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

    spawner.spawn(wifi_task(controller)).ok();
    spawner.spawn(net_task(runner)).ok();

    // Wait for IP address
    info!("Waiting for WiFi...");
    while !stack.is_link_up() {
        Timer::after(Duration::from_millis(100)).await;
    }
    info!("WiFi link up, waiting for DHCP...");
    while stack.config_v4().is_none() {
        Timer::after(Duration::from_millis(100)).await;
    }
    let config = stack.config_v4().unwrap();
    info!("Got IP: {:?}", config.address);

    // WebSocket test loop
    loop {
        info!("Connecting to {}:{}...", WS_HOST, WS_PORT);

        // DNS lookup
        let addr = match stack
            .dns_query(WS_HOST, embassy_net::dns::DnsQueryType::A)
            .await
        {
            Ok(addrs) if !addrs.is_empty() => addrs[0],
            _ => {
                warn!("DNS failed");
                Timer::after(Duration::from_secs(5)).await;
                continue;
            }
        };

        // TCP connect
        let mut rx_buf = [0u8; 4096];
        let mut tx_buf = [0u8; 4096];
        let mut socket = TcpSocket::new(stack, &mut rx_buf, &mut tx_buf);
        socket.set_timeout(Some(Duration::from_secs(30)));

        if socket.connect((addr, WS_PORT)).await.is_err() {
            warn!("TCP connect failed");
            Timer::after(Duration::from_secs(5)).await;
            continue;
        }
        info!("TCP connected");

        // TLS handshake
        let mut tls_read = [0u8; 16640];
        let mut tls_write = [0u8; 16640];
        let tls_config = TlsConfig::new().with_server_name(WS_HOST);
        let mut tls: TlsConnection<_, Aes128GcmSha256> =
            TlsConnection::new(socket, &mut tls_read, &mut tls_write);

        if tls
            .open::<_, NoVerify>(TlsContext::new(&tls_config, &mut rng))
            .await
            .is_err()
        {
            warn!("TLS handshake failed");
            Timer::after(Duration::from_secs(5)).await;
            continue;
        }
        led.set(Color::Yellow);
        info!("TLS connected");

        // WebSocket handshake
        let mut ws_buf = [0u8; 1024];
        let websocket = WebSocketClient::new_client(&mut rng);
        let mut framer = Framer::new(websocket);
        let options = WebSocketOptions {
            path: "/",
            host: WS_HOST,
            origin: WS_HOST,
            sub_protocols: None,
            additional_headers: None,
        };
        if framer
            .connect(&mut tls, &mut ws_buf, &options)
            .await
            .is_err()
        {
            warn!("WebSocket connect failed");
            Timer::after(Duration::from_secs(5)).await;
            continue;
        }
        led.set(Color::Green);
        info!("WebSocket connected");

        // Send test message
        if framer
            .write(
                &mut tls,
                &mut ws_buf,
                WebSocketSendMessageType::Text,
                true,
                b"Hello from Hactar!",
            )
            .await
            .is_err()
        {
            warn!("WebSocket write failed");
            Timer::after(Duration::from_secs(5)).await;
            continue;
        }

        // Read echo response
        if let Some(Ok(ReadResult::Text(text))) = framer.read(&mut tls, &mut ws_buf).await {
            info!("Echo: '{}'", text);
            led.set(Color::Blue);
        }

        // Close connection
        let _ = framer
            .close(
                &mut tls,
                &mut ws_buf,
                WebSocketCloseStatusCode::NormalClosure,
                None,
            )
            .await;

        info!("Waiting 10s...");
        Timer::after(Duration::from_secs(10)).await;
    }
}

// ESP32 hardware RNG wrapper
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
async fn wifi_task(mut controller: WifiController<'static>) {
    info!("WiFi task started");

    loop {
        if esp_radio::wifi::sta_state() == WifiStaState::Connected {
            controller.wait_for_event(WifiEvent::StaDisconnected).await;
            warn!("WiFi disconnected");
            Timer::after(Duration::from_secs(5)).await;
        }

        if !matches!(controller.is_started(), Ok(true)) {
            let config = ModeConfig::Client(
                ClientConfig::default()
                    .with_ssid(SSID.try_into().unwrap())
                    .with_password(PASSWORD.try_into().unwrap()),
            );
            controller.set_config(&config).unwrap();
            controller.start_async().await.unwrap();
        }

        match controller.connect_async().await {
            Ok(_) => info!("WiFi connected to '{}'", SSID),
            Err(_) => {
                warn!("WiFi connect failed");
                Timer::after(Duration::from_secs(5)).await;
            }
        }
    }
}

#[embassy_executor::task]
async fn net_task(mut runner: Runner<'static, WifiDevice<'static>>) {
    runner.run().await
}

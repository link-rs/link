//! NET chip firmware using ESP-IDF.
//!
//! This is the ESP-IDF version of the NET chip firmware, providing:
//! - WiFi connectivity with stored credentials
//! - UART communication with MGMT and UI chips
//! - LED status indication
//! - NVS storage for WiFi credentials and relay URL
//! - Audio loopback mode
//!
//! Note: WebSocket functionality is not yet implemented in this version.

use esp_idf_svc::{
    eventloop::EspSystemEventLoop,
    hal::{
        gpio::{OutputPin, PinDriver},
        prelude::Peripherals,
        uart::{self, UartDriver},
    },
    nvs::{EspDefaultNvsPartition, EspNvs, NvsDefault},
};
use heapless::String;
use link::{
    net::{WifiSsid, MAX_RELAY_URL_LEN, MAX_WIFI_SSIDS},
    uart_config, Color, MgmtToNet, NetToMgmt, NetToUi, UiToNet,
    HEADER_SIZE, MAX_VALUE_SIZE, SYNC_WORD,
};
use log::{info, warn};
use std::thread;
use std::time::Duration;

/// NVS namespace for NET storage.
const NVS_NAMESPACE: &str = "net";

fn main() {
    // Initialize ESP-IDF
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
    // MGMT UART: UART0, GPIO43 (TX), GPIO44 (RX)
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
        peripherals.pins.gpio43, // TX
        peripherals.pins.gpio44, // RX
        Option::<GpioStub>::None,
        Option::<GpioStub>::None,
        &mgmt_uart_config,
    )
    .unwrap();

    // UI UART: UART1, GPIO17 (TX), GPIO18 (RX)
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
        peripherals.pins.gpio17, // TX
        peripherals.pins.gpio18, // RX
        Option::<GpioStub>::None,
        Option::<GpioStub>::None,
        &ui_uart_config,
    )
    .unwrap();

    info!("net-idf: UARTs initialized");

    // Initialize WiFi
    let wifi =
        esp_idf_svc::wifi::EspWifi::new(peripherals.modem, sys_loop.clone(), Some(nvs_partition.clone())).unwrap();
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

    // Loopback mode state
    let mut loopback = false;

    // Main event loop
    info!("net-idf: starting main loop");

    // For now, try to connect to WiFi if we have credentials
    if !storage.wifi_ssids.is_empty() {
        let wifi_ssid = &storage.wifi_ssids[0];
        info!("net-idf: connecting to WiFi '{}'", wifi_ssid.ssid);

        if let Err(e) = connect_wifi(&mut wifi, &wifi_ssid.ssid, &wifi_ssid.password) {
            warn!("net-idf: WiFi connect failed: {:?}", e);
        } else {
            info!("net-idf: WiFi connected");
            set_led_color(&mut led_r, &mut led_g, &mut led_b, Color::Yellow);
        }
    }

    // Simple TLV handling loop
    // Buffer size: sync word (4) + header (6) + max value
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
                );
            }
        }

        // Check UI UART for incoming data
        if let Some((msg_type, value)) = try_read_tlv(&ui_uart, &mut ui_rx_buf, &mut ui_rx_pos) {
            if let Ok(tlv_type) = UiToNet::try_from(msg_type) {
                handle_ui_message(tlv_type, &value, &mgmt_uart, &ui_uart, loopback);
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
    use esp_idf_svc::wifi::{AuthMethod, ClientConfiguration, Configuration};

    let config = Configuration::Client(ClientConfiguration {
        ssid: ssid.try_into().unwrap(),
        password: password.try_into().unwrap(),
        auth_method: if password.is_empty() {
            AuthMethod::None
        } else {
            AuthMethod::WPA2Personal
        },
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
/// Protocol: 4-byte sync "LINK" + 2-byte type (BE) + 4-byte length (BE) + value
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

    // Try to parse TLV - need at least sync word + header
    if *pos >= FRAME_HEADER_SIZE {
        // Check for sync word "LINK"
        if buf[0..4] != SYNC_WORD {
            // Sync error - try to find sync word in buffer
            if let Some(idx) = buf[1..*pos].windows(4).position(|w| w == SYNC_WORD) {
                let new_start = idx + 1;
                buf.copy_within(new_start..*pos, 0);
                *pos -= new_start;
            } else {
                // No sync word found, keep last 3 bytes (might be partial sync)
                if *pos > 3 {
                    buf.copy_within(*pos - 3..*pos, 0);
                    *pos = 3;
                }
            }
            return None;
        }

        // Parse header (after sync word): type u16 BE + length u32 BE
        let msg_type = u16::from_be_bytes([buf[4], buf[5]]);
        let length = u32::from_be_bytes([buf[6], buf[7], buf[8], buf[9]]) as usize;

        if length > MAX_VALUE_SIZE {
            // Invalid length - skip this sync word and try again
            buf.copy_within(4..*pos, 0);
            *pos -= 4;
            return None;
        }

        let total_len = FRAME_HEADER_SIZE + length;
        if *pos >= total_len {
            // Complete message
            let mut value = heapless::Vec::new();
            value
                .extend_from_slice(&buf[FRAME_HEADER_SIZE..total_len])
                .ok();

            // Shift remaining data
            buf.copy_within(total_len..*pos, 0);
            *pos -= total_len;

            return Some((msg_type, value));
        }
    }

    None
}

/// Write a TLV message to UART
/// Protocol: 4-byte sync "LINK" + 2-byte type (BE) + 4-byte length (BE) + value
fn write_tlv<T: Into<u16>>(uart: &UartDriver, msg_type: T, value: &[u8]) {
    let msg_type: u16 = msg_type.into();

    // Write sync word
    uart.write(&SYNC_WORD).ok();

    // Write header: type (2 bytes BE) + length (4 bytes BE)
    let mut header = [0u8; HEADER_SIZE];
    header[0..2].copy_from_slice(&msg_type.to_be_bytes());
    header[2..6].copy_from_slice(&(value.len() as u32).to_be_bytes());
    uart.write(&header).ok();

    // Write value
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
) {
    match msg_type {
        MgmtToNet::Ping => {
            info!("net-idf: MGMT ping, sending pong");
            write_tlv(mgmt_uart, NetToMgmt::Pong, value);
        }
        MgmtToNet::CircularPing => {
            info!("net-idf: MGMT circular ping -> UI");
            write_tlv(ui_uart, NetToUi::CircularPing, value);
        }
        MgmtToNet::AddWifiSsid => {
            info!("net-idf: add WiFi SSID");
            if let Ok(wifi) = postcard::from_bytes::<WifiSsid>(value) {
                if storage.add_wifi_ssid(&wifi.ssid, &wifi.password).is_ok() {
                    if let Err(e) = storage.save() {
                        warn!("net-idf: failed to save storage: {:?}", e);
                        write_tlv(mgmt_uart, NetToMgmt::Error, b"save");
                        return;
                    }
                    write_tlv(mgmt_uart, NetToMgmt::Ack, &[]);
                } else {
                    write_tlv(mgmt_uart, NetToMgmt::Error, b"add");
                }
            } else {
                write_tlv(mgmt_uart, NetToMgmt::Error, b"deserialize");
            }
        }
        MgmtToNet::GetWifiSsids => {
            info!("net-idf: get WiFi SSIDs");
            let mut buf = [0u8; 256];
            if let Ok(serialized) = postcard::to_slice(&storage.wifi_ssids, &mut buf) {
                write_tlv(mgmt_uart, NetToMgmt::WifiSsids, serialized);
            }
        }
        MgmtToNet::ClearWifiSsids => {
            info!("net-idf: clear WiFi SSIDs");
            storage.wifi_ssids.clear();
            if let Err(e) = storage.save() {
                warn!("net-idf: failed to save storage: {:?}", e);
                write_tlv(mgmt_uart, NetToMgmt::Error, b"save");
                return;
            }
            write_tlv(mgmt_uart, NetToMgmt::Ack, &[]);
        }
        MgmtToNet::GetRelayUrl => {
            info!("net-idf: get relay URL");
            write_tlv(mgmt_uart, NetToMgmt::RelayUrl, storage.relay_url.as_bytes());
        }
        MgmtToNet::SetRelayUrl => {
            info!("net-idf: set relay URL");
            if let Ok(url) = core::str::from_utf8(value) {
                if let Ok(url_string) = String::try_from(url) {
                    storage.relay_url = url_string;
                    if let Err(e) = storage.save() {
                        warn!("net-idf: failed to save storage: {:?}", e);
                        write_tlv(mgmt_uart, NetToMgmt::Error, b"save");
                        return;
                    }
                    write_tlv(mgmt_uart, NetToMgmt::Ack, &[]);
                } else {
                    write_tlv(mgmt_uart, NetToMgmt::Error, b"url");
                }
            } else {
                write_tlv(mgmt_uart, NetToMgmt::Error, b"utf8");
            }
        }
        MgmtToNet::WsSend => {
            info!("net-idf: WS send (not implemented)");
            // WebSocket not implemented - just acknowledge
        }
        MgmtToNet::WsEchoTest => {
            info!("net-idf: WS echo test (not implemented)");
            // Send empty result (not connected)
            write_tlv(mgmt_uart, NetToMgmt::WsEchoTestResult, &[]);
        }
        MgmtToNet::WsSpeedTest => {
            info!("net-idf: WS speed test (not implemented)");
            // Send empty result (not connected)
            write_tlv(mgmt_uart, NetToMgmt::WsSpeedTestResult, &[]);
        }
        MgmtToNet::SetLoopback => {
            let enabled = value.first().copied().unwrap_or(0) != 0;
            info!("net-idf: set loopback = {}", enabled);
            *loopback = enabled;
            write_tlv(mgmt_uart, NetToMgmt::Ack, &[]);
        }
        MgmtToNet::GetLoopback => {
            info!("net-idf: get loopback = {}", *loopback);
            write_tlv(mgmt_uart, NetToMgmt::Loopback, &[*loopback as u8]);
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
            info!("net-idf: UI circular ping -> MGMT");
            write_tlv(mgmt_uart, NetToMgmt::CircularPing, value);
        }
        UiToNet::AudioFrameA | UiToNet::AudioFrameB => {
            if loopback {
                // Loopback mode: echo audio back to UI
                write_tlv(ui_uart, NetToUi::AudioFrame, value);
            } else {
                // Would send to WebSocket if connected
                // For now, just drop the frame
            }
        }
    }
}

/// NVS-backed storage implementation
struct NvsStorage {
    nvs: Option<EspNvs<NvsDefault>>,
    wifi_ssids: heapless::Vec<WifiSsid, MAX_WIFI_SSIDS>,
    relay_url: String<MAX_RELAY_URL_LEN>,
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
                    if let Ok(ssids) = postcard::from_bytes::<heapless::Vec<WifiSsid, MAX_WIFI_SSIDS>>(data) {
                        storage.wifi_ssids = ssids;
                        info!("net-idf: loaded {} WiFi SSIDs from NVS", storage.wifi_ssids.len());
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
                        if let Ok(url_string) = String::try_from(url) {
                            storage.relay_url = url_string;
                            info!("net-idf: loaded relay URL from NVS: {}", storage.relay_url);
                        }
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
        let mut buf = [0u8; 512];
        if let Ok(serialized) = postcard::to_slice(&self.wifi_ssids, &mut buf) {
            nvs.set_blob(NVS_KEY_WIFI_SSIDS, serialized)?;
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
            ssid: String::try_from(ssid).map_err(|_| ())?,
            password: String::try_from(password).map_err(|_| ())?,
        };

        self.wifi_ssids.push(wifi).map_err(|_| ())?;
        Ok(())
    }
}

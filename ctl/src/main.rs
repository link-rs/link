//! CTL - Control interface for the link device
//!
//! CLI mode:  ctl ui ping hello
//! REPL mode: ctl (no args) -> interactive prompt

mod handlers;

use clap::{FromArgMatches, Parser, Subcommand};
use futures::executor::block_on;
use link::ctl::{BufferedPort, CtlCore, SyncPortAdapter};
use link::ctl::flash::{FlashPhase, FlashError, MgmtBootloaderInfo, EspflashError, EspflashDeviceInfo};
use link::ctl::ProgressCallbacks;
use rand::Rng;
use reedline_repl_rs::clap::ArgMatches;
use reedline_repl_rs::{CallBackMap, Repl};
use serialport::SerialPortType;
use std::io::Write;
use std::time::Duration;

// Use the core CtlError for methods that use CtlCore (accessible via ctl module)
type CtlError = link::ctl::core::CtlError;

/// The application wraps CtlCore with SyncPortAdapter for synchronous CLI usage.
/// All operations go through CtlCore via block_on().
pub struct App {
    /// The async core for all operations.
    core: CtlCore<SyncPortAdapter<BufferedPort<Box<dyn serialport::SerialPort>>>>,
}

impl App {
    /// Create a new App wrapping the given buffered port.
    pub fn new(port: BufferedPort<Box<dyn serialport::SerialPort>>) -> Self {
        let adapter = SyncPortAdapter::new(port);
        let core = CtlCore::new(adapter);
        Self { core }
    }

    /// Get a mutable reference to the underlying port.
    pub fn port_mut(&mut self) -> &mut BufferedPort<Box<dyn serialport::SerialPort>> {
        self.core.port_mut().get_mut()
    }

    /// Send Hello handshake to detect if a valid device is connected.
    pub fn hello(&mut self, challenge: &[u8; 4]) -> bool {
        block_on(self.core.hello(challenge))
    }

    /// Ping the MGMT chip.
    pub fn mgmt_ping(&mut self, data: &[u8]) -> Result<(), CtlError> {
        block_on(self.core.mgmt_ping(data))
    }

    /// Get MGMT chip stack usage information.
    pub fn mgmt_get_stack_info(&mut self) -> Result<link::ctl::StackInfoResult, CtlError> {
        block_on(self.core.mgmt_get_stack_info())
    }

    /// Repaint the MGMT chip stack for future measurement.
    pub fn mgmt_repaint_stack(&mut self) -> Result<(), CtlError> {
        block_on(self.core.mgmt_repaint_stack())
    }

    /// Ping the UI chip through the MGMT tunnel.
    pub fn ui_ping(&mut self, data: &[u8]) -> Result<(), CtlError> {
        block_on(self.core.ui_ping(data))
    }

    /// Get the version stored in UI chip EEPROM.
    pub fn get_version(&mut self) -> Result<u32, CtlError> {
        block_on(self.core.get_version())
    }

    /// Set the version stored in UI chip EEPROM.
    pub fn set_version(&mut self, version: u32) -> Result<(), CtlError> {
        block_on(self.core.set_version(version))
    }

    /// Get the SFrame key stored in UI chip EEPROM.
    pub fn get_sframe_key(&mut self) -> Result<[u8; 16], CtlError> {
        block_on(self.core.get_sframe_key())
    }

    /// Set the SFrame key stored in UI chip EEPROM.
    pub fn set_sframe_key(&mut self, key: &[u8; 16]) -> Result<(), CtlError> {
        block_on(self.core.set_sframe_key(key))
    }

    /// Set UI chip loopback mode.
    pub fn ui_set_loopback(&mut self, mode: link::LoopbackMode) -> Result<(), CtlError> {
        block_on(self.core.ui_set_loopback(mode))
    }

    /// Get UI chip loopback mode.
    pub fn ui_get_loopback(&mut self) -> Result<link::LoopbackMode, CtlError> {
        block_on(self.core.ui_get_loopback())
    }

    /// Get UI chip stack usage information.
    pub fn ui_get_stack_info(&mut self) -> Result<link::ctl::StackInfoResult, CtlError> {
        block_on(self.core.ui_get_stack_info())
    }

    /// Repaint the UI chip stack for future measurement.
    pub fn ui_repaint_stack(&mut self) -> Result<(), CtlError> {
        block_on(self.core.ui_repaint_stack())
    }

    /// Reset the UI chip into bootloader mode.
    pub fn reset_ui_to_bootloader(&mut self) -> Result<(), CtlError> {
        block_on(self.core.reset_ui_to_bootloader())
    }

    /// Reset the UI chip into user mode.
    pub fn reset_ui_to_user(&mut self) -> Result<(), CtlError> {
        block_on(self.core.reset_ui_to_user())
    }

    /// Ping the NET chip through the MGMT tunnel.
    pub fn net_ping(&mut self, data: &[u8]) -> Result<(), CtlError> {
        block_on(self.core.net_ping(data))
    }

    /// Set NET chip loopback mode.
    pub fn net_set_loopback(&mut self, mode: link::NetLoopback) -> Result<(), CtlError> {
        block_on(self.core.net_set_loopback(mode))
    }

    /// Get NET chip loopback mode.
    pub fn net_get_loopback(&mut self) -> Result<link::NetLoopback, CtlError> {
        block_on(self.core.net_get_loopback())
    }

    /// Add a WiFi SSID and password pair.
    pub fn add_wifi_ssid(&mut self, ssid: &str, password: &str) -> Result<(), CtlError> {
        block_on(self.core.add_wifi_ssid(ssid, password))
    }

    /// Clear all WiFi SSIDs.
    pub fn clear_wifi_ssids(&mut self) -> Result<(), CtlError> {
        block_on(self.core.clear_wifi_ssids())
    }

    /// Get the relay URL.
    pub fn get_relay_url(&mut self) -> Result<String, CtlError> {
        block_on(self.core.get_relay_url()).map(|s| s.to_string())
    }

    /// Set the relay URL.
    pub fn set_relay_url(&mut self, url: &str) -> Result<(), CtlError> {
        block_on(self.core.set_relay_url(url))
    }

    /// Get configuration for a specific channel.
    pub fn get_channel_config(&mut self, channel_id: u8) -> Result<link::ctl::ChannelConfig, CtlError> {
        block_on(self.core.get_channel_config(channel_id))
    }

    /// Set configuration for a channel.
    pub fn set_channel_config(&mut self, config: &link::ctl::ChannelConfig) -> Result<(), CtlError> {
        block_on(self.core.set_channel_config(config))
    }

    /// Clear all channel configurations.
    pub fn clear_channel_configs(&mut self) -> Result<(), CtlError> {
        block_on(self.core.clear_channel_configs())
    }

    /// Get jitter buffer statistics for a channel.
    pub fn get_jitter_stats(&mut self, channel_id: u8) -> Result<link::ctl::JitterStatsResult, CtlError> {
        block_on(self.core.get_jitter_stats(channel_id))
    }

    /// Reset the NET chip into bootloader mode.
    pub fn reset_net_to_bootloader(&mut self) -> Result<(), CtlError> {
        block_on(self.core.reset_net_to_bootloader())
    }

    /// Reset the NET chip into user mode.
    pub fn reset_net_to_user(&mut self) -> Result<(), CtlError> {
        block_on(self.core.reset_net_to_user())
    }

    /// Send a WebSocket ping and verify echo response.
    pub fn ws_ping(&mut self, data: &[u8]) -> Result<(), CtlError> {
        block_on(self.core.ws_ping(data))
    }

    /// Run WebSocket echo test.
    pub fn ws_echo_test(&mut self) -> Result<link::ctl::EchoTestResults, CtlError> {
        block_on(self.core.ws_echo_test())
    }

    /// Run WebSocket speed test.
    pub fn ws_speed_test(&mut self) -> Result<link::ctl::SpeedTestResults, CtlError> {
        block_on(self.core.ws_speed_test())
    }

    /// Send a chat message via MoQ.
    pub fn send_chat_message(&mut self, message: &str) -> Result<(), CtlError> {
        block_on(self.core.send_chat_message(message))
    }

    /// Send a UI-first circular ping.
    pub fn ui_first_circular_ping(&mut self, data: &[u8]) -> Result<(), CtlError> {
        block_on(self.core.ui_first_circular_ping(data))
    }

    /// Send a NET-first circular ping.
    pub fn net_first_circular_ping(&mut self, data: &[u8]) -> Result<(), CtlError> {
        block_on(self.core.net_first_circular_ping(data))
    }

    /// Hold the UI chip in reset.
    pub fn hold_ui_reset(&mut self) -> Result<(), CtlError> {
        block_on(self.core.hold_ui_reset())
    }

    /// Hold the NET chip in reset.
    pub fn hold_net_reset(&mut self) -> Result<(), CtlError> {
        block_on(self.core.hold_net_reset())
    }

    /// Get all WiFi SSIDs from NET chip storage.
    pub fn get_wifi_ssids(&mut self) -> Result<heapless::Vec<link::WifiSsid, 8>, CtlError> {
        block_on(self.core.get_wifi_ssids())
    }

    /// Get all channel configurations.
    pub fn get_all_channel_configs(&mut self) -> Result<heapless::Vec<link::ctl::ChannelConfig, 4>, CtlError> {
        block_on(self.core.get_all_channel_configs())
    }

    /// Drain any pending data from buffers.
    pub fn drain(&mut self) {
        self.core.drain();
    }

    // =========================================================================
    // Flashing methods using CtlCore
    // =========================================================================

    /// Get bootloader information from the MGMT chip.
    pub fn get_mgmt_bootloader_info(&mut self) -> Result<MgmtBootloaderInfo, link::ctl::stm::Error<std::io::Error>> {
        block_on(self.core.get_mgmt_bootloader_info())
    }

    /// Flash firmware to the MGMT chip.
    pub fn flash_mgmt<F>(&mut self, firmware: &[u8], progress: F) -> Result<(), FlashError<std::io::Error>>
    where
        F: FnMut(FlashPhase, usize, usize),
    {
        block_on(self.core.flash_mgmt(firmware, progress))
    }

    /// Set the NET UART baud rate on the MGMT chip.
    pub fn set_net_baud_rate(&mut self, baud_rate: u32) -> Result<(), CtlError> {
        block_on(self.core.set_net_baud_rate(baud_rate))
    }

    /// Set the CTL UART baud rate on the MGMT chip.
    pub fn set_ctl_baud_rate(&mut self, baud_rate: u32) -> Result<(), CtlError> {
        block_on(self.core.set_ctl_baud_rate(baud_rate))
    }

    /// Write a TLV to the MGMT connection.
    pub fn write_tlv(&mut self, tlv_type: link::CtlToMgmt, value: &[u8]) -> Result<(), CtlError> {
        block_on(self.core.write_tlv_raw(tlv_type, value))
    }

    /// Read a TLV from the MGMT connection.
    pub fn read_tlv(&mut self) -> Result<Option<link::Tlv<link::MgmtToCtl>>, CtlError> {
        block_on(self.core.read_tlv_raw())
    }

    /// Get bootloader information from the UI chip.
    pub fn get_ui_bootloader_info<D>(&mut self, delay_ms: D) -> Result<MgmtBootloaderInfo, link::ctl::stm::Error<std::io::Error>>
    where
        D: FnOnce(u64),
    {
        block_on(self.core.get_ui_bootloader_info(delay_ms))
    }

    /// Flash firmware to the UI chip.
    pub fn flash_ui<F, D>(&mut self, firmware: &[u8], delay_ms: D, verify: bool, progress: F) -> Result<(), FlashError<std::io::Error>>
    where
        F: FnMut(FlashPhase, usize, usize),
        D: FnOnce(u64),
    {
        block_on(self.core.flash_ui(firmware, delay_ms, verify, progress))
    }

    /// Read a log message from the UI chip.
    pub fn read_ui_log(&mut self) -> Result<Option<String>, CtlError> {
        block_on(self.core.read_ui_log())
    }

    /// Try to read a UI log (timeout-aware, for polling).
    pub fn try_read_ui_log(&mut self) -> Result<Option<String>, CtlError> {
        block_on(self.core.try_read_ui_log())
    }

    /// Get bootloader information from the NET chip.
    pub fn get_net_bootloader_info(&mut self) -> Result<EspflashDeviceInfo, EspflashError> {
        block_on(self.core.get_net_bootloader_info())
    }

    /// Flash firmware to the NET chip.
    pub fn flash_net(&mut self, firmware: &[u8], partition_table: Option<&[u8]>, progress: &mut dyn ProgressCallbacks) -> Result<(), EspflashError> {
        block_on(self.core.flash_net(firmware, partition_table, progress))
    }

    /// Erase the NET chip's flash.
    pub fn erase_net(&mut self) -> Result<(), EspflashError> {
        block_on(self.core.erase_net())
    }
}

#[derive(Parser)]
#[command(name = "ctl")]
#[command(about = "Control interface for the link device", long_about = None)]
struct Cli {
    #[arg(short, long)]
    port: Option<String>,

    #[arg(short, long, default_value = "115200")]
    baud: u32,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Debug, Parser)]
#[command(name = "")]
struct ReplCli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Clone, Subcommand)]
enum Command {
    Mgmt {
        #[command(subcommand)]
        action: MgmtAction,
    },

    Ui {
        #[command(subcommand)]
        action: UiAction,
    },

    Net {
        #[command(subcommand)]
        action: NetAction,
    },

    CircularPing {
        #[arg(short, long)]
        reverse: bool,

        #[arg(default_value = "hello")]
        data: String,
    },

    Exit,
}

#[derive(Debug, Clone, Subcommand)]
enum MgmtAction {
    Ping {
        #[arg(default_value = "hello")]
        data: String,
    },
    Info,
    Flash {
        file: std::path::PathBuf,
    },
    #[command(name = "net-baud-rate")]
    NetBaudRate {
        #[command(subcommand)]
        action: Option<GetSetU32>,
    },
    #[command(name = "ctl-baud-rate")]
    CtlBaudRate {
        #[command(subcommand)]
        action: Option<GetSetU32>,
    },
    /// Run a speed test on the CTL-MGMT UART link.
    /// Sends packets as fast as possible for the duration, then reports results.
    #[command(name = "speed-test")]
    SpeedTest {
        /// Baud rate to use for the test (default: current baud rate)
        #[arg(short, long)]
        baud: Option<u32>,

        /// Duration of the test in seconds (default: 1)
        #[arg(short, long, default_value = "1")]
        duration: u32,

        /// Payload size in bytes (default: 64, max: 640)
        #[arg(short, long, default_value = "64")]
        size: usize,
    },

    /// Stack usage measurement
    Stack {
        #[command(subcommand)]
        action: Option<StackAction>,
    },
}

#[derive(Debug, Clone, Subcommand)]
enum UiAction {
    Ping {
        #[arg(default_value = "hello")]
        data: String,
    },

    Info,

    Flash {
        file: std::path::PathBuf,
        /// Skip verification after flashing
        #[arg(long)]
        no_verify: bool,
    },

    Version {
        #[command(subcommand)]
        action: Option<GetSetU32>,
    },

    #[command(name = "sframe-key")]
    SFrameKey {
        #[command(subcommand)]
        action: Option<GetSetHex>,
    },

    Loopback {
        #[command(subcommand)]
        action: Option<LoopbackAction>,
    },

    Reset {
        action: Option<String>,
    },

    /// Monitor log messages from UI chip
    Monitor {
        /// Reset the chip before monitoring
        #[arg(long)]
        reset: bool,
    },

    /// Stack usage measurement
    Stack {
        #[command(subcommand)]
        action: Option<StackAction>,
    },
}

#[derive(Debug, Clone, Default, Subcommand)]
enum StackAction {
    /// Get stack usage information
    #[default]
    Info,
    /// Repaint the stack with the known pattern
    Repaint,
}

#[derive(Debug, Clone, Default, Subcommand)]
enum GetSetU32 {
    #[default]
    Get,
    Set {
        value: u32,
    },
}

#[derive(Debug, Clone, Default, Subcommand)]
enum GetSetHex {
    #[default]
    Get,
    Set {
        value: String,
    },
}

#[derive(Debug, Clone, Default, Subcommand)]
enum NetLoopbackMode {
    #[default]
    Get,
    /// Normal PTT operation - audio to MoQ, filter self-echo
    Off,
    /// Local bypass - audio directly back to UI (no MoQ)
    Raw,
    /// MoQ loopback - audio to MoQ, hear own audio via relay
    Moq,
}

#[derive(Debug, Clone, Default, Subcommand)]
enum GetSetString {
    #[default]
    Get,
    Set {
        value: String,
    },
}

#[derive(Debug, Clone, Default, Subcommand)]
enum LoopbackAction {
    #[default]
    Get,
    Off,
    Raw,
    Alaw,
    Sframe,
}

#[derive(Debug, Clone, Subcommand)]
enum NetAction {
    Ping {
        #[arg(default_value = "hello")]
        data: String,
    },

    Info,

    Flash {
        file: std::path::PathBuf,

        /// Path to a custom partition table (CSV or binary).
        /// If not specified, the default partition table is used.
        #[arg(short = 'P', long)]
        partition_table: Option<std::path::PathBuf>,
    },

    Wifi {
        #[command(subcommand)]
        action: Option<WifiAction>,
    },

    #[command(name = "relay-url")]
    RelayUrl {
        #[command(subcommand)]
        action: Option<GetSetString>,
    },

    #[command(name = "ws-ping")]
    WsPing {
        #[arg(default_value = "hello from hactar")]
        data: String,
    },

    #[command(name = "ws-echo-test")]
    WsEchoTest,

    #[command(name = "ws-speed-test")]
    WsSpeedTest,

    /// Set loopback mode: off (normal PTT), raw (local bypass), moq (hear own audio via relay)
    Loopback {
        #[command(subcommand)]
        mode: Option<NetLoopbackMode>,
    },

    #[command(name = "chat")]
    Chat {
        /// Chat message to send
        message: String,
    },

    /// Reset the NET chip
    Reset {
        /// Reset action: "bootloader" to enter bootloader mode, or nothing/anything else for user mode
        action: Option<String>,
    },

    /// Erase the NET chip's flash via OpenOCD
    Erase,

    /// Monitor data from NET chip (prints FromNet TLVs)
    Monitor {
        /// Reset the chip before monitoring
        #[arg(long)]
        reset: bool,
    },

    /// Manage channel configurations
    Channel {
        #[command(subcommand)]
        action: Option<ChannelAction>,
    },

    /// Get jitter buffer statistics for a channel
    #[command(name = "jitter-stats")]
    JitterStats {
        /// Channel ID (0=Ptt, 1=PttAi)
        channel_id: u8,
    },
}

#[derive(Debug, Clone, Subcommand)]
enum WifiAction {
    Add { ssid: String, password: String },
    Clear,
}

#[derive(Debug, Clone, Subcommand)]
enum ChannelAction {
    /// Get configuration for a specific channel
    Get {
        /// Channel ID (0=Ptt, 1=PttAi)
        channel_id: u8,
    },
    /// Set configuration for a channel
    Set {
        /// Channel ID (0=Ptt, 1=PttAi)
        channel_id: u8,
        /// Enable the channel
        #[arg(long)]
        enabled: bool,
        /// Relay URL for this channel (empty = use global)
        #[arg(long, default_value = "")]
        relay_url: String,
    },
    /// Clear all channel configurations
    Clear,
}

/// Open a serial port with standard settings
fn open_serial_port(
    port_name: &str,
    baud: u32,
) -> Result<Box<dyn serialport::SerialPort>, serialport::Error> {
    serialport::new(port_name, baud)
        .parity(serialport::Parity::Even)
        .timeout(Duration::from_secs(3))
        .open()
}

/// Try to connect to a specific port and verify it's a Link device.
/// Returns the App if successful, None if connection failed or not a Link device.
fn try_connect(port_name: &str, baud: u32) -> Option<App> {
    let mut port = open_serial_port(port_name, baud).ok()?;

    // Set short timeout for hello check
    port.set_timeout(Duration::from_millis(50)).ok()?;

    let buffered_port = BufferedPort::new(port);
    let mut app = App::new(buffered_port);
    let challenge: [u8; 4] = rand::rng().random();

    if app.hello(&challenge) {
        // Restore normal timeout for subsequent operations
        app.port_mut()
            .get_mut()
            .set_timeout(Duration::from_secs(3))
            .ok()?;
        Some(app)
    } else {
        None
    }
}

/// Find a Link device among available ports and return a connected App.
fn find_link_device(baud: u32) -> Option<(App, String)> {
    let all_ports: Vec<_> = serialport::available_ports()
        .unwrap_or_default()
        .into_iter()
        .filter(|p| matches!(p.port_type, SerialPortType::UsbPort(_)))
        .map(|p| p.port_name)
        .filter(|name| !name.starts_with("/dev/tty."))
        .collect();

    if all_ports.is_empty() {
        return None;
    }

    println!("Scanning for Link devices...");

    for port_name in &all_ports {
        if let Some(app) = try_connect(port_name, baud) {
            println!("Found Link device on {}", port_name);
            return Some((app, port_name.clone()));
        }
    }

    None
}

/// Prompt user to manually select a port and connect.
fn manually_select_port(baud: u32) -> Result<(App, String), String> {
    let all_ports: Vec<_> = serialport::available_ports()
        .unwrap_or_default()
        .into_iter()
        .filter(|p| matches!(p.port_type, SerialPortType::UsbPort(_)))
        .map(|p| p.port_name)
        .filter(|name| !name.starts_with("/dev/tty."))
        .collect();

    if all_ports.is_empty() {
        return Err("No USB serial ports found".to_string());
    }

    println!("No Link devices detected, showing all USB serial ports:");
    for (i, port) in all_ports.iter().enumerate() {
        println!("  {}: {}", i + 1, port);
    }
    print!("Select port [1-{}] (default: 1): ", all_ports.len());
    std::io::stdout().flush().unwrap();

    let mut input = String::new();
    std::io::stdin()
        .read_line(&mut input)
        .map_err(|e| format!("Failed to read input: {}", e))?;

    let trimmed = input.trim();
    let choice: usize = if trimmed.is_empty() {
        1
    } else {
        trimmed.parse().map_err(|_| "Invalid number".to_string())?
    };

    if choice < 1 || choice > all_ports.len() {
        return Err(format!(
            "Please select a number between 1 and {}",
            all_ports.len()
        ));
    }

    let port_name = &all_ports[choice - 1];
    let port =
        open_serial_port(port_name, baud).map_err(|e| format!("Failed to open port: {}", e))?;

    let buffered_port = BufferedPort::new(port);
    Ok((App::new(buffered_port), port_name.clone()))
}

/// Open a connection to the device
fn connect(port: Option<String>, baud: u32) -> Result<(App, String), Box<dyn std::error::Error>> {
    // If user specified a port, connect directly
    if let Some(port_name) = port {
        let port = open_serial_port(&port_name, baud)?;
        let buffered_port = BufferedPort::new(port);
        return Ok((App::new(buffered_port), port_name));
    }

    // Try to find a Link device automatically
    if let Some((app, port_name)) = find_link_device(baud) {
        return Ok((app, port_name));
    }

    // Fall back to manual selection
    manually_select_port(baud).map_err(|e| e.into())
}

fn dispatch(cmd: Command, app: &mut App) -> Result<(), Box<dyn std::error::Error>> {
    match cmd {
        Command::Mgmt { action } => handlers::handle_mgmt(action, app),
        Command::Ui { action } => handlers::handle_ui(action, app),
        Command::Net { action } => handlers::handle_net(action, app),
        Command::CircularPing { reverse, data } => {
            if reverse {
                println!("Sending NET-first circular ping with data: {}", data);
                app.net_first_circular_ping(data.as_bytes())?;
            } else {
                println!("Sending UI-first circular ping with data: {}", data);
                app.ui_first_circular_ping(data.as_bytes())?;
            }
            println!("Completed circular ping!");
            Ok(())
        }
        Command::Exit => {
            std::process::exit(0);
        }
    }
}

fn mgmt_handler(
    args: ArgMatches,
    app: &mut App,
) -> Result<Option<String>, reedline_repl_rs::Error> {
    let action = MgmtAction::from_arg_matches(&args)
        .map_err(|e| reedline_repl_rs::Error::UnknownCommand(e.to_string()))?;
    dispatch(Command::Mgmt { action }, app)
        .map(|()| None)
        .map_err(|e| reedline_repl_rs::Error::UnknownCommand(e.to_string()))
}

fn ui_handler(args: ArgMatches, app: &mut App) -> Result<Option<String>, reedline_repl_rs::Error> {
    let action = UiAction::from_arg_matches(&args)
        .map_err(|e| reedline_repl_rs::Error::UnknownCommand(e.to_string()))?;
    dispatch(Command::Ui { action }, app)
        .map(|()| None)
        .map_err(|e| reedline_repl_rs::Error::UnknownCommand(e.to_string()))
}

fn net_handler(args: ArgMatches, app: &mut App) -> Result<Option<String>, reedline_repl_rs::Error> {
    let action = NetAction::from_arg_matches(&args)
        .map_err(|e| reedline_repl_rs::Error::UnknownCommand(e.to_string()))?;
    dispatch(Command::Net { action }, app)
        .map(|()| None)
        .map_err(|e| reedline_repl_rs::Error::UnknownCommand(e.to_string()))
}

fn circular_ping_handler(
    args: ArgMatches,
    app: &mut App,
) -> Result<Option<String>, reedline_repl_rs::Error> {
    let reverse = args.get_flag("reverse");
    let data = args
        .get_one::<String>("data")
        .cloned()
        .unwrap_or_else(|| "hello".to_string());
    dispatch(Command::CircularPing { reverse, data }, app)
        .map(|()| None)
        .map_err(|e| reedline_repl_rs::Error::UnknownCommand(e.to_string()))
}

fn exit_handler(
    _args: ArgMatches,
    app: &mut App,
) -> Result<Option<String>, reedline_repl_rs::Error> {
    dispatch(Command::Exit, app)
        .map(|()| None)
        .map_err(|e| reedline_repl_rs::Error::UnknownCommand(e.to_string()))
}

fn run_repl(app: App, port_name: &str) -> Result<(), reedline_repl_rs::Error> {
    println!("Connected to {} - entering REPL mode", port_name);
    println!("Type 'help' for available commands, 'exit' to quit\n");

    let mut callbacks: CallBackMap<App, reedline_repl_rs::Error> = CallBackMap::new();
    callbacks.insert("mgmt".to_string(), mgmt_handler);
    callbacks.insert("ui".to_string(), ui_handler);
    callbacks.insert("net".to_string(), net_handler);
    callbacks.insert("circular-ping".to_string(), circular_ping_handler);
    callbacks.insert("exit".to_string(), exit_handler);

    let mut repl = Repl::<App, reedline_repl_rs::Error>::new(app)
        .with_name("ctl")
        .with_version(env!("CARGO_PKG_VERSION"))
        .with_description("Control interface for the link device")
        .with_banner(&format!("Connected to {}", port_name))
        .with_derived::<ReplCli>(callbacks);

    repl.run()
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();

    match cli.command {
        Some(cmd) => {
            // CLI mode: connect, run one command, exit
            let (mut app, port_name) = connect(cli.port, cli.baud)?;
            println!("Connected to {} at {} baud", port_name, cli.baud);

            if let Err(e) = dispatch(cmd, &mut app) {
                eprintln!("Error: {}", e);
                std::process::exit(1);
            }
        }
        None => {
            // REPL mode: connect once, run commands in loop
            let (app, port_name) = connect(cli.port, cli.baud)?;
            run_repl(app, &port_name)?;
        }
    }

    Ok(())
}

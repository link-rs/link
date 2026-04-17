//! CTL - Control interface for the link device
//!
//! CLI mode:  ctl ui ping hello
//! REPL mode: ctl (no args) -> interactive prompt

mod handlers;
mod serial;

use clap::{FromArgMatches, Parser, Subcommand};
use link::ctl::{CtlCore, SetTimeout};
use rand::Rng;
use reedline_repl_rs::clap::ArgMatches;
use reedline_repl_rs::{CallBackMap, Repl};
use serial::TokioSerialPort;
use std::io::Write;
use std::time::Duration;
use tokio_serial::{SerialPortInfo, SerialPortType, SerialStream};

/// Type alias for the CtlCore with tokio-serial port.
pub type Core = CtlCore<TokioSerialPort>;

/// Create a new Core from a TokioSerialPort.
fn new_core(port: TokioSerialPort) -> Core {
    CtlCore::new(port)
}

#[derive(Parser)]
#[command(name = "ctl")]
#[command(about = "Control interface for the link device", long_about = None)]
struct Cli {
    /// Serial port to connect to (auto-detected if omitted)
    #[arg(short, long)]
    port: Option<String>,

    /// Baud rate for the serial connection
    #[arg(short, long, default_value_t = link::uart_config::HIGH_SPEED.baudrate)]
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
    /// Commands for the MGMT chip
    Mgmt {
        #[command(subcommand)]
        action: MgmtAction,
    },

    /// Commands for the UI chip
    Ui {
        #[command(subcommand)]
        action: UiAction,
    },

    /// Commands for the NET chip
    Net {
        #[command(subcommand)]
        action: NetAction,
    },

    /// Send a hello handshake to verify connectivity
    Hello,

    /// Send a ping that traverses all chips in a circle
    CircularPing {
        /// Send NET-first instead of the default UI-first
        #[arg(short, long)]
        reverse: bool,

        #[arg(default_value = "hello")]
        data: String,
    },

    /// Exit the REPL
    Exit,
}

#[derive(Debug, Clone, Subcommand)]
enum MgmtAction {
    /// Send a ping to the MGMT chip
    Ping {
        #[arg(default_value = "hello")]
        data: String,
    },
    /// Get MGMT chip firmware info
    Info,
    /// Get the MGMT board version from option bytes
    Board,
    /// Get or set the MGMT DATA0 version option byte
    Version {
        #[command(subcommand)]
        action: Option<GetSetU8>,
    },
    /// Flash firmware to the MGMT chip
    Flash { file: std::path::PathBuf },
    /// Stack usage measurement
    Stack {
        #[command(subcommand)]
        action: Option<StackAction>,
    },
}

#[derive(Debug, Clone, Subcommand)]
enum UiAction {
    /// Send a ping to the UI chip
    Ping {
        #[arg(default_value = "hello")]
        data: String,
    },

    /// Get UI chip firmware info
    Info,

    /// Flash firmware to the UI chip
    Flash {
        file: std::path::PathBuf,
        /// Skip verification after flashing
        #[arg(long)]
        no_verify: bool,
    },

    /// Get or set the UI firmware version field
    Version {
        #[command(subcommand)]
        action: Option<GetSetU32>,
    },

    /// Get or set the SFrame encryption key
    #[command(name = "sframe-key")]
    SFrameKey {
        #[command(subcommand)]
        action: Option<GetSetHex>,
    },

    /// Get or set the UI audio loopback mode
    Loopback {
        #[command(subcommand)]
        action: Option<LoopbackAction>,
    },

    /// Get or set the UI output volume
    Volume {
        #[command(subcommand)]
        action: Option<GetSetU8>,
    },

    /// Set UI BOOT0 pin
    Boot0 {
        #[command(subcommand)]
        action: PinAction,
    },

    /// Set UI BOOT1 pin
    Boot1 {
        #[command(subcommand)]
        action: PinAction,
    },

    /// Set UI RST pin
    Rst {
        #[command(subcommand)]
        action: PinAction,
    },

    /// Reset the UI chip
    Reset {
        #[command(subcommand)]
        action: Option<ResetAction>,
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

    /// Logs enabled control
    Logs {
        #[command(subcommand)]
        action: Option<LogsAction>,
    },

    /// Clear all stored configuration (EEPROM)
    #[command(name = "clear-storage")]
    ClearStorage,

    /// Get or set the audio routing mode (ctl or net)
    #[command(name = "audio-mode")]
    AudioMode {
        #[command(subcommand)]
        action: Option<AudioModeAction>,
    },

    /// Audio capture and playback (sets audio-mode to ctl)
    Audio {
        #[command(subcommand)]
        action: AudioAction,
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
enum LogsAction {
    /// Get logs enabled state
    #[default]
    Get,
    /// Enable logs
    On,
    /// Disable logs
    Off,
}

#[derive(Debug, Clone, Subcommand)]
enum PinAction {
    /// Set pin level
    Set {
        #[arg(value_enum)]
        level: PinLevel,
    },
}

#[derive(Debug, Clone, clap::ValueEnum)]
enum PinLevel {
    High,
    Low,
}

#[derive(Debug, Clone, Default, Subcommand)]
enum ResetAction {
    /// Reset to user mode (normal operation)
    #[default]
    User,
    /// Reset to bootloader mode
    Bootloader,
    /// Hold in reset (RST low)
    Hold,
    /// Release from reset (RST high)
    Release,
}

#[derive(Debug, Clone, Default, Subcommand)]
enum GetSetU32 {
    /// Get the current value
    #[default]
    Get,
    /// Set a new value
    Set { value: u32 },
}

#[derive(Debug, Clone, Default, Subcommand)]
enum GetSetU8 {
    /// Get the current value
    #[default]
    Get,
    /// Set a new value
    Set { value: u8 },
}

#[derive(Debug, Clone, Default, Subcommand)]
enum GetSetHex {
    /// Get the current value
    #[default]
    Get,
    /// Set a new hex value
    Set { value: String },
}

#[derive(Debug, Clone, Default, Subcommand)]
enum NetLoopbackAction {
    /// Get the current loopback mode
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
    /// Get the current value
    #[default]
    Get,
    /// Set a new value
    Set { value: String },
}

#[derive(Debug, Clone, Copy, clap::ValueEnum)]
enum Language {
    #[value(name = "en-US")]
    EnUs,
    #[value(name = "es-ES")]
    EsEs,
    #[value(name = "de-DE")]
    DeDe,
    #[value(name = "hi-IN")]
    HiIn,
    #[value(name = "nb-NO")]
    NbNo,
}

impl std::fmt::Display for Language {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Language::EnUs => write!(f, "en-US"),
            Language::EsEs => write!(f, "es-ES"),
            Language::DeDe => write!(f, "de-DE"),
            Language::HiIn => write!(f, "hi-IN"),
            Language::NbNo => write!(f, "nb-NO"),
        }
    }
}

#[derive(Debug, Clone, Default, Subcommand)]
enum LanguageAction {
    #[default]
    Get,
    Set {
        #[arg(value_enum)]
        lang: Language,
    },
}

#[derive(Debug, Clone, Default, Subcommand)]
enum LoopbackAction {
    /// Get the current loopback mode
    #[default]
    Get,
    /// Disable loopback (normal operation)
    Off,
    /// Raw PCM loopback
    Raw,
    /// A-law codec loopback
    Alaw,
    /// SFrame encrypted loopback
    Sframe,
}

#[derive(Debug, Clone, Default, Subcommand)]
enum AudioModeAction {
    /// Get the current audio routing mode
    #[default]
    Get,
    /// Route audio to/from CTL for capture/playback testing
    Ctl,
    /// Route audio to/from NET (normal operation)
    Net,
}

#[derive(Debug, Clone, Subcommand)]
enum AudioAction {
    /// Capture audio from the UI chip
    Capture {
        #[command(subcommand)]
        mode: CaptureMode,
    },
    /// Play audio to the UI chip
    Play {
        #[command(subcommand)]
        mode: PlayMode,
    },
}

#[derive(Debug, Clone, Subcommand)]
enum CaptureMode {
    /// Play captured audio to computer speakers (8kHz mono)
    Live,
    /// Save captured audio to a WAV file (8kHz mono 16-bit)
    Wav { file: std::path::PathBuf },
}

#[derive(Debug, Clone, Subcommand)]
enum PlayMode {
    /// Play a WAV file to the device speaker (must be 8kHz mono)
    Wav { file: std::path::PathBuf },
    /// Stream from computer microphone to device speaker (8kHz mono)
    Live,
}

#[derive(Debug, Clone, Subcommand)]
enum NetAction {
    /// Send a ping to the NET chip
    Ping {
        #[arg(default_value = "hello")]
        data: String,
    },

    /// Get NET chip firmware info
    Info,

    /// Flash firmware to the NET chip
    Flash {
        file: std::path::PathBuf,

        /// Path to a custom partition table (CSV or binary).
        /// If not specified, the default partition table is used.
        #[arg(short = 'P', long)]
        partition_table: Option<std::path::PathBuf>,
    },

    /// Manage saved Wi-Fi credentials
    Wifi {
        #[command(subcommand)]
        action: Option<WifiAction>,
    },

    /// Get or set the MoQ relay URL
    #[command(name = "relay-url")]
    RelayUrl {
        #[command(subcommand)]
        action: Option<GetSetString>,
    },

    /// Set loopback mode: off (normal PTT), raw (local bypass), moq (hear own audio via relay)
    Loopback {
        #[command(subcommand)]
        mode: Option<NetLoopbackAction>,
    },

    /// Set NET BOOT pin (GPIO0)
    Boot {
        #[command(subcommand)]
        action: PinAction,
    },

    /// Set NET RST pin (EN)
    Rst {
        #[command(subcommand)]
        action: PinAction,
    },

    /// Reset the NET chip
    Reset {
        #[command(subcommand)]
        action: Option<ResetAction>,
    },

    /// Erase the NET chip's flash
    Erase,

    /// Monitor data from NET chip (prints FromNet TLVs)
    Monitor {
        /// Reset the chip before monitoring
        #[arg(long)]
        reset: bool,
    },

    /// Logs enabled control
    Logs {
        #[command(subcommand)]
        action: Option<LogsAction>,
    },

    /// Language setting
    Language {
        #[command(subcommand)]
        action: Option<LanguageAction>,
    },

    /// Channel configuration (JSON array: ["relay","org","channel","ptt"])
    Channel {
        #[command(subcommand)]
        action: Option<GetSetString>,
    },

    /// AI configuration (JSON object)
    #[command(name = "ai")]
    Ai {
        #[command(subcommand)]
        action: Option<GetSetString>,
    },

    /// Clear all stored configuration (NVS)
    #[command(name = "clear-storage")]
    ClearStorage,

    /// Burn JTAG/USB disable efuse (IRREVERSIBLE!)
    #[command(name = "burn-jtag-efuse")]
    BurnJtagEfuse {
        /// Skip confirmation prompt (dangerous!)
        #[arg(long)]
        yes: bool,
    },
}

#[derive(Debug, Clone, Subcommand)]
enum WifiAction {
    /// Add a Wi-Fi network
    Add { ssid: String, password: String },
    /// Clear all saved Wi-Fi credentials
    Clear,
}

/// Open a serial port with standard settings using tokio-serial.
fn open_serial_port(port_name: &str, baud: u32) -> Result<SerialStream, tokio_serial::Error> {
    let builder = tokio_serial::new(port_name, baud).parity(tokio_serial::Parity::Even);
    SerialStream::open(&builder)
}

/// Get available USB serial ports.
fn available_ports() -> Vec<SerialPortInfo> {
    tokio_serial::available_ports()
        .unwrap_or_default()
        .into_iter()
        .filter(|p| matches!(p.port_type, SerialPortType::UsbPort(_)))
        .filter(|p| !p.port_name.starts_with("/dev/tty."))
        .collect()
}

/// Try to connect to a specific port and verify it's a Link device.
/// Returns the Core if successful, None if connection failed or not a Link device.
async fn try_connect(port_name: &str, baud: u32) -> Option<Core> {
    let stream = open_serial_port(port_name, baud).ok()?;
    let port = TokioSerialPort::new(stream);
    let mut core = new_core(port);

    // Ensure MGMT chip is running (not held in reset by DTR)
    let delay_ms = |ms| tokio::time::sleep(Duration::from_millis(ms));
    core.init_port(delay_ms).await;

    // Set short timeout for hello check
    core.port_mut()
        .set_timeout(Duration::from_millis(500))
        .ok()?;

    let challenge: [u8; 4] = rand::rng().random();
    if core.hello(&challenge).await {
        // Restore normal timeout for subsequent operations
        core.port_mut().set_timeout(Duration::from_secs(3)).ok()?;
        Some(core)
    } else {
        None
    }
}

/// Find a Link device among available ports and return a connected Core.
async fn find_link_device(baud: u32) -> Option<(Core, String)> {
    let all_ports: Vec<_> = available_ports().into_iter().map(|p| p.port_name).collect();

    if all_ports.is_empty() {
        return None;
    }

    println!("Scanning for Link devices...");

    for port_name in &all_ports {
        if let Some(mut core) = try_connect(port_name, baud).await {
            println!("Found Link device on {}", port_name);

            // Wait for MGMT to be fully ready for tunneling
            if !core.wait_for_mgmt_ready(50).await {
                continue; // Try next port
            }

            // Clear any stale data from buffers after hello exchanges
            core.drain();
            return Some((core, port_name.clone()));
        }
    }

    None
}

/// Prompt user to manually select a port and connect.
fn manually_select_port(baud: u32) -> Result<(Core, String), String> {
    let all_ports: Vec<_> = available_ports().into_iter().map(|p| p.port_name).collect();

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
    let stream =
        open_serial_port(port_name, baud).map_err(|e| format!("Failed to open port: {}", e))?;

    let port = TokioSerialPort::new(stream);
    Ok((new_core(port), port_name.clone()))
}

/// Open a connection to the device
async fn connect(
    port: Option<String>,
    baud: u32,
    auto_detect_link: bool,
) -> Result<(Core, String), Box<dyn std::error::Error>> {
    let delay_ms = |ms| tokio::time::sleep(Duration::from_millis(ms));

    // If user specified a port, connect directly
    if let Some(port_name) = port {
        let stream = open_serial_port(&port_name, baud)?;
        let port = TokioSerialPort::new(stream);
        let mut core = new_core(port);
        core.init_port(delay_ms).await;

        // Clear any stale data from buffers after hello exchanges
        core.drain();

        return Ok((core, port_name));
    }

    if auto_detect_link {
        if let Some((app, port_name)) = find_link_device(baud).await {
            return Ok((app, port_name));
        }
    }

    // Fall back to manual selection (only if stdin is a terminal)
    if !std::io::IsTerminal::is_terminal(&std::io::stdin()) {
        return Err("No Link device found".into());
    }
    let (mut core, port_name) = manually_select_port(baud)?;
    core.init_port(delay_ms).await;

    // Clear any stale data from buffers after hello exchanges
    core.drain();

    Ok((core, port_name))
}

async fn dispatch(cmd: Command, core: &mut Core) -> Result<(), Box<dyn std::error::Error>> {
    match cmd {
        Command::Mgmt { action } => handlers::handle_mgmt(action, core).await,
        Command::Ui { action } => handlers::handle_ui(action, core).await,
        Command::Net { action } => handlers::handle_net(action, core).await,
        Command::Hello => {
            let challenge: [u8; 4] = rand::rng().random();
            println!(
                "Sending hello with challenge: {:02x}{:02x}{:02x}{:02x}",
                challenge[0], challenge[1], challenge[2], challenge[3]
            );
            if core.hello(&challenge).await {
                println!("Hello OK!");
            } else {
                println!("Hello failed!");
            }
            Ok(())
        }
        Command::CircularPing { reverse, data } => {
            if reverse {
                println!("Sending NET-first circular ping with data: {}", data);
                core.net_first_circular_ping(data.as_bytes()).await?;
            } else {
                println!("Sending UI-first circular ping with data: {}", data);
                core.ui_first_circular_ping(data.as_bytes()).await?;
            }
            println!("Completed circular ping!");
            Ok(())
        }
        Command::Exit => {
            std::process::exit(0);
        }
    }
}

fn can_auto_detect_link(cmd: &Command) -> bool {
    !matches!(
        cmd,
        Command::Mgmt {
            action: MgmtAction::Flash { .. }
        }
    )
}

fn mgmt_handler(
    args: ArgMatches,
    core: &mut Core,
) -> Result<Option<String>, reedline_repl_rs::Error> {
    let action = MgmtAction::from_arg_matches(&args)
        .map_err(|e| reedline_repl_rs::Error::UnknownCommand(e.to_string()))?;

    if let Err(e) = tokio::task::block_in_place(|| {
        tokio::runtime::Handle::current().block_on(dispatch(Command::Mgmt { action }, core))
    }) {
        eprintln!("Error: {}", e);
    }
    Ok(None)
}

fn ui_handler(
    args: ArgMatches,
    core: &mut Core,
) -> Result<Option<String>, reedline_repl_rs::Error> {
    let action = UiAction::from_arg_matches(&args)
        .map_err(|e| reedline_repl_rs::Error::UnknownCommand(e.to_string()))?;

    if let Err(e) = tokio::task::block_in_place(|| {
        tokio::runtime::Handle::current().block_on(dispatch(Command::Ui { action }, core))
    }) {
        eprintln!("Error: {}", e);
    }
    Ok(None)
}

fn net_handler(
    args: ArgMatches,
    core: &mut Core,
) -> Result<Option<String>, reedline_repl_rs::Error> {
    let action = NetAction::from_arg_matches(&args)
        .map_err(|e| reedline_repl_rs::Error::UnknownCommand(e.to_string()))?;

    if let Err(e) = tokio::task::block_in_place(|| {
        tokio::runtime::Handle::current().block_on(dispatch(Command::Net { action }, core))
    }) {
        eprintln!("Error: {}", e);
    }
    Ok(None)
}

fn hello_handler(
    _args: ArgMatches,
    core: &mut Core,
) -> Result<Option<String>, reedline_repl_rs::Error> {
    if let Err(e) = tokio::task::block_in_place(|| {
        tokio::runtime::Handle::current().block_on(dispatch(Command::Hello, core))
    }) {
        eprintln!("Error: {}", e);
    }
    Ok(None)
}

fn circular_ping_handler(
    args: ArgMatches,
    core: &mut Core,
) -> Result<Option<String>, reedline_repl_rs::Error> {
    let reverse = args.get_flag("reverse");
    let data = args
        .get_one::<String>("data")
        .cloned()
        .unwrap_or_else(|| "hello".to_string());

    if let Err(e) = tokio::task::block_in_place(|| {
        tokio::runtime::Handle::current()
            .block_on(dispatch(Command::CircularPing { reverse, data }, core))
    }) {
        eprintln!("Error: {}", e);
    }
    Ok(None)
}

fn exit_handler(
    _args: ArgMatches,
    core: &mut Core,
) -> Result<Option<String>, reedline_repl_rs::Error> {
    if let Err(e) = tokio::task::block_in_place(|| {
        tokio::runtime::Handle::current().block_on(dispatch(Command::Exit, core))
    }) {
        eprintln!("Error: {}", e);
    }
    Ok(None)
}

fn run_repl(core: Core, port_name: &str) -> Result<(), reedline_repl_rs::Error> {
    println!("Connected to {} - entering REPL mode", port_name);
    println!("Type 'help' for available commands, 'exit' to quit\n");

    let mut callbacks: CallBackMap<Core, reedline_repl_rs::Error> = CallBackMap::new();
    callbacks.insert("mgmt".to_string(), mgmt_handler);
    callbacks.insert("ui".to_string(), ui_handler);
    callbacks.insert("net".to_string(), net_handler);
    callbacks.insert("hello".to_string(), hello_handler);
    callbacks.insert("circular-ping".to_string(), circular_ping_handler);
    callbacks.insert("exit".to_string(), exit_handler);

    let mut repl = Repl::<Core, reedline_repl_rs::Error>::new(core)
        .with_name("ctl")
        .with_version(env!("CARGO_PKG_VERSION"))
        .with_description("Control interface for the link device")
        .with_banner(&format!("Connected to {}", port_name))
        .with_derived::<ReplCli>(callbacks);

    repl.run()
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();

    match cli.command {
        Some(cmd) => {
            // CLI mode: connect, run one command, exit
            let auto_detect_link = can_auto_detect_link(&cmd);
            let (mut core, port_name) = connect(cli.port, cli.baud, auto_detect_link).await?;
            println!("Connected to {} at {} baud", port_name, cli.baud);

            if let Err(e) = dispatch(cmd, &mut core).await {
                eprintln!("Error: {}", e);
                std::process::exit(1);
            }
        }
        None => {
            // REPL mode: connect once, run commands in loop
            let (core, port_name) = connect(cli.port, cli.baud, true).await?;
            run_repl(core, &port_name)?;
        }
    }

    Ok(())
}

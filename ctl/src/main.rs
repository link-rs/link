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

    Hello,

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
}

#[derive(Debug, Clone, Default, Subcommand)]
enum StackAction {
    /// Get stack usage information
    #[default]
    Info,
    /// Repaint the stack with the known pattern
    Repaint,
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
enum NetLoopbackAction {
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
    core.port_mut().set_timeout(Duration::from_millis(500)).ok()?;

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
        if let Some(app) = try_connect(port_name, baud).await {
            println!("Found Link device on {}", port_name);
            return Some((app, port_name.clone()));
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
) -> Result<(Core, String), Box<dyn std::error::Error>> {
    let delay_ms = |ms| tokio::time::sleep(Duration::from_millis(ms));

    // If user specified a port, connect directly
    if let Some(port_name) = port {
        let stream = open_serial_port(&port_name, baud)?;
        let port = TokioSerialPort::new(stream);
        let mut core = new_core(port);
        core.init_port(delay_ms).await;
        return Ok((core, port_name));
    }

    // Try to find a Link device automatically (init_port called inside try_connect)
    if let Some((app, port_name)) = find_link_device(baud).await {
        return Ok((app, port_name));
    }

    // Fall back to manual selection
    let (mut core, port_name) = manually_select_port(baud)?;
    core.init_port(delay_ms).await;
    Ok((core, port_name))
}

async fn dispatch(cmd: Command, core: &mut Core) -> Result<(), Box<dyn std::error::Error>> {
    match cmd {
        Command::Mgmt { action } => handlers::handle_mgmt(action, core).await,
        Command::Ui { action } => handlers::handle_ui(action, core).await,
        Command::Net { action } => handlers::handle_net(action, core).await,
        Command::Hello => {
            let challenge: [u8; 4] = rand::rng().random();
            println!("Sending hello with challenge: {:02x}{:02x}{:02x}{:02x}", challenge[0], challenge[1], challenge[2], challenge[3]);
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

fn mgmt_handler(
    args: ArgMatches,
    core: &mut Core,
) -> Result<Option<String>, reedline_repl_rs::Error> {
    let action = MgmtAction::from_arg_matches(&args)
        .map_err(|e| reedline_repl_rs::Error::UnknownCommand(e.to_string()))?;

    tokio::task::block_in_place(|| tokio::runtime::Handle::current().block_on(dispatch(Command::Mgmt { action }, core)))
        .map(|()| None)
        .map_err(|e| reedline_repl_rs::Error::UnknownCommand(e.to_string()))
}

fn ui_handler(args: ArgMatches, core: &mut Core) -> Result<Option<String>, reedline_repl_rs::Error> {
    let action = UiAction::from_arg_matches(&args)
        .map_err(|e| reedline_repl_rs::Error::UnknownCommand(e.to_string()))?;

    tokio::task::block_in_place(|| tokio::runtime::Handle::current().block_on(dispatch(Command::Ui { action }, core)))
        .map(|()| None)
        .map_err(|e| reedline_repl_rs::Error::UnknownCommand(e.to_string()))
}

fn net_handler(args: ArgMatches, core: &mut Core) -> Result<Option<String>, reedline_repl_rs::Error> {
    let action = NetAction::from_arg_matches(&args)
        .map_err(|e| reedline_repl_rs::Error::UnknownCommand(e.to_string()))?;

    tokio::task::block_in_place(|| tokio::runtime::Handle::current().block_on(dispatch(Command::Net { action }, core)))
        .map(|()| None)
        .map_err(|e| reedline_repl_rs::Error::UnknownCommand(e.to_string()))
}

fn hello_handler(
    _args: ArgMatches,
    core: &mut Core,
) -> Result<Option<String>, reedline_repl_rs::Error> {
    tokio::task::block_in_place(|| tokio::runtime::Handle::current().block_on(dispatch(Command::Hello, core)))
        .map(|()| None)
        .map_err(|e| reedline_repl_rs::Error::UnknownCommand(e.to_string()))
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

    tokio::task::block_in_place(|| tokio::runtime::Handle::current().block_on(dispatch(Command::CircularPing { reverse, data }, core)))
        .map(|()| None)
        .map_err(|e| reedline_repl_rs::Error::UnknownCommand(e.to_string()))
}

fn exit_handler(
    _args: ArgMatches,
    core: &mut Core,
) -> Result<Option<String>, reedline_repl_rs::Error> {
    tokio::task::block_in_place(|| tokio::runtime::Handle::current().block_on(dispatch(Command::Exit, core)))
        .map(|()| None)
        .map_err(|e| reedline_repl_rs::Error::UnknownCommand(e.to_string()))
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
            let (mut core, port_name) = connect(cli.port, cli.baud).await?;
            println!("Connected to {} at {} baud", port_name, cli.baud);

            if let Err(e) = dispatch(cmd, &mut core).await {
                eprintln!("Error: {}", e);
                std::process::exit(1);
            }
        }
        None => {
            // REPL mode: connect once, run commands in loop
            let (core, port_name) = connect(cli.port, cli.baud).await?;
            run_repl(core, &port_name)?;
        }
    }

    Ok(())
}

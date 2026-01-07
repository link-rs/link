//! CTL - Control interface for the link device
//!
//! CLI mode:  ctl ui ping hello
//! REPL mode: ctl (no args) -> interactive prompt

mod handlers;

use clap::{FromArgMatches, Parser, Subcommand};
use rand::Rng;
use reedline_repl_rs::clap::ArgMatches;
use reedline_repl_rs::{CallBackMap, Repl};
use serialport::SerialPortType;
use std::io::{BufReader, BufWriter, Write};
use std::time::Duration;

type AppReader = BufReader<Box<dyn serialport::SerialPort>>;
type AppWriter = BufWriter<Box<dyn serialport::SerialPort>>;
type App = link::ctl::App<AppReader, AppWriter>;

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
        action: Option<GetSetBool>,
    },

    Reset {
        action: Option<String>,
    },
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
enum GetSetBool {
    #[default]
    Get,
    Set {
        value: bool,
    },
}

#[derive(Debug, Clone, Default, Subcommand)]
enum GetSetString {
    #[default]
    Get,
    Set {
        value: String,
    },
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

        #[arg(short, long, default_value = "0x10000")]
        address: String,

        #[arg(short, long)]
        compress: bool,

        #[arg(long)]
        no_verify: bool,
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

    Loopback {
        #[command(subcommand)]
        action: Option<GetSetBool>,
    },
}

#[derive(Debug, Clone, Subcommand)]
enum WifiAction {
    Add { ssid: String, password: String },
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
    let port = open_serial_port(port_name, baud).ok()?;

    // Set short timeout for hello check
    let port_clone = port.try_clone().ok()?;

    // Create shorter timeout versions for the hello check
    let mut port_read = port;
    let port_write = port_clone;
    port_read.set_timeout(Duration::from_millis(50)).ok()?;

    let reader = BufReader::new(port_read);
    let writer = BufWriter::new(port_write);

    let mut app = link::ctl::App::new(reader, writer);
    let challenge: [u8; 4] = rand::rng().random();

    if app.hello(&challenge) {
        // Restore normal timeout for subsequent operations
        app.reader_mut()
            .inner_mut()
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
    let port_clone = port
        .try_clone()
        .map_err(|e| format!("Failed to clone port: {}", e))?;

    let reader = BufReader::new(port);
    let writer = BufWriter::new(port_clone);

    Ok((link::ctl::App::new(reader, writer), port_name.clone()))
}

/// Open a connection to the device
fn connect(port: Option<String>, baud: u32) -> Result<(App, String), Box<dyn std::error::Error>> {
    // If user specified a port, connect directly
    if let Some(port_name) = port {
        let port = open_serial_port(&port_name, baud)?;
        let port_clone = port.try_clone()?;

        let reader = BufReader::new(port);
        let writer = BufWriter::new(port_clone);

        return Ok((link::ctl::App::new(reader, writer), port_name));
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
                app.net_first_circular_ping(data.as_bytes());
            } else {
                println!("Sending UI-first circular ping with data: {}", data);
                app.ui_first_circular_ping(data.as_bytes());
            }
            println!("Completed circular ping!");
            Ok(())
        }
        Command::Exit => {
            std::process::exit(0);
        }
    }
}

fn repl_handler(
    args: ArgMatches,
    app: &mut App,
) -> Result<Option<String>, reedline_repl_rs::Error> {
    let cmd = Command::from_arg_matches(&args)
        .map_err(|e| reedline_repl_rs::Error::UnknownCommand(e.to_string()))?;

    dispatch(cmd, app)
        .map(|()| None)
        .map_err(|e| reedline_repl_rs::Error::UnknownCommand(e.to_string()))
}

fn run_repl(app: App, port_name: &str) -> Result<(), reedline_repl_rs::Error> {
    println!("Connected to {} - entering REPL mode", port_name);
    println!("Type 'help' for available commands, 'exit' to quit\n");

    let mut callbacks: CallBackMap<App, reedline_repl_rs::Error> = CallBackMap::new();
    callbacks.insert("mgmt".to_string(), repl_handler);
    callbacks.insert("ui".to_string(), repl_handler);
    callbacks.insert("net".to_string(), repl_handler);
    callbacks.insert("circular-ping".to_string(), repl_handler);
    callbacks.insert("exit".to_string(), repl_handler);

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

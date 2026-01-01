//! CTL - Control interface for the link device
//!
//! CLI mode:  ctl ui ping hello
//! REPL mode: ctl (no args) -> interactive prompt

mod handlers;

use clap::{FromArgMatches, Parser, Subcommand};
use embedded_io_adapters::tokio_1::FromTokio;
use rand::Rng;
use reedline_repl_rs::clap::ArgMatches;
use reedline_repl_rs::{AsyncCallBackMap, Repl};
use serialport::SerialPortType;
use std::future::Future;
use std::io::Write;
use std::pin::Pin;
use std::time::Duration;
use tokio::io::{ReadHalf, WriteHalf};
use tokio_serial::{SerialPortBuilderExt, SerialStream};

type AppWriter = FromTokio<WriteHalf<SerialStream>>;
type AppReader = FromTokio<ReadHalf<SerialStream>>;
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

/// Try to connect to a specific port and verify it's a Link device.
/// Returns the App if successful, None if connection failed or not a Link device.
async fn try_connect(port_name: &str, baud: u32) -> Option<App> {
    let port = tokio_serial::new(port_name, baud)
        .parity(tokio_serial::Parity::Even)
        .open_native_async()
        .ok()?;

    let (reader, writer) = tokio::io::split(port);
    let reader = FromTokio::new(reader);
    let writer = FromTokio::new(writer);

    let mut app = link::ctl::App::new(reader, writer);
    let challenge: [u8; 4] = rand::rng().random();

    match tokio::time::timeout(Duration::from_millis(50), app.hello(&challenge)).await {
        Ok(true) => Some(app),
        _ => None,
    }
}

/// Find a Link device among available ports and return a connected App.
async fn find_link_device(baud: u32) -> Option<(App, String)> {
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
        if let Some(app) = try_connect(port_name, baud).await {
            println!("Found Link device on {}", port_name);
            return Some((app, port_name.clone()));
        }
    }

    None
}

/// Prompt user to manually select a port and connect.
async fn manually_select_port(baud: u32) -> Result<(App, String), String> {
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
    let serial_port = tokio_serial::new(port_name, baud)
        .parity(tokio_serial::Parity::Even)
        .open_native_async()
        .map_err(|e| format!("Failed to open port: {}", e))?;

    let (reader, writer) = tokio::io::split(serial_port);
    let reader = FromTokio::new(reader);
    let writer = FromTokio::new(writer);

    Ok((link::ctl::App::new(reader, writer), port_name.clone()))
}

/// Open a connection to the device
async fn connect(
    port: Option<String>,
    baud: u32,
) -> Result<(App, String), Box<dyn std::error::Error>> {
    // If user specified a port, connect directly
    if let Some(port_name) = port {
        let serial_port = tokio_serial::new(&port_name, baud)
            .parity(tokio_serial::Parity::Even)
            .open_native_async()?;

        let (reader, writer) = tokio::io::split(serial_port);
        let reader = FromTokio::new(reader);
        let writer = FromTokio::new(writer);

        return Ok((link::ctl::App::new(reader, writer), port_name));
    }

    // Try to find a Link device automatically
    if let Some((app, port_name)) = find_link_device(baud).await {
        return Ok((app, port_name));
    }

    // Fall back to manual selection
    manually_select_port(baud).await.map_err(|e| e.into())
}

async fn dispatch(cmd: Command, app: &mut App) -> Result<(), Box<dyn std::error::Error>> {
    match cmd {
        Command::Mgmt { action } => handlers::handle_mgmt(action, app).await,
        Command::Ui { action } => handlers::handle_ui(action, app).await,
        Command::Net { action } => handlers::handle_net(action, app).await,
        Command::CircularPing { reverse, data } => {
            if reverse {
                println!("Sending NET-first circular ping with data: {}", data);
                app.net_first_circular_ping(data.as_bytes()).await;
            } else {
                println!("Sending UI-first circular ping with data: {}", data);
                app.ui_first_circular_ping(data.as_bytes()).await;
            }
            println!("Completed circular ping!");
            Ok(())
        }
        Command::Exit => {
            std::process::exit(0);
        }
    }
}

fn repl_handler<'a>(
    args: ArgMatches,
    app: &'a mut App,
) -> Pin<Box<dyn Future<Output = Result<Option<String>, reedline_repl_rs::Error>> + 'a>> {
    Box::pin(async move {
        let cmd = Command::from_arg_matches(&args)
            .map_err(|e| reedline_repl_rs::Error::UnknownCommand(e.to_string()))?;

        dispatch(cmd, app)
            .await
            .map(|()| None)
            .map_err(|e| reedline_repl_rs::Error::UnknownCommand(e.to_string()))
    })
}

async fn run_repl(app: App, port_name: &str) -> Result<(), reedline_repl_rs::Error> {
    println!("Connected to {} - entering REPL mode", port_name);
    println!("Type 'help' for available commands, 'exit' to quit\n");

    let mut callbacks: AsyncCallBackMap<App, reedline_repl_rs::Error> = AsyncCallBackMap::new();
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
        .with_async_derived::<ReplCli>(callbacks);

    repl.run_async().await
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();

    match cli.command {
        Some(cmd) => {
            // CLI mode: connect, run one command, exit
            let (mut app, port_name) = connect(cli.port, cli.baud).await?;
            println!("Connected to {} at {} baud", port_name, cli.baud);

            if let Err(e) = dispatch(cmd, &mut app).await {
                eprintln!("Error: {}", e);
                std::process::exit(1);
            }
        }
        None => {
            // REPL mode: connect once, run commands in loop
            let (app, port_name) = connect(cli.port, cli.baud).await?;
            run_repl(app, &port_name).await?;
        }
    }

    Ok(())
}

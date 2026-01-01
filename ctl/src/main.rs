//! CTL - Control interface for the link device
//!
//! CLI mode:  ctl ui ping hello
//! REPL mode: ctl (no args) -> interactive prompt

mod handlers;

use clap::{FromArgMatches, Parser, Subcommand};
use embedded_io_adapters::tokio_1::FromTokio;
use indicatif::{ProgressBar, ProgressStyle};
use link::ctl::FlashPhase;
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

/// Select a port name (for bootloader handlers that do their own connection).
/// This is a compatibility shim - bootloader handlers should be refactored.
async fn select_port_name(specified: Option<String>) -> Result<String, String> {
    if let Some(port) = specified {
        return Ok(port);
    }

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

    if all_ports.len() == 1 {
        println!("Auto-selected port: {}", all_ports[0]);
        return Ok(all_ports[0].clone());
    }

    println!("Available USB serial ports:");
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

    Ok(all_ports[choice - 1].clone())
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

async fn dispatch(
    cmd: Command,
    app: &mut App,
) -> Result<Option<String>, Box<dyn std::error::Error>> {
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
            Ok(Some("Completed circular ping!".to_string()))
        }
        Command::Exit => {
            std::process::exit(0);
        }
    }
}

// CLAUDE I don't think these need to make their own serial connections.  The port configuration is
// the same in bootloader or non-bootloader mode.  Move these into the normal mgmt handler.

async fn handle_mgmt_info(port: Option<String>) -> Result<(), Box<dyn std::error::Error>> {
    println!("MGMT Bootloader Info");
    println!("====================\n");

    let port_name = select_port_name(port).await?;

    println!("\nTo read bootloader information, the MGMT chip must be in bootloader mode.");
    println!("Please follow these steps:");
    println!("  1. Set the BOOT0 pin high on the MGMT chip");
    println!("  2. Reset the MGMT chip");
    println!();
    print!("Press Enter when ready (or Ctrl+C to cancel)... ");
    std::io::stdout().flush()?;

    let mut input = String::new();
    std::io::stdin().read_line(&mut input)?;

    println!("Opening {} at 115200 baud with even parity...", port_name);
    let port = tokio_serial::new(&port_name, 115200)
        .parity(tokio_serial::Parity::Even)
        .open_native_async()?;

    let (reader, writer) = tokio::io::split(port);
    let reader = FromTokio::new(reader);
    let writer = FromTokio::new(writer);

    let mut app = link::ctl::App::new(reader, writer);

    println!("Querying bootloader information...\n");

    let Ok(info) = app.get_mgmt_bootloader_info().await else {
        eprintln!("Failed to get bootloader info");
        eprintln!("\nMake sure the MGMT chip is in bootloader mode:");
        eprintln!("  1. Set BOOT0 pin high");
        eprintln!("  2. Reset the device");
        return Err("Bootloader communication failed".into());
    };

    let major = info.bootloader_version >> 4;
    let minor = info.bootloader_version & 0x0F;
    println!(
        "Bootloader Version: {}.{} (0x{:02X})",
        major, minor, info.bootloader_version
    );
    println!(
        "Chip ID: 0x{:04X} ({})",
        info.chip_id,
        bootloader::stm::chip_name(info.chip_id)
    );

    println!("\nSupported Commands ({}):", info.command_count);
    for i in 0..info.command_count {
        let cmd = info.commands[i];
        println!("  0x{:02X} - {}", cmd, bootloader::stm::command_name(cmd));
    }

    if let Some(flash) = info.flash_sample {
        println!("\nFlash Memory Sample (0x08000000):");
        print!("  ");
        for (i, byte) in flash.iter().enumerate() {
            print!("{:02X} ", byte);
            if (i + 1) % 16 == 0 && i + 1 < flash.len() {
                print!("\n  ");
            }
        }
        println!();

        let sp = u32::from_le_bytes([flash[0], flash[1], flash[2], flash[3]]);
        let reset = u32::from_le_bytes([flash[4], flash[5], flash[6], flash[7]]);

        println!("\nVector Table Analysis:");
        println!("  Initial SP:      0x{:08X}", sp);
        println!("  Reset Handler:   0x{:08X}", reset);

        if (0x2000_0000..0x2002_0000).contains(&sp) {
            println!("  (SP appears valid - points to SRAM)");
        }
        if (0x0800_0000..0x0810_0000).contains(&reset) && (reset & 1) == 1 {
            println!("  (Reset handler appears valid - points to Flash, Thumb mode)");
        }
    } else {
        println!("\nFlash Memory: Could not read (read protection may be enabled)");
    }

    println!("\nDone!");
    Ok(())
}

async fn handle_mgmt_flash(
    port: Option<String>,
    file: std::path::PathBuf,
) -> Result<(), Box<dyn std::error::Error>> {
    println!("MGMT Flash");
    println!("==========\n");

    let firmware = std::fs::read(&file)?;
    println!("Firmware: {} ({} bytes)", file.display(), firmware.len());

    let port_name = select_port_name(port).await?;

    println!("\nTo flash the MGMT chip, it must be in bootloader mode.");
    println!("Please follow these steps:");
    println!("  1. Set the BOOT0 pin high on the MGMT chip");
    println!("  2. Reset the MGMT chip");
    println!();
    print!("Press Enter when ready (or Ctrl+C to cancel)... ");
    std::io::stdout().flush()?;

    let mut input = String::new();
    std::io::stdin().read_line(&mut input)?;

    println!("Opening {} at 115200 baud with even parity...", port_name);
    let port = tokio_serial::new(&port_name, 115200)
        .parity(tokio_serial::Parity::Even)
        .open_native_async()?;

    let (reader, writer) = tokio::io::split(port);
    let reader = FromTokio::new(reader);
    let writer = FromTokio::new(writer);

    let mut app = link::ctl::App::new(reader, writer);

    let pb = ProgressBar::new(firmware.len() as u64);
    let bytes_style = ProgressStyle::default_bar()
        .template("{prefix:>12} [{bar:40.cyan/blue}] {bytes}/{total_bytes} ({percent}%)")
        .unwrap()
        .progress_chars("#>-");
    let pages_style = ProgressStyle::default_bar()
        .template("{prefix:>12} [{bar:40.cyan/blue}] {pos}/{len} pages ({percent}%)")
        .unwrap()
        .progress_chars("#>-");
    pb.set_style(pages_style.clone());

    let mut current_phase = None;
    let result = app
        .flash_mgmt(&firmware, |phase, progress, total| {
            if current_phase != Some(phase) {
                current_phase = Some(phase);
                match phase {
                    FlashPhase::Compressing => {}
                    FlashPhase::Erasing => {
                        pb.set_style(pages_style.clone());
                        pb.set_prefix("Erasing");
                    }
                    FlashPhase::Writing => {
                        pb.set_style(bytes_style.clone());
                        pb.set_prefix("Writing");
                    }
                    FlashPhase::Verifying => {
                        pb.set_style(bytes_style.clone());
                        pb.set_prefix("Verifying");
                    }
                }
                pb.set_length(total as u64);
                pb.set_position(0);
            }
            pb.set_position(progress as u64);
        })
        .await;

    pb.finish_and_clear();

    match result {
        Ok(()) => {
            println!("\nFlash complete!");
            println!("The MGMT chip should now be running the new firmware.");
            println!("\nNote: Set BOOT0 low and reset to ensure normal boot on next power cycle.");
        }
        Err(e) => {
            eprintln!("\nFlash failed: {:?}", e);
            eprintln!("\nMake sure the MGMT chip is in bootloader mode:");
            eprintln!("  1. Set BOOT0 pin high");
            eprintln!("  2. Reset the device");
            return Err("Flash failed".into());
        }
    }

    Ok(())
}

fn repl_handler<'a>(
    args: ArgMatches,
    app: &'a mut App,
) -> Pin<Box<dyn Future<Output = Result<Option<String>, reedline_repl_rs::Error>> + 'a>> {
    Box::pin(async move {
        let cmd = Command::from_arg_matches(&args)
            .map_err(|e| reedline_repl_rs::Error::UnknownCommand(e.to_string()))?;

        match dispatch(cmd, app).await {
            Ok(output) => Ok(output),
            Err(e) => Err(reedline_repl_rs::Error::UnknownCommand(e.to_string())),
        }
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

    // Handle special bootloader commands (they create their own connection)
    if let Some(Command::Mgmt {
        action: MgmtAction::Info,
    }) = &cli.command
    {
        return handle_mgmt_info(cli.port).await;
    }
    if let Some(Command::Mgmt {
        action: MgmtAction::Flash { file },
    }) = &cli.command
    {
        return handle_mgmt_flash(cli.port, file.clone()).await;
    }

    match cli.command {
        Some(cmd) => {
            // CLI mode: connect, run one command, exit
            let (mut app, port_name) = connect(cli.port, cli.baud).await?;
            println!("Connected to {} at {} baud", port_name, cli.baud);

            match dispatch(cmd, &mut app).await {
                Ok(Some(output)) => println!("{}", output),
                Ok(None) => {}
                Err(e) => {
                    eprintln!("Error: {}", e);
                    std::process::exit(1);
                }
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

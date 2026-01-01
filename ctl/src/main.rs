//! CTL - Control interface for the link device
//!
//! CLI mode:  ctl ui ping hello
//! REPL mode: ctl (no args) -> interactive prompt

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
        Command::Mgmt { action } => handle_mgmt(action, app).await,
        Command::Ui { action } => handle_ui(action, app).await,
        Command::Net { action } => handle_net(action, app).await,
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

// CLAUDE I don't like the level of nesting in these handle_x functions.  Let's make some modules
// `mgmt`, `ui`, `net`; put the specific handler logic in free functions there, and call the
// those functions from the handler functions.

// CLAUDE There's a mix in here between the handler function returning the string to print vs just
// calling println itself.  Let's normalize on the latter, and return Result<(), Box<dyn Error>>.

async fn handle_mgmt(
    action: MgmtAction,
    app: &mut App,
) -> Result<Option<String>, Box<dyn std::error::Error>> {
    match action {
        MgmtAction::Ping { data } => {
            println!("Sending MGMT ping with data: {}", data);
            app.mgmt_ping(data.as_bytes()).await;
            Ok(Some("Received pong!".to_string()))
        }
        MgmtAction::Info => {
            Err("mgmt info requires bootloader mode - run as: ctl mgmt info".into())
        }
        MgmtAction::Flash { .. } => {
            Err("mgmt flash requires bootloader mode - run as: ctl mgmt flash <file>".into())
        }
    }
}

async fn handle_ui(
    action: UiAction,
    app: &mut App,
) -> Result<Option<String>, Box<dyn std::error::Error>> {
    match action {
        UiAction::Ping { data } => {
            println!("Sending UI ping with data: {}", data);
            app.ui_ping(data.as_bytes()).await;
            Ok(Some("Received pong!".to_string()))
        }
        UiAction::Info => {
            println!("UI Bootloader Info");
            println!("==================\n");
            println!("Resetting UI chip to bootloader mode...");

            let delay = |ms| tokio::time::sleep(std::time::Duration::from_millis(ms));
            let info = app
                .get_ui_bootloader_info(delay)
                .await
                .map_err(|_| "Failed to get bootloader info")?;

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

            Ok(Some("UI chip reset back to user mode.\nDone!".to_string()))
        }
        UiAction::Flash { file } => {
            println!("UI Flash");
            println!("========\n");

            let firmware = std::fs::read(&file)?;
            println!("Firmware: {} ({} bytes)", file.display(), firmware.len());
            println!("Resetting UI chip to bootloader mode...\n");

            let pb = ProgressBar::new(firmware.len() as u64);
            let bytes_style = ProgressStyle::default_bar()
                .template("{prefix:>12} [{bar:40.cyan/blue}] {bytes}/{total_bytes} ({percent}%)")
                .unwrap()
                .progress_chars("#>-");
            let sectors_style = ProgressStyle::default_bar()
                .template("{prefix:>12} [{bar:40.cyan/blue}] {pos}/{len} sectors ({percent}%)")
                .unwrap()
                .progress_chars("#>-");
            pb.set_style(sectors_style.clone());

            let mut current_phase = None;
            let delay = |ms| tokio::time::sleep(std::time::Duration::from_millis(ms));
            let result = app
                .flash_ui(&firmware, delay, |phase, progress, total| {
                    if current_phase != Some(phase) {
                        current_phase = Some(phase);
                        match phase {
                            FlashPhase::Compressing => {}
                            FlashPhase::Erasing => {
                                pb.set_style(sectors_style.clone());
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
                Ok(()) => Ok(Some(
                    "Flash complete!\nUI chip reset back to user mode.".to_string(),
                )),
                Err(e) => Err(format!("Flash failed: {:?}", e).into()),
            }
        }
        UiAction::Version { action } => match action.unwrap_or_default() {
            GetSetU32::Get => {
                let version = app.get_version().await;
                Ok(Some(format!("{}", version)))
            }
            GetSetU32::Set { value } => {
                app.set_version(value).await;
                Ok(Some(format!("Version set to {}", value)))
            }
        },
        UiAction::SFrameKey { action } => match action.unwrap_or_default() {
            GetSetHex::Get => {
                let key = app.get_sframe_key().await;
                Ok(Some(hex::encode(key)))
            }
            GetSetHex::Set { value } => {
                let key_bytes = hex::decode(&value).map_err(|_| "Invalid hex string")?;
                if key_bytes.len() != 16 {
                    return Err("SFrame key must be exactly 32 hex characters (16 bytes)".into());
                }
                let mut key_array = [0u8; 16];
                key_array.copy_from_slice(&key_bytes);
                app.set_sframe_key(&key_array).await;
                Ok(Some(format!("SFrame key set to {}", value)))
            }
        },
        UiAction::Loopback { action } => match action.unwrap_or_default() {
            GetSetBool::Get => {
                let enabled = app.ui_get_loopback().await;
                Ok(Some(format!("{}", enabled)))
            }
            GetSetBool::Set { value } => {
                app.ui_set_loopback(value).await;
                Ok(Some(format!("UI loopback set to {}", value)))
            }
        },
        UiAction::Reset { action } => match action.as_deref() {
            Some("hold") => {
                app.hold_ui_reset().await;
                Ok(Some("UI chip held in reset".to_string()))
            }
            Some("release") => {
                app.reset_ui_to_user().await;
                Ok(Some("UI chip released from reset".to_string()))
            }
            _ => {
                app.reset_ui_to_user().await;
                Ok(Some("UI chip reset".to_string()))
            }
        },
    }
}

async fn handle_net(
    action: NetAction,
    app: &mut App,
) -> Result<Option<String>, Box<dyn std::error::Error>> {
    match action {
        NetAction::Ping { data } => {
            println!("Sending NET ping with data: {}", data);
            app.net_ping(data.as_bytes()).await;
            Ok(Some("Received pong!".to_string()))
        }
        NetAction::Info => {
            println!("Resetting NET chip to bootloader mode...");
            let info = app
                .get_net_bootloader_info()
                .await
                .map_err(|e| format!("Failed to get bootloader info: {:?}", e))?;

            let sec = &info.security_info;
            println!("\nNET Bootloader Info");
            println!("===================\n");
            println!("Chip Type:         {}", sec.chip_type.name());
            println!("Chip ID:           {} (0x{:04X})", sec.chip_id, sec.chip_id);
            println!("Security Flags:    0x{:08X}", sec.flags);
            println!("Flash Crypt Count: {}", sec.flash_crypt_cnt);
            println!(
                "Key Purposes:      {:02X} {:02X} {:02X} {:02X} {:02X} {:02X} {:02X}",
                sec.key_purposes[0],
                sec.key_purposes[1],
                sec.key_purposes[2],
                sec.key_purposes[3],
                sec.key_purposes[4],
                sec.key_purposes[5],
                sec.key_purposes[6]
            );

            Ok(Some("NET chip reset back to user mode.\nDone!".to_string()))
        }
        NetAction::Wifi { action } => match action {
            None => {
                let ssids = app.get_wifi_ssids().await;
                if ssids.is_empty() {
                    Ok(Some("No WiFi networks configured".to_string()))
                } else {
                    let mut output = String::new();
                    for wifi in ssids {
                        output.push_str(&format!("{}\t{}\n", wifi.ssid, wifi.password));
                    }
                    Ok(Some(output.trim_end().to_string()))
                }
            }
            Some(WifiAction::Add { ssid, password }) => {
                app.add_wifi_ssid(&ssid, &password).await;
                Ok(Some(format!("Added WiFi network: {}", ssid)))
            }
            Some(WifiAction::Clear) => {
                app.clear_wifi_ssids().await;
                Ok(Some("Cleared all WiFi networks".to_string()))
            }
        },
        NetAction::RelayUrl { action } => match action.unwrap_or_default() {
            GetSetString::Get => {
                let url = app.get_relay_url().await;
                Ok(Some(url.to_string()))
            }
            GetSetString::Set { value } => {
                app.set_relay_url(&value).await;
                Ok(Some(format!("Relay URL set to {}", value)))
            }
        },
        NetAction::Flash {
            file,
            address,
            compress,
            no_verify,
        } => {
            println!("NET Flash (ESP32)");
            println!("=================\n");

            let address: u32 = if address.starts_with("0x") || address.starts_with("0X") {
                u32::from_str_radix(&address[2..], 16).map_err(|_| "Invalid hex address")?
            } else {
                address.parse().map_err(|_| "Invalid address")?
            };

            if address == 0x10000 {
                println!("Note: Using default app address 0x10000 (standard ESP-IDF layout)");
                println!("      Use --address to override if needed.\n");
            }

            if compress {
                println!("Mode: Compressed transfer enabled (-c)\n");
            }

            if no_verify {
                println!("Note: MD5 verification disabled (--no-verify)\n");
            }

            let firmware = std::fs::read(&file)?;
            println!("Firmware: {} ({} bytes)", file.display(), firmware.len());
            println!("Flash address: 0x{:08X}", address);
            println!("Resetting NET chip to bootloader mode...\n");

            let pb = ProgressBar::new(firmware.len() as u64);
            let bytes_style = ProgressStyle::default_bar()
                .template("{prefix:>12} [{bar:40.cyan/blue}] {bytes}/{total_bytes} ({percent}%)")
                .unwrap()
                .progress_chars("#>-");
            let erase_style = ProgressStyle::default_bar()
                .template("{prefix:>12} [{bar:40.cyan/blue}] {pos}/{len} ({percent}%)")
                .unwrap()
                .progress_chars("#>-");
            pb.set_style(erase_style.clone());

            let mut current_phase = None;
            let verify = !no_verify;
            let result = app
                .flash_net(
                    &firmware,
                    address,
                    compress,
                    verify,
                    |phase, progress, total| {
                        if current_phase != Some(phase) {
                            current_phase = Some(phase);
                            match phase {
                                FlashPhase::Compressing => {
                                    pb.set_style(bytes_style.clone());
                                    pb.set_prefix("Compressing");
                                }
                                FlashPhase::Erasing => {
                                    pb.set_style(erase_style.clone());
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
                    },
                )
                .await;

            pb.finish_and_clear();

            match result {
                Ok(()) => Ok(Some(
                    "Flash complete!\nNET chip reset back to user mode.".to_string(),
                )),
                Err(e) => Err(format!("Flash failed: {:?}", e).into()),
            }
        }
        NetAction::WsPing { data } => {
            println!("Sending WebSocket ping with data: {}", data);
            app.ws_ping(data.as_bytes()).await;
            Ok(Some("Received echo response!".to_string()))
        }
        NetAction::WsEchoTest => {
            println!("Running WebSocket echo test...");
            println!("  Sending 50 packets (640 bytes each) at 20ms intervals (50 fps)\n");

            let results = app.ws_echo_test().await;

            let mut output = String::new();
            output.push_str("Results:\n");
            output.push_str(&format!("  Packets sent:           {}\n", results.sent));
            output.push_str(&format!("  Packets received (raw): {}\n", results.received));
            output.push_str(&format!(
                "  Packets output (buf):   {}\n",
                results.buffered_output
            ));
            output.push_str(&format!(
                "  Buffer underruns:       {}\n",
                results.underruns
            ));

            if results.received > 0 && results.sent > 0 {
                let loss_pct =
                    ((results.sent - results.received) as f64 / results.sent as f64) * 100.0;
                output.push_str(&format!("  Packet loss:            {:.1}%\n", loss_pct));
            }

            fn format_jitter_stats(label: &str, timings: &[u32]) -> String {
                if timings.is_empty() {
                    return format!("\n{}: No data\n", label);
                }
                let min = timings.iter().min().copied().unwrap_or(0);
                let max = timings.iter().max().copied().unwrap_or(0);
                let sum: u64 = timings.iter().map(|&x| x as u64).sum();
                let avg = sum / timings.len() as u64;

                let mut s = format!("\n{}:\n", label);
                s.push_str(&format!(
                    "  Min: {:>6} µs ({:>5.1} ms)\n",
                    min,
                    min as f64 / 1000.0
                ));
                s.push_str(&format!(
                    "  Max: {:>6} µs ({:>5.1} ms)\n",
                    max,
                    max as f64 / 1000.0
                ));
                s.push_str(&format!(
                    "  Avg: {:>6} µs ({:>5.1} ms)\n",
                    avg,
                    avg as f64 / 1000.0
                ));

                let target_us = 20000i64;
                let jitter: i64 = timings
                    .iter()
                    .map(|&x| (x as i64 - target_us).abs())
                    .sum::<i64>()
                    / timings.len() as i64;
                s.push_str(&format!(
                    "  Avg deviation from 20ms: {:>6} µs ({:>5.1} ms)\n",
                    jitter,
                    jitter as f64 / 1000.0
                ));
                s
            }

            output.push_str(&format_jitter_stats(
                "Raw jitter (before buffer)",
                results.raw_jitter_us.as_slice(),
            ));
            output.push_str(&format_jitter_stats(
                "Buffered jitter (after buffer)",
                results.buffered_jitter_us.as_slice(),
            ));

            if !results.raw_jitter_us.is_empty() {
                output.push_str(&format!(
                    "\nRaw timings (µs): {:?}\n",
                    results.raw_jitter_us.as_slice()
                ));
            }
            if !results.buffered_jitter_us.is_empty() {
                output.push_str(&format!(
                    "Buffered timings (µs): {:?}",
                    results.buffered_jitter_us.as_slice()
                ));
            }

            Ok(Some(output))
        }
        NetAction::WsSpeedTest => {
            println!("Running WebSocket speed test...");
            println!("  Sending 50 packets (640 bytes each) as fast as possible\n");

            let results = app.ws_speed_test().await;

            let mut output = String::new();
            output.push_str("Results:\n");
            output.push_str(&format!("  Packets sent:     {}\n", results.sent));
            output.push_str(&format!("  Packets received: {}\n", results.received));
            output.push_str(&format!(
                "  Send time:        {} ms\n",
                results.send_time_ms
            ));
            output.push_str(&format!(
                "  Receive time:     {} ms\n",
                results.recv_time_ms
            ));

            if results.sent > 0 {
                let send_rate =
                    (results.sent as f64 * 640.0) / (results.send_time_ms as f64 / 1000.0) / 1024.0;
                output.push_str(&format!("  Send rate:        {:.1} KB/s\n", send_rate));
                let fps = results.sent as f64 / (results.send_time_ms as f64 / 1000.0);
                output.push_str(&format!("  Send FPS:         {:.1}\n", fps));
            }

            if results.received > 0 && results.sent > 0 {
                let loss_pct =
                    ((results.sent - results.received) as f64 / results.sent as f64) * 100.0;
                output.push_str(&format!("  Packet loss:      {:.1}%", loss_pct));
            }

            Ok(Some(output))
        }
        NetAction::Loopback { action } => match action.unwrap_or_default() {
            GetSetBool::Get => {
                let enabled = app.net_get_loopback().await;
                Ok(Some(format!("{}", enabled)))
            }
            GetSetBool::Set { value } => {
                app.net_set_loopback(value).await;
                Ok(Some(format!("NET loopback set to {}", value)))
            }
        },
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

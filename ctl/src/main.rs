use clap::{Parser, Subcommand};
use embedded_io_adapters::tokio_1::FromTokio;
use indicatif::{ProgressBar, ProgressStyle};
use link::ctl::FlashPhase;
use rand::Rng;
use serialport::SerialPortType;
use std::io::Write;
use std::time::Duration;
use tokio_serial::SerialPortBuilderExt;

#[derive(Parser)]
#[command(name = "ctl")]
#[command(about = "Control interface for the link device", long_about = None)]
struct Cli {
    /// Serial port to use (auto-detected if not specified)
    #[arg(short, long)]
    port: Option<String>,

    #[arg(short, long, default_value = "115200")]
    baud: u32,

    #[command(subcommand)]
    command: Command,
}

/// Test if a serial port has a valid Link device connected.
/// Sends a Hello handshake with a random challenge and verifies the response.
async fn test_link_device(port_name: &str, baud: u32) -> bool {
    // Open port with timeout
    let port = match tokio_serial::new(port_name, baud)
        .parity(tokio_serial::Parity::Even)
        .open_native_async()
    {
        Ok(p) => p,
        Err(_) => return false,
    };

    let (reader, writer) = tokio::io::split(port);
    let reader = FromTokio::new(reader);
    let writer = FromTokio::new(writer);

    let mut app = link::ctl::App::new(writer, reader);

    // Generate random 4-byte challenge
    let challenge: [u8; 4] = rand::rng().random();

    // Try Hello handshake with 50ms timeout
    match tokio::time::timeout(Duration::from_millis(50), app.hello(&challenge)).await {
        Ok(result) => result,
        Err(_) => false, // Timeout
    }
}

/// Select a serial port, optionally filtering by valid Link devices.
async fn select_port(specified: Option<String>, baud: u32) -> Result<String, String> {
    if let Some(port) = specified {
        return Ok(port);
    }

    // Find candidate serial ports
    let all_ports: Vec<_> = serialport::available_ports()
        .unwrap_or_default()
        .into_iter()
        .filter(|p| matches!(p.port_type, SerialPortType::UsbPort(_)))
        .map(|p| p.port_name)
        // Filter out /dev/tty.* ports on macOS (prefer /dev/cu.* for outgoing connections)
        .filter(|name| !name.starts_with("/dev/tty."))
        .collect();

    if all_ports.is_empty() {
        return Err("No USB serial ports found".to_string());
    } else if all_ports.len() == 1 {
        println!("Auto-selected port: {}", all_ports[0]);
        return Ok(all_ports[0].clone());
    }

    // Test all ports in parallel for valid Link devices
    println!("Scanning for Link devices...");
    let futures: Vec<_> = all_ports
        .iter()
        .map(|port| {
            let port = port.clone();
            async move {
                let is_valid = test_link_device(&port, baud).await;
                (port, is_valid)
            }
        })
        .collect();

    let results = futures::future::join_all(futures).await;
    let valid_ports: Vec<String> = results
        .into_iter()
        .filter_map(|(port, is_valid)| if is_valid { Some(port) } else { None })
        .collect();

    // If we found valid devices, use those; otherwise fall back to all USB ports
    let ports = if valid_ports.is_empty() {
        println!("No Link devices detected, showing all USB serial ports");
        all_ports
    } else {
        valid_ports
    };

    match ports.len() {
        0 => Err("No USB serial ports found".to_string()),
        1 => {
            println!("Auto-selected port: {}", ports[0]);
            Ok(ports[0].clone())
        }
        _ => {
            println!("Multiple ports found:");
            for (i, port) in ports.iter().enumerate() {
                println!("  {}: {}", i + 1, port);
            }
            print!("Select port [1-{}] (default: 1): ", ports.len());
            std::io::stdout().flush().unwrap();

            let mut input = String::new();
            std::io::stdin()
                .read_line(&mut input)
                .map_err(|e| format!("Failed to read input: {}", e))?;

            let trimmed = input.trim();
            let choice: usize = if trimmed.is_empty() {
                1 // Default to first port
            } else {
                trimmed.parse().map_err(|_| "Invalid number".to_string())?
            };

            if choice < 1 || choice > ports.len() {
                return Err(format!(
                    "Please select a number between 1 and {}",
                    ports.len()
                ));
            }

            Ok(ports[choice - 1].clone())
        }
    }
}

/// Handle the `mgmt info` command which requires bootloader mode
async fn handle_mgmt_info(port: Option<String>) -> Result<(), Box<dyn std::error::Error>> {
    println!("MGMT Bootloader Info");
    println!("====================\n");

    let port_name = select_port(port, 115200).await?;

    println!("\nTo read bootloader information, the MGMT chip must be in bootloader mode.");
    println!("Please follow these steps:");
    println!("  1. Set the BOOT0 pin high on the MGMT chip");
    println!("  2. Reset the MGMT chip");
    println!();
    print!("Press Enter when ready (or Ctrl+C to cancel)... ");
    std::io::stdout().flush()?;

    let mut input = String::new();
    std::io::stdin().read_line(&mut input)?;

    // Open serial port at 115200 baud with even parity
    println!("Opening {} at 115200 baud with even parity...", port_name);
    let port = tokio_serial::new(&port_name, 115200)
        .parity(tokio_serial::Parity::Even)
        .open_native_async()?;

    // Split into read/write halves and wrap for embedded-io-async
    let (reader, writer) = tokio::io::split(port);
    let reader = FromTokio::new(reader);
    let writer = FromTokio::new(writer);

    let mut app = link::ctl::App::new(writer, reader);

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
        chip_name(info.chip_id)
    );

    println!("\nSupported Commands ({}):", info.command_count);
    for i in 0..info.command_count {
        let cmd = info.commands[i];
        println!("  0x{:02X} - {}", cmd, command_name(cmd));
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

        // Analyze vector table
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

/// Handle the `mgmt flash` command which requires bootloader mode
async fn handle_mgmt_flash(
    port: Option<String>,
    file: std::path::PathBuf,
) -> Result<(), Box<dyn std::error::Error>> {
    println!("MGMT Flash");
    println!("==========\n");

    // Read the firmware file
    let firmware = std::fs::read(&file)?;
    println!("Firmware: {} ({} bytes)", file.display(), firmware.len());

    let port_name = select_port(port, 115200).await?;

    println!("\nTo flash the MGMT chip, it must be in bootloader mode.");
    println!("Please follow these steps:");
    println!("  1. Set the BOOT0 pin high on the MGMT chip");
    println!("  2. Reset the MGMT chip");
    println!();
    print!("Press Enter when ready (or Ctrl+C to cancel)... ");
    std::io::stdout().flush()?;

    let mut input = String::new();
    std::io::stdin().read_line(&mut input)?;

    // Open serial port at 115200 baud with even parity
    println!("Opening {} at 115200 baud with even parity...", port_name);
    let port = tokio_serial::new(&port_name, 115200)
        .parity(tokio_serial::Parity::Even)
        .open_native_async()?;

    let (reader, writer) = tokio::io::split(port);
    let reader = FromTokio::new(reader);
    let writer = FromTokio::new(writer);

    let mut app = link::ctl::App::new(writer, reader);

    // Create progress bar
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
                    FlashPhase::Compressing => {
                        // MGMT doesn't use compression
                    }
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

#[derive(Subcommand)]
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
}

#[derive(Subcommand)]
enum MgmtAction {
    Ping {
        #[arg(default_value = "hello")]
        data: String,
    },
    /// Get bootloader information from MGMT chip (requires bootloader mode)
    Info,
    /// Flash firmware to MGMT chip (requires bootloader mode)
    Flash {
        /// Path to binary file to flash
        file: std::path::PathBuf,
    },
}

#[derive(Subcommand)]
enum UiAction {
    Ping {
        #[arg(default_value = "hello")]
        data: String,
    },
    /// Get bootloader information from UI chip (auto-resets chip)
    Info,
    /// Flash firmware to UI chip (auto-resets chip)
    Flash {
        /// Path to binary file to flash
        file: std::path::PathBuf,
    },
    GetVersion,
    SetVersion {
        /// Version number (base 10)
        version: u32,
    },
    #[command(name = "get-sframe-key")]
    GetSFrameKey,
    #[command(name = "set-sframe-key")]
    SetSFrameKey {
        /// SFrame key as 32-character hex string (e.g., "5b9f37b1546b61f914da9f557a8fe215")
        key: String,
    },
}

#[derive(Subcommand)]
enum NetAction {
    Ping {
        #[arg(default_value = "hello")]
        data: String,
    },
    /// Get bootloader information from NET chip (ESP32, auto-resets chip)
    Info,
    /// Flash firmware to NET chip (ESP32, auto-resets chip)
    ///
    /// WARNING: Currently assumes standard ESP-IDF partition layout with app at 0x10000.
    /// This may not work for custom partition tables. A future version should parse
    /// flasher_args.json from the build directory to determine correct addresses.
    Flash {
        /// Path to binary file to flash
        file: std::path::PathBuf,
        /// Flash address offset (default: 0x10000 for standard ESP-IDF app partition)
        #[arg(short, long, default_value = "0x10000")]
        address: String,
        /// Use compressed transfer (faster for large files)
        #[arg(short, long)]
        compress: bool,
        /// Skip MD5 verification after flashing (faster for large files)
        #[arg(long)]
        no_verify: bool,
    },
    #[command(name = "add-wifi")]
    AddWifi {
        /// WiFi network SSID
        ssid: String,
        /// WiFi network password
        password: String,
    },
    #[command(name = "get-wifi")]
    GetWifi,
    #[command(name = "clear-wifi")]
    ClearWifi,
    #[command(name = "get-relay-url")]
    GetRelayUrl,
    #[command(name = "set-relay-url")]
    SetRelayUrl {
        /// Relay server URL (wss://...)
        url: String,
    },
    #[command(name = "ws-ping")]
    WsPing {
        /// Data to send (will be echoed back by server)
        #[arg(default_value = "hello from hactar")]
        data: String,
    },
}

/// Known STM32 product IDs
fn chip_name(product_id: u16) -> &'static str {
    match product_id {
        0x410 => "STM32F1 Medium-density",
        0x411 => "STM32F2",
        0x412 => "STM32F1 Low-density",
        0x413 => "STM32F4 (405/407/415/417)",
        0x414 => "STM32F1 High-density",
        0x415 => "STM32L4 (75/76)",
        0x416 => "STM32L1 Medium-density",
        0x417 => "STM32L0 (51/52/53/62/63)",
        0x418 => "STM32F1 Connectivity line",
        0x419 => "STM32F4 (27/29/37/39/69/79)",
        0x420 => "STM32F1 Medium-density VL",
        0x421 => "STM32F446",
        0x440 => "STM32F0 (30/51/71)",
        0x442 => "STM32F0 (30/91/98)",
        0x443 => "STM32F0 (3/4/5)",
        0x444 => "STM32F0 (3/4) small",
        0x445 => "STM32F0 (4/7)",
        0x448 => "STM32F0 (70/71/72)",
        0x460 => "STM32G0 (70/71/B1)",
        0x466 => "STM32G0 (30/31/41)",
        0x467 => "STM32G0 (B0/C1)",
        _ => "Unknown",
    }
}

/// Command name lookup
fn command_name(code: u8) -> &'static str {
    match code {
        0x00 => "Get",
        0x01 => "Get Version",
        0x02 => "Get ID",
        0x11 => "Read Memory",
        0x21 => "Go",
        0x31 => "Write Memory",
        0x43 => "Erase",
        0x44 => "Extended Erase",
        0x63 => "Write Protect",
        0x73 => "Write Unprotect",
        0x82 => "Readout Protect",
        0x92 => "Readout Unprotect",
        _ => "Unknown",
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();

    // Handle mgmt commands specially - they require bootloader mode
    match &cli.command {
        Command::Mgmt {
            action: MgmtAction::Info,
        } => return handle_mgmt_info(cli.port).await,
        Command::Mgmt {
            action: MgmtAction::Flash { file },
        } => return handle_mgmt_flash(cli.port, file.clone()).await,
        _ => {}
    }

    let port_name = select_port(cli.port, cli.baud).await?;
    let port = tokio_serial::new(&port_name, cli.baud)
        .parity(tokio_serial::Parity::Even)
        .open_native_async()?;

    println!("Connected to {} at {} baud", port_name, cli.baud);

    // Split the port into read/write halves and wrap for embedded-io-async
    let (reader, writer) = tokio::io::split(port);
    let reader = FromTokio::new(reader);
    let writer = FromTokio::new(writer);

    let mut app = link::ctl::App::new(writer, reader);

    match cli.command {
        Command::Mgmt { action } => match action {
            MgmtAction::Ping { data } => {
                println!("Sending MGMT ping with data: {}", data);
                app.mgmt_ping(data.as_bytes()).await;
                println!("Received pong!");
            }
            MgmtAction::Info => unreachable!(), // Handled above
            MgmtAction::Flash { .. } => unreachable!(), // Handled above
        },
        Command::Ui { action } => match action {
            UiAction::Ping { data } => {
                println!("Sending UI ping with data: {}", data);
                app.ui_ping(data.as_bytes()).await;
                println!("Received pong!");
            }
            UiAction::Info => {
                println!("UI Bootloader Info");
                println!("==================\n");

                println!("Resetting UI chip to bootloader mode...");
                let delay = |ms| tokio::time::sleep(std::time::Duration::from_millis(ms));
                let Ok(info) = app.get_ui_bootloader_info(delay).await else {
                    eprintln!("Failed to get bootloader info");
                    eprintln!("\nThe UI chip may not be responding correctly.");
                    std::process::exit(1);
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
                    chip_name(info.chip_id)
                );

                println!("\nSupported Commands ({}):", info.command_count);
                for i in 0..info.command_count {
                    let cmd = info.commands[i];
                    println!("  0x{:02X} - {}", cmd, command_name(cmd));
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

                    // Analyze vector table
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

                println!("\nUI chip reset back to user mode.");
                println!("Done!");
            }
            UiAction::Flash { file } => {
                println!("UI Flash");
                println!("========\n");

                // Read the firmware file
                let firmware = std::fs::read(&file).expect("Failed to read firmware file");
                println!("Firmware: {} ({} bytes)", file.display(), firmware.len());
                println!("Resetting UI chip to bootloader mode...\n");

                // Create progress bar
                let pb = ProgressBar::new(firmware.len() as u64);
                let bytes_style = ProgressStyle::default_bar()
                    .template(
                        "{prefix:>12} [{bar:40.cyan/blue}] {bytes}/{total_bytes} ({percent}%)",
                    )
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
                                FlashPhase::Compressing => {
                                    // UI doesn't use compression
                                }
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
                    Ok(()) => {
                        println!("Flash complete!");
                        println!("UI chip reset back to user mode.");
                    }
                    Err(e) => {
                        eprintln!("\nFlash failed: {:?}", e);
                        eprintln!("\nThe UI chip may not be responding correctly.");
                        std::process::exit(1);
                    }
                }
            }
            UiAction::GetVersion => {
                let version = app.get_version().await;
                println!("{}", version);
            }
            UiAction::SetVersion { version } => {
                app.set_version(version).await;
                println!("Version set to {}", version);
            }
            UiAction::GetSFrameKey => {
                let key = app.get_sframe_key().await;
                println!("{}", hex::encode(key));
            }
            UiAction::SetSFrameKey { key } => {
                let key_bytes = hex::decode(&key).expect("Invalid hex string");
                if key_bytes.len() != 16 {
                    eprintln!("Error: SFrame key must be exactly 32 hex characters (16 bytes)");
                    std::process::exit(1);
                }
                let mut key_array = [0u8; 16];
                key_array.copy_from_slice(&key_bytes);
                app.set_sframe_key(&key_array).await;
                println!("SFrame key set to {}", key);
            }
        },
        Command::Net { action } => match action {
            NetAction::Ping { data } => {
                println!("Sending NET ping with data: {}", data);
                app.net_ping(data.as_bytes()).await;
                println!("Received pong!");
            }
            NetAction::Info => {
                println!("Resetting NET chip to bootloader mode...");
                let info = match app.get_net_bootloader_info().await {
                    Ok(info) => info,
                    Err(e) => {
                        eprintln!("Failed to get bootloader info: {:?}", e);
                        eprintln!("\nThe NET chip may not be responding correctly.");
                        std::process::exit(1);
                    }
                };

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

                println!("\nNET chip reset back to user mode.");
                println!("Done!");
            }
            NetAction::Flash {
                file,
                address,
                compress,
                no_verify,
            } => {
                println!("NET Flash (ESP32)");
                println!("=================\n");

                // Parse the address (supports hex with 0x prefix or decimal)
                let address: u32 = if address.starts_with("0x") || address.starts_with("0X") {
                    u32::from_str_radix(&address[2..], 16).expect("Invalid hex address")
                } else {
                    address.parse().expect("Invalid address")
                };

                // TODO: Parse flasher_args.json to get correct addresses automatically
                // For now, we assume standard ESP-IDF layout (app at 0x10000)
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

                // Read the firmware file
                let firmware = std::fs::read(&file).expect("Failed to read firmware file");
                println!("Firmware: {} ({} bytes)", file.display(), firmware.len());
                println!("Flash address: 0x{:08X}", address);
                println!("Resetting NET chip to bootloader mode...\n");

                // Create progress bar
                let pb = ProgressBar::new(firmware.len() as u64);
                let bytes_style = ProgressStyle::default_bar()
                    .template(
                        "{prefix:>12} [{bar:40.cyan/blue}] {bytes}/{total_bytes} ({percent}%)",
                    )
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
                    Ok(()) => {
                        println!("Flash complete!");
                        println!("NET chip reset back to user mode.");
                    }
                    Err(e) => {
                        eprintln!("\nFlash failed: {:?}", e);
                        eprintln!("\nThe NET chip may not be responding correctly.");
                        std::process::exit(1);
                    }
                }
            }
            NetAction::AddWifi { ssid, password } => {
                app.add_wifi_ssid(&ssid, &password).await;
                println!("Added WiFi network: {}", ssid);
            }
            NetAction::GetWifi => {
                let ssids = app.get_wifi_ssids().await;
                if ssids.is_empty() {
                    println!("No WiFi networks configured");
                } else {
                    for wifi in ssids {
                        println!("{}\t{}", wifi.ssid, wifi.password);
                    }
                }
            }
            NetAction::ClearWifi => {
                app.clear_wifi_ssids().await;
                println!("Cleared all WiFi networks");
            }
            NetAction::GetRelayUrl => {
                let url = app.get_relay_url().await;
                println!("{}", url);
            }
            NetAction::SetRelayUrl { url } => {
                app.set_relay_url(&url).await;
                println!("Relay URL set to {}", url);
            }
            NetAction::WsPing { data } => {
                println!("Sending WebSocket ping with data: {}", data);
                app.ws_ping(data.as_bytes()).await;
                println!("Received echo response!");
            }
        },
        Command::CircularPing { reverse, data } => {
            if reverse {
                println!("Sending NET-first circular ping with data: {}", data);
                app.net_first_circular_ping(data.as_bytes()).await;
            } else {
                println!("Sending UI-first circular ping with data: {}", data);
                app.ui_first_circular_ping(data.as_bytes()).await;
            }
            println!("Completed circular ping!");
        }
    }

    Ok(())
}

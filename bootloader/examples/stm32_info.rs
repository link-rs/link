//! Example: Query STM32 bootloader information
//!
//! This example demonstrates how to:
//! 1. Scan for available serial ports
//! 2. Connect to an STM32 in bootloader mode
//! 3. Query and display bootloader information
//!
//! To use this example, put your STM32 into bootloader mode:
//! 1. Set BOOT0 pin high
//! 2. Reset the device
//!
//! Then run: cargo run --example stm32_info

use bootloader::stm::Bootloader;
use embedded_io_adapters::tokio_1::FromTokio;
use serialport::SerialPortType;
use std::io::Write;
use tokio_serial::SerialPortBuilderExt;

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
        0x422 => "STM32F3 (01/02)",
        0x423 => "STM32F4 (01/11)",
        0x425 => "STM32L0 (31/41)",
        0x427 => "STM32L1 Medium-density Plus",
        0x428 => "STM32F1 High-density VL",
        0x429 => "STM32L1 Cat.2",
        0x430 => "STM32F1 XL-density",
        0x431 => "STM32F411",
        0x432 => "STM32F37x",
        0x433 => "STM32F4 (01/11) LQFP64",
        0x434 => "STM32F469/479",
        0x435 => "STM32L43x/44x",
        0x436 => "STM32L1 High-density",
        0x437 => "STM32L152RE",
        0x438 => "STM32F334",
        0x439 => "STM32F3 (01/02) xB",
        0x440 => "STM32F0 (30/51/71)",
        0x441 => "STM32F412",
        0x442 => "STM32F0 (30/91/98)",
        0x443 => "STM32F0 (3/4/5)",
        0x444 => "STM32F0 (3/4) small",
        0x445 => "STM32F0 (4/7)",
        0x446 => "STM32F303 HD",
        0x447 => "STM32L0 (73/83)",
        0x448 => "STM32F0 (70/71/72)",
        0x449 => "STM32F7 (45/46/56)",
        0x450 => "STM32H7 (42/43/50/53)",
        0x451 => "STM32F76x/77x",
        0x452 => "STM32F72x/73x",
        0x457 => "STM32L0 (11/21)",
        0x458 => "STM32F410",
        0x460 => "STM32G0 (70/71/B1)",
        0x461 => "STM32L496/4A6",
        0x462 => "STM32L45x/46x",
        0x463 => "STM32F413/423",
        0x464 => "STM32L41x/42x",
        0x466 => "STM32G0 (30/31/41)",
        0x467 => "STM32G0 (B0/C1)",
        0x468 => "STM32G4 (31/41)",
        0x469 => "STM32G4 (73/74/83/84)",
        0x470 => "STM32L4R/S",
        0x471 => "STM32L4P5/Q5",
        0x472 => "STM32L5 (52/62)",
        0x479 => "STM32G4 (91/A1)",
        0x480 => "STM32H7 (A3/B0/B3)",
        0x483 => "STM32H7 (2x/3x)",
        0x494 => "STM32WB (15/35/55)",
        0x495 => "STM32WB (50/55) (USB)",
        0x496 => "STM32WB (35/55) (no USB)",
        0x497 => "STM32WL (E5/55)",
        0x498 => "STM32WL (E4/54) (no SMPS)",
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

/// Find USB serial ports
fn find_usb_serial_ports() -> Vec<(String, String)> {
    serialport::available_ports()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|p| {
            if let SerialPortType::UsbPort(info) = &p.port_type {
                let description = format!(
                    "{} {}",
                    info.manufacturer.as_deref().unwrap_or("Unknown"),
                    info.product.as_deref().unwrap_or("Serial Device")
                );
                Some((p.port_name, description))
            } else {
                None
            }
        })
        .collect()
}

/// Prompt user to select a serial port
fn select_port() -> Result<String, String> {
    let ports = find_usb_serial_ports();

    match ports.len() {
        0 => Err("No USB serial ports found".to_string()),
        1 => {
            println!("Auto-selected port: {} ({})", ports[0].0, ports[0].1);
            Ok(ports[0].0.clone())
        }
        _ => {
            println!("Available USB serial ports:");
            for (i, (name, desc)) in ports.iter().enumerate() {
                println!("  {}: {} ({})", i + 1, name, desc);
            }
            print!("\nSelect port [1-{}]: ", ports.len());
            std::io::stdout().flush().unwrap();

            let mut input = String::new();
            std::io::stdin()
                .read_line(&mut input)
                .map_err(|e| format!("Failed to read input: {}", e))?;

            let choice: usize = input
                .trim()
                .parse()
                .map_err(|_| "Invalid number".to_string())?;

            if choice < 1 || choice > ports.len() {
                return Err(format!(
                    "Please select a number between 1 and {}",
                    ports.len()
                ));
            }

            Ok(ports[choice - 1].0.clone())
        }
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("STM32 Bootloader Info");
    println!("=====================\n");

    // Select serial port
    let port_name = select_port()?;

    // Open serial port at 115200 baud (default for STM32 bootloader)
    // Note: Some STM32 bootloaders support auto-baud detection
    let baud_rate = 115200;
    println!("\nOpening {} at {} baud...", port_name, baud_rate);

    let port = tokio_serial::new(&port_name, baud_rate)
        .parity(tokio_serial::Parity::Even) // STM32 bootloader uses even parity
        .open_native_async()?;

    // Split into read/write halves and wrap for embedded-io-async
    let (reader, writer) = tokio::io::split(port);
    let reader = FromTokio::new(reader);
    let writer = FromTokio::new(writer);

    let mut bl = Bootloader::new(reader, writer);

    // Initialize communication
    println!("Initializing bootloader communication...\n");
    match bl.init().await {
        Ok(()) => println!("Bootloader synchronized successfully!\n"),
        Err(e) => {
            eprintln!("Failed to initialize bootloader: {:?}", e);
            eprintln!("\nMake sure the STM32 is in bootloader mode:");
            eprintln!("  1. Set BOOT0 pin high");
            eprintln!("  2. Reset the device");
            return Err("Bootloader initialization failed".into());
        }
    }

    // Get bootloader info
    println!("Querying bootloader information...\n");

    match bl.get().await {
        Ok(info) => {
            let major = info.version >> 4;
            let minor = info.version & 0x0F;
            println!("Bootloader Version: {}.{} (0x{:02X})", major, minor, info.version);
            println!("\nSupported Commands ({}):", info.command_count);
            for i in 0..info.command_count {
                let cmd = info.commands[i];
                println!("  0x{:02X} - {}", cmd, command_name(cmd));
            }
        }
        Err(e) => {
            eprintln!("Failed to get bootloader info: {:?}", e);
        }
    }

    // Get protocol version
    println!();
    match bl.get_version().await {
        Ok(version) => {
            let major = version.version >> 4;
            let minor = version.version & 0x0F;
            println!(
                "Protocol Version: {}.{} (0x{:02X})",
                major, minor, version.version
            );
            println!(
                "  Option bytes: 0x{:02X}, 0x{:02X}",
                version.option1, version.option2
            );
        }
        Err(e) => {
            eprintln!("Failed to get protocol version: {:?}", e);
        }
    }

    // Get chip ID
    println!();
    match bl.get_id().await {
        Ok(chip_id) => {
            println!("Chip ID: 0x{:04X} ({})", chip_id, chip_name(chip_id));
        }
        Err(e) => {
            eprintln!("Failed to get chip ID: {:?}", e);
        }
    }

    // Try to read a small amount of memory from the start of flash
    println!();
    println!("Reading flash memory at 0x08000000...");
    let mut buffer = [0u8; 32];
    match bl.read_memory(0x0800_0000, &mut buffer).await {
        Ok(bytes_read) => {
            println!("Read {} bytes from flash:", bytes_read);
            print!("  ");
            for (i, byte) in buffer[..bytes_read].iter().enumerate() {
                print!("{:02X} ", byte);
                if (i + 1) % 16 == 0 && i + 1 < bytes_read {
                    print!("\n  ");
                }
            }
            println!();

            // Check if this looks like a valid vector table
            let sp = u32::from_le_bytes([buffer[0], buffer[1], buffer[2], buffer[3]]);
            let reset = u32::from_le_bytes([buffer[4], buffer[5], buffer[6], buffer[7]]);

            println!();
            println!("Vector Table Analysis:");
            println!("  Initial SP:      0x{:08X}", sp);
            println!("  Reset Handler:   0x{:08X}", reset);

            // Check if values look valid for STM32
            if (0x2000_0000..0x2002_0000).contains(&sp) {
                println!("  (SP appears valid - points to SRAM)");
            }
            if (0x0800_0000..0x0810_0000).contains(&reset) && (reset & 1) == 1 {
                println!("  (Reset handler appears valid - points to Flash, Thumb mode)");
            }
        }
        Err(e) => {
            eprintln!("Failed to read memory: {:?}", e);
            eprintln!("(Read protection may be enabled)");
        }
    }

    // Jump back to user code to reset the chip to normal operation
    println!();
    println!("Jumping to user code at 0x08000000...");
    match bl.go(0x0800_0000).await {
        Ok(()) => println!("Jump successful - chip should now be running user code"),
        Err(e) => eprintln!("Failed to jump to user code: {:?}", e),
    }

    println!("\nDone!");
    Ok(())
}

//! MGMT chip command handlers.

use crate::{App, GetSetU32, MgmtAction};
use indicatif::{ProgressBar, ProgressStyle};
use link::ctl::FlashPhase;
use serialport::SerialPort;
use std::io::Write;

pub fn handle_mgmt(action: MgmtAction, app: &mut App) -> Result<(), Box<dyn std::error::Error>> {
    match action {
        MgmtAction::Ping { data } => {
            println!("Sending MGMT ping with data: {}", data);
            app.mgmt_ping(data.as_bytes());
            println!("Received pong!");
            Ok(())
        }
        MgmtAction::Info => {
            println!("MGMT Bootloader Info");
            println!("====================\n");

            println!("To read bootloader information, the MGMT chip must be in bootloader mode.");
            println!("Please follow these steps:");
            println!("  1. Set the BOOT0 pin high on the MGMT chip");
            println!("  2. Reset the MGMT chip");
            println!();
            print!("Press Enter when ready (or Ctrl+C to cancel)... ");
            std::io::stdout().flush()?;

            let mut input = String::new();
            std::io::stdin().read_line(&mut input)?;

            println!("Querying bootloader information...\n");

            let Ok(info) = app.get_mgmt_bootloader_info() else {
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
                link::ctl::stm::chip_name(info.chip_id)
            );

            println!("\nSupported Commands ({}):", info.command_count);
            for i in 0..info.command_count {
                let cmd = info.commands[i];
                println!("  0x{:02X} - {}", cmd, link::ctl::stm::command_name(cmd));
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
        MgmtAction::Flash { file } => {
            println!("MGMT Flash");
            println!("==========\n");

            let firmware = std::fs::read(&file)?;
            println!("Firmware: {} ({} bytes)", file.display(), firmware.len());

            println!("\nTo flash the MGMT chip, it must be in bootloader mode.");
            println!("Please follow these steps:");
            println!("  1. Set the BOOT0 pin high on the MGMT chip");
            println!("  2. Reset the MGMT chip");
            println!();
            print!("Press Enter when ready (or Ctrl+C to cancel)... ");
            std::io::stdout().flush()?;

            let mut input = String::new();
            std::io::stdin().read_line(&mut input)?;

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
            let result = app.flash_mgmt(&firmware, |phase, progress, total| {
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
            });

            pb.finish_and_clear();

            match result {
                Ok(()) => {
                    println!("\nFlash complete!");
                    println!("The MGMT chip should now be running the new firmware.");
                    println!(
                        "\nNote: Set BOOT0 low and reset to ensure normal boot on next power cycle."
                    );
                    Ok(())
                }
                Err(e) => {
                    eprintln!("\nFlash failed: {:?}", e);
                    eprintln!("\nMake sure the MGMT chip is in bootloader mode:");
                    eprintln!("  1. Set BOOT0 pin high");
                    eprintln!("  2. Reset the device");
                    Err("Flash failed".into())
                }
            }
        }
        MgmtAction::NetBaudRate { action } => {
            let action = action.unwrap_or_default();
            match action {
                GetSetU32::Get => {
                    // MGMT doesn't currently support querying baud rate
                    println!("Get not implemented - MGMT protocol doesn't support baud rate queries");
                    Ok(())
                }
                GetSetU32::Set { value } => {
                    println!("Setting NET UART baud rate to {}", value);
                    app.set_net_baud_rate(value);
                    println!("NET baud rate set to {}", value);
                    Ok(())
                }
            }
        }
        MgmtAction::CtlBaudRate { action } => {
            let action = action.unwrap_or_default();
            match action {
                GetSetU32::Get => {
                    // MGMT doesn't currently support querying baud rate
                    println!("Get not implemented - MGMT protocol doesn't support baud rate queries");
                    Ok(())
                }
                GetSetU32::Set { value } => {
                    println!("Setting CTL UART baud rate to {}", value);

                    // Send command to MGMT (ACK is sent before rate change)
                    app.set_ctl_baud_rate(value);

                    // Now change local serial port baud rate to match
                    let reader_port: &mut Box<dyn SerialPort> =
                        app.reader_mut().inner_mut().get_mut();
                    reader_port.set_baud_rate(value)?;

                    let writer_port: &mut Box<dyn SerialPort> =
                        app.writer_mut().inner_mut().get_mut();
                    writer_port.set_baud_rate(value)?;

                    println!("CTL baud rate set to {} (both MGMT and local)", value);
                    Ok(())
                }
            }
        }
    }
}

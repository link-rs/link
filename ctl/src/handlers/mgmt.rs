//! MGMT chip command handlers.

use super::Core;
use crate::{GetSetU32, MgmtAction, StackAction};
use indicatif::{ProgressBar, ProgressStyle};
use link::ctl::SetTimeout;
use link::ctl::flash::{FlashPhase, MgmtBootloaderEntry};
use link::{CtlToMgmt, MgmtToCtl};
use std::io::Write;
use std::time::{Duration, Instant};
use tokio_serial::SerialPort;

pub async fn handle_mgmt(action: MgmtAction, core: &mut Core) -> Result<(), Box<dyn std::error::Error>> {
    match action {
        MgmtAction::Ping { data } => {
            println!("Sending MGMT ping with data: {}", data);
            core.mgmt_ping(data.as_bytes()).await?;
            println!("Received pong!");
            Ok(())
        }
        MgmtAction::Info => {
            println!("MGMT Bootloader Info");
            println!("====================\n");

            // Try automatic bootloader entry (EV16) or detect if already in bootloader
            print!("Attempting automatic bootloader entry... ");
            std::io::stdout().flush()?;

            // Set short timeout for probing
            let _ = core.port_mut().set_timeout(Duration::from_millis(200));

            let delay_ms = |ms| std::thread::sleep(Duration::from_millis(ms));
            match core.try_enter_mgmt_bootloader(delay_ms).await {
                MgmtBootloaderEntry::AutoReset => {
                    println!("success (EV16 detected)");
                }
                MgmtBootloaderEntry::AlreadyActive => {
                    println!("bootloader already active");
                }
                MgmtBootloaderEntry::NotDetected => {
                    println!("not detected");
                    println!("\nAutomatic reset failed. Manual bootloader entry required.");
                    println!("Please follow these steps:");
                    println!("  1. Set the BOOT0 pin high on the MGMT chip");
                    println!("  2. Reset the MGMT chip");
                    println!();
                    print!("Press Enter when ready (or Ctrl+C to cancel)... ");
                    std::io::stdout().flush()?;

                    let mut input = String::new();
                    std::io::stdin().read_line(&mut input)?;
                }
            }

            // Restore normal timeout
            let _ = core.port_mut().set_timeout(Duration::from_secs(3));

            println!("Querying bootloader information...\n");

            let Ok(info) = core.get_mgmt_bootloader_info().await else {
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

            // Try automatic bootloader entry (EV16) or detect if already in bootloader
            print!("\nAttempting automatic bootloader entry... ");
            std::io::stdout().flush()?;

            // Set short timeout for probing
            let _ = core.port_mut().set_timeout(Duration::from_millis(200));

            let delay_ms = |ms| std::thread::sleep(Duration::from_millis(ms));
            match core.try_enter_mgmt_bootloader(delay_ms).await {
                MgmtBootloaderEntry::AutoReset => {
                    println!("success (EV16 detected)");
                }
                MgmtBootloaderEntry::AlreadyActive => {
                    println!("bootloader already active");
                }
                MgmtBootloaderEntry::NotDetected => {
                    println!("not detected");
                    println!("\nAutomatic reset failed. Manual bootloader entry required.");
                    println!("Please follow these steps:");
                    println!("  1. Set the BOOT0 pin high on the MGMT chip");
                    println!("  2. Reset the MGMT chip");
                    println!();
                    print!("Press Enter when ready (or Ctrl+C to cancel)... ");
                    std::io::stdout().flush()?;

                    let mut input = String::new();
                    std::io::stdin().read_line(&mut input)?;
                }
            }

            // Restore normal timeout for flashing
            let _ = core.port_mut().set_timeout(Duration::from_secs(3));

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
            let result = core.flash_mgmt(&firmware, |phase, progress, total| {
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
            }).await;

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
                    println!(
                        "Get not implemented - MGMT protocol doesn't support baud rate queries"
                    );
                    Ok(())
                }
                GetSetU32::Set { value } => {
                    println!("Setting NET UART baud rate to {}", value);
                    core.set_net_baud_rate(value).await?;
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
                    println!(
                        "Get not implemented - MGMT protocol doesn't support baud rate queries"
                    );
                    Ok(())
                }
                GetSetU32::Set { value } => {
                    println!("Setting CTL UART baud rate to {}", value);

                    // Send command to MGMT (ACK is sent before rate change)
                    core.set_ctl_baud_rate(value).await?;

                    // Now change local serial port baud rate to match
                    core.port_mut().get_mut().set_baud_rate(value)?;

                    println!("CTL baud rate set to {} (both MGMT and local)", value);
                    Ok(())
                }
            }
        }
        MgmtAction::SpeedTest {
            baud,
            duration,
            size,
        } => {
            // Validate and clamp payload size
            const MAX_PAYLOAD: usize = 640; // MAX_VALUE_SIZE from TLV
            let payload_size = size.min(MAX_PAYLOAD);
            if size > MAX_PAYLOAD {
                println!(
                    "Warning: payload size clamped to {} (max TLV value size)",
                    MAX_PAYLOAD
                );
            }

            // Get current baud rate for reporting
            let initial_baud = core.port_mut().get_ref().baud_rate().unwrap_or(115200);

            // If a baud rate was specified, change to it first
            let test_baud = if let Some(new_baud) = baud {
                println!(
                    "Changing baud rate from {} to {}...",
                    initial_baud, new_baud
                );

                // Send command to MGMT (ACK is sent before rate change)
                core.set_ctl_baud_rate(new_baud).await?;

                // Now change local serial port baud rate to match
                core.port_mut().get_mut().set_baud_rate(new_baud)?;

                // Small delay for baud rate to stabilize
                tokio::time::sleep(Duration::from_millis(10)).await;

                new_baud
            } else {
                initial_baud
            };

            println!(
                "Running CTL-MGMT speed test at {} baud for {} second(s) with {} byte packets...",
                test_baud, duration, payload_size
            );

            // Create payload buffer
            let payload: Vec<u8> = (0..payload_size).map(|i| (i & 0xFF) as u8).collect();

            let test_duration = Duration::from_secs(duration as u64);
            let start = Instant::now();
            let mut packets_sent: u32 = 0;
            let mut send_errors: u32 = 0;

            // Send packets as fast as possible for the duration
            while start.elapsed() < test_duration {
                if core.write_tlv_raw(CtlToMgmt::SpeedTestData, &payload).await.is_err() {
                    send_errors += 1;
                } else {
                    packets_sent += 1;
                }
            }

            let send_elapsed = start.elapsed();

            // Send done signal
            println!("Sending done signal and waiting for results...");
            core.write_tlv_raw(CtlToMgmt::SpeedTestDone, &[]).await?;

            // Wait for result from MGMT (with timeout)
            core.port_mut().set_timeout(Duration::from_secs(5))?;

            let (packets_received, bytes_received) = loop {
                match core.read_tlv_raw().await {
                    Ok(Some(tlv)) => {
                        if tlv.tlv_type == MgmtToCtl::SpeedTestResult && tlv.value.len() >= 8 {
                            let packets = u32::from_le_bytes([
                                tlv.value[0],
                                tlv.value[1],
                                tlv.value[2],
                                tlv.value[3],
                            ]);
                            let bytes = u32::from_le_bytes([
                                tlv.value[4],
                                tlv.value[5],
                                tlv.value[6],
                                tlv.value[7],
                            ]);
                            break (packets, bytes);
                        }
                        // Ignore other TLVs
                    }
                    Ok(None) | Err(_) => {
                        eprintln!("Timeout waiting for speed test result");
                        break (0, 0);
                    }
                }
            };

            // Calculate statistics
            let packets_per_second = packets_received as f64 / send_elapsed.as_secs_f64();
            // Total bytes on wire per packet: sync(4) + header(6) + payload
            let bytes_per_packet = 4 + 6 + payload_size;
            let total_wire_bytes = packets_received as usize * bytes_per_packet;
            // Bits per second (10 bits per byte for UART: 8 data + start + stop)
            let bits_per_second = (total_wire_bytes as f64 * 10.0) / send_elapsed.as_secs_f64();
            let efficiency = (bits_per_second / test_baud as f64) * 100.0;

            println!("\nSpeed Test Results:");
            println!("  Baud rate:          {} bps", test_baud);
            println!(
                "  Duration:           {:.2} seconds",
                send_elapsed.as_secs_f64()
            );
            println!("  Payload size:       {} bytes", payload_size);
            println!("  Packets sent:       {}", packets_sent);
            println!("  Packets received:   {}", packets_received);
            println!("  Payload received:   {} bytes", bytes_received);
            if send_errors > 0 {
                println!("  Send errors:        {}", send_errors);
            }
            println!(
                "  Packet rate:        {:.1} packets/sec",
                packets_per_second
            );
            println!(
                "  Throughput:         {:.0} bits/sec ({:.1}% efficiency)",
                bits_per_second, efficiency
            );

            // Restore original baud rate if we changed it
            if baud.is_some() && initial_baud != test_baud {
                println!("\nRestoring baud rate to {}...", initial_baud);
                core.set_ctl_baud_rate(initial_baud).await?;
                core.port_mut().get_mut().set_baud_rate(initial_baud)?;
            }

            // Restore normal timeout
            core.port_mut().set_timeout(Duration::from_secs(3))?;

            Ok(())
        }
        MgmtAction::Stack { action } => match action.unwrap_or_default() {
            StackAction::Info => {
                let info = core.mgmt_get_stack_info().await?;
                println!("Stack Base:  0x{:08X}", info.stack_base);
                println!("Stack Top:   0x{:08X}", info.stack_top);
                println!("Stack Size:  {} bytes ({:.1} KB)", info.stack_size, info.stack_size as f64 / 1024.0);
                println!("Stack Used:  {} bytes ({:.1}%)", info.stack_used, info.stack_used as f64 / info.stack_size as f64 * 100.0);
                println!("Stack Free:  {} bytes", info.stack_size.saturating_sub(info.stack_used));
                Ok(())
            }
            StackAction::Repaint => {
                core.mgmt_repaint_stack().await?;
                println!("Stack repainted");
                Ok(())
            }
        },
    }
}

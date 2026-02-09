//! UI chip command handlers.

use super::Core;
use crate::{GetSetHex, GetSetU32, LoopbackAction, PinAction, PinLevel, ResetAction, StackAction, UiAction};
use indicatif::{ProgressBar, ProgressStyle};
use link::ctl::flash::FlashPhase;
use link::ctl::SetTimeout;
use link::UiLoopbackMode;
use std::io::Write;
use std::time::Duration;

pub async fn handle_ui(action: UiAction, core: &mut Core) -> Result<(), Box<dyn std::error::Error>> {
    match action {
        UiAction::Ping { data } => {
            println!("Sending UI ping with data: {}", data);
            core.ui_ping(data.as_bytes()).await?;
            println!("Received pong!");
            Ok(())
        }
        UiAction::Info => {
            println!("UI Bootloader Info");
            println!("==================\n");
            println!("Resetting UI chip to bootloader mode...");

            let delay = |ms| tokio::time::sleep(Duration::from_millis(ms));
            let info = core.get_ui_bootloader_info(delay).await
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

            println!("UI chip reset back to user mode.");
            println!("Done!");
            Ok(())
        }
        UiAction::Flash { file, no_verify } => {
            println!("UI Flash");
            println!("========\n");

            let firmware = std::fs::read(&file)?;
            println!("Firmware: {} ({} bytes)", file.display(), firmware.len());
            if no_verify {
                println!("Verification: skipped");
            }

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
            let delay = |ms| tokio::time::sleep(Duration::from_millis(ms));
            let verify = !no_verify;
            let result = core.flash_ui(&firmware, delay, verify, |phase, progress, total| {
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
            }).await;

            pb.finish_and_clear();

            match result {
                Ok(()) => {
                    println!("Flash complete!");
                    println!("UI chip reset back to user mode.");
                    Ok(())
                }
                Err(e) => Err(format!("Flash failed: {:?}", e).into()),
            }
        }
        UiAction::Version { action } => match action.unwrap_or_default() {
            GetSetU32::Get => {
                let version = core.get_version().await?;
                println!("{}", version);
                Ok(())
            }
            GetSetU32::Set { value } => {
                core.set_version(value).await?;
                println!("Version set to {}", value);
                Ok(())
            }
        },
        UiAction::SFrameKey { action } => match action.unwrap_or_default() {
            GetSetHex::Get => {
                let key = core.get_sframe_key().await?;
                println!("{}", hex::encode(key));
                Ok(())
            }
            GetSetHex::Set { value } => {
                let key_bytes = hex::decode(&value).map_err(|_| "Invalid hex string")?;
                if key_bytes.len() != 16 {
                    return Err("SFrame key must be exactly 32 hex characters (16 bytes)".into());
                }
                let mut key_array = [0u8; 16];
                key_array.copy_from_slice(&key_bytes);
                core.set_sframe_key(&key_array).await?;
                println!("SFrame key set to {}", value);
                Ok(())
            }
        },
        UiAction::Loopback { action } => match action.unwrap_or_default() {
            LoopbackAction::Get => {
                let mode = core.ui_get_loopback().await?;
                println!("{:?}", mode);
                Ok(())
            }
            LoopbackAction::Off => {
                core.ui_set_loopback(UiLoopbackMode::Off).await?;
                println!("UI loopback: off");
                Ok(())
            }
            LoopbackAction::Raw => {
                core.ui_set_loopback(UiLoopbackMode::Raw).await?;
                println!("UI loopback: raw");
                Ok(())
            }
            LoopbackAction::Alaw => {
                core.ui_set_loopback(UiLoopbackMode::Alaw).await?;
                println!("UI loopback: alaw");
                Ok(())
            }
            LoopbackAction::Sframe => {
                core.ui_set_loopback(UiLoopbackMode::Sframe).await?;
                println!("UI loopback: sframe");
                Ok(())
            }
        },
        UiAction::Boot0 { action: PinAction::Set { level } } => {
            let high = matches!(level, PinLevel::High);
            core.set_ui_boot0(high).await?;
            println!("UI BOOT0: {}", if high { "high" } else { "low" });
            Ok(())
        }
        UiAction::Boot1 { action: PinAction::Set { level } } => {
            let high = matches!(level, PinLevel::High);
            core.set_ui_boot1(high).await?;
            println!("UI BOOT1: {}", if high { "high" } else { "low" });
            Ok(())
        }
        UiAction::Rst { action: PinAction::Set { level } } => {
            let high = matches!(level, PinLevel::High);
            core.set_ui_rst(high).await?;
            println!("UI RST: {}", if high { "high" } else { "low" });
            Ok(())
        }
        UiAction::Reset { action } => match action.unwrap_or_default() {
            ResetAction::User => {
                core.reset_ui_to_user().await?;
                println!("UI chip reset to user mode");
                Ok(())
            }
            ResetAction::Bootloader => {
                core.reset_ui_to_bootloader().await?;
                println!("UI chip reset to bootloader mode");
                Ok(())
            }
            ResetAction::Hold => {
                core.hold_ui_reset().await?;
                println!("UI chip held in reset");
                Ok(())
            }
            ResetAction::Release => {
                core.set_ui_rst(true).await?;
                println!("UI chip released from reset");
                Ok(())
            }
        },
        UiAction::Monitor { reset } => {
            if reset {
                println!("Resetting UI chip...");
                core.reset_ui_to_user().await?;
            }
            println!("Monitoring UI chip logs (ESC to stop)...\n");

            // Set a short timeout for non-blocking reads
            if let Err(e) = core.port_mut().set_timeout(Duration::from_millis(100)) {
                eprintln!("Warning: couldn't set timeout: {}", e);
            }

            use crossterm::event::{self, Event, KeyCode, KeyEvent};
            use crossterm::terminal;

            // Enable raw mode to capture ESC
            terminal::enable_raw_mode()?;

            let result = async {
                loop {
                    // Check for key press (non-blocking)
                    if event::poll(Duration::from_millis(0))? {
                        if let Event::Key(KeyEvent {
                            code: KeyCode::Esc, ..
                        }) = event::read()?
                        {
                            return Ok::<(), Box<dyn std::error::Error>>(());
                        }
                    }

                    // Check for TLV data from UI (timeout-aware)
                    match core.try_read_ui_log().await {
                        Ok(Some(msg)) => {
                            // Use \r\n for raw terminal mode
                            print!("[UI] {}\r\n", msg);
                            std::io::stdout().flush().ok();
                        }
                        Ok(None) => {
                            // Timeout or non-log TLV, continue
                        }
                        Err(e) => {
                            return Err(format!("Read error: {:?}", e).into());
                        }
                    }
                }
            }.await;

            // Always restore terminal mode and timeout
            terminal::disable_raw_mode()?;

            // Restore timeout to normal (3 seconds)
            if let Err(e) = core.port_mut().set_timeout(Duration::from_secs(3)) {
                eprintln!("Warning: couldn't restore timeout: {}", e);
            }

            println!("\nMonitor stopped.");

            result
        }
        UiAction::Stack { action } => match action.unwrap_or_default() {
            StackAction::Info => {
                let info = core.ui_get_stack_info().await?;
                println!("Stack Base:  0x{:08X}", info.stack_base);
                println!("Stack Top:   0x{:08X}", info.stack_top);
                println!("Stack Size:  {} bytes ({:.1} KB)", info.stack_size, info.stack_size as f64 / 1024.0);
                println!("Stack Used:  {} bytes ({:.1}%)", info.stack_used, info.usage_percent());
                println!("Stack Free:  {} bytes", info.stack_free());
                Ok(())
            }
            StackAction::Repaint => {
                core.ui_repaint_stack().await?;
                println!("Stack repainted");
                Ok(())
            }
        },
    }
}

//! UI chip command handlers.

use super::Core;
use crate::{
    GetSetHex, GetSetU32, LogsAction, LoopbackAction, PinAction, PinLevel, ResetAction,
    StackAction, UiAction, VolumeAction,
};
use indicatif::{ProgressBar, ProgressStyle};
use link::ctl::SetTimeout;
use link::ctl::flash::FlashPhase;
use link::protocol_config::timeouts;
use link::{AdjDirection, PinValue, UiLoopbackMode};
use std::io::Write;
use std::time::Duration;

pub async fn handle_ui(
    action: UiAction,
    core: &mut Core,
) -> Result<(), Box<dyn std::error::Error>> {
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
            let info = core
                .get_ui_bootloader_info(delay)
                .await
                .map_err(|_| "Failed to get bootloader info")?;

            println!(
                "Bootloader Version: {}.{} (0x{:02X})",
                info.version_major(),
                info.version_minor(),
                info.bootloader_version
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

                println!("\nVector Table Analysis:");
                if let Some(sp) = info.sp() {
                    println!("  Initial SP:      0x{:08X}", sp);
                }
                if let Some(reset) = info.reset_handler() {
                    println!("  Reset Handler:   0x{:08X}", reset);
                }
                if info.sp_valid() {
                    println!("  (SP appears valid - points to SRAM)");
                }
                if info.reset_valid() {
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
            let result = core
                .flash_ui(&firmware, delay, verify, |phase, progress, total| {
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
                core.set_sframe_key(&key_bytes).await?;
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
        UiAction::Boot0 {
            action: PinAction::Set { level },
        } => {
            let value = match level {
                PinLevel::High => PinValue::High,
                PinLevel::Low => PinValue::Low,
            };
            core.set_ui_boot0(value).await?;
            println!("UI BOOT0: {:?}", value);
            Ok(())
        }
        UiAction::Boot1 {
            action: PinAction::Set { level },
        } => {
            let value = match level {
                PinLevel::High => PinValue::High,
                PinLevel::Low => PinValue::Low,
            };
            core.set_ui_boot1(value).await?;
            println!("UI BOOT1: {:?}", value);
            Ok(())
        }
        UiAction::Rst {
            action: PinAction::Set { level },
        } => {
            let value = match level {
                PinLevel::High => PinValue::High,
                PinLevel::Low => PinValue::Low,
            };
            core.set_ui_rst(value).await?;
            println!("UI RST: {:?}", value);
            Ok(())
        }
        UiAction::Reset { action } => match action.unwrap_or_default() {
            ResetAction::User => {
                let delay = |ms| tokio::time::sleep(Duration::from_millis(ms));
                core.reset_ui_to_user(delay).await?;
                println!("UI chip reset to user mode");
                Ok(())
            }
            ResetAction::Bootloader => {
                let delay = |ms| tokio::time::sleep(Duration::from_millis(ms));
                core.reset_ui_to_bootloader(delay).await?;
                println!("UI chip reset to bootloader mode");
                Ok(())
            }
            ResetAction::Hold => {
                core.hold_ui_reset().await?;
                println!("UI chip held in reset");
                Ok(())
            }
            ResetAction::Release => {
                core.set_ui_rst(PinValue::High).await?;
                println!("UI chip released from reset");
                Ok(())
            }
        },
        UiAction::Monitor { reset } => {
            if reset {
                println!("Resetting UI chip...");
                let delay = |ms| tokio::time::sleep(Duration::from_millis(ms));
                core.reset_ui_to_user(delay).await?;
            }
            println!("Monitoring UI chip logs (ESC to stop)...\n");

            // Set a short timeout for non-blocking reads
            if let Err(e) = core
                .port_mut()
                .set_timeout(Duration::from_millis(timeouts::MONITOR_MS))
            {
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
                            if e.is_timeout() {
                                continue;
                            }
                            return Err(format!("Read error: {:?}", e).into());
                        }
                    }
                }
            }
            .await;

            // Always restore terminal mode and timeout
            terminal::disable_raw_mode()?;

            // Restore timeout to normal
            if let Err(e) = core
                .port_mut()
                .set_timeout(Duration::from_secs(timeouts::NORMAL_SECS))
            {
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
                println!(
                    "Stack Size:  {} bytes ({:.1} KB)",
                    info.stack_size,
                    info.stack_size as f64 / 1024.0
                );
                println!(
                    "Stack Used:  {} bytes ({:.1}%)",
                    info.stack_used,
                    info.usage_percent()
                );
                println!("Stack Free:  {} bytes", info.stack_free());
                Ok(())
            }
            StackAction::Repaint => {
                core.ui_repaint_stack().await?;
                println!("Stack repainted");
                Ok(())
            }
        },
        UiAction::Logs { action } => match action.unwrap_or_default() {
            LogsAction::Get => {
                let enabled = core.ui_get_logs_enabled().await?;
                println!("{}", if enabled { "on" } else { "off" });
                Ok(())
            }
            LogsAction::On => {
                core.ui_set_logs_enabled(true).await?;
                println!("UI logs: on");
                Ok(())
            }
            LogsAction::Off => {
                core.ui_set_logs_enabled(false).await?;
                println!("UI logs: off");
                Ok(())
            }
        },
        UiAction::ClearStorage => {
            core.ui_clear_storage().await?;
            println!("UI storage cleared");
            Ok(())
        }
        UiAction::Volume { action } => match action.unwrap_or_default() {
            VolumeAction::Get => {
                let volume = core.ui_get_volume().await?;
                println!("{}", volume);
                Ok(())
            }
            VolumeAction::Set { value } => {
                let volume = core.ui_set_volume(value).await?;
                println!("Volume set to {}", volume);
                Ok(())
            }
            VolumeAction::Up { amount } => {
                let volume = core
                    .ui_adjust_volume(AdjDirection::Up, amount.unwrap_or(1))
                    .await?;
                println!("Volume set to {}", volume);
                Ok(())
            }
            VolumeAction::Down { amount } => {
                let volume = core
                    .ui_adjust_volume(AdjDirection::Down, amount.unwrap_or(1))
                    .await?;
                println!("Volume set to {}", volume);
                Ok(())
            }
        },
        UiAction::MicPreamp { action } => match action.unwrap_or_default() {
            VolumeAction::Get => {
                let preamp = core.ui_get_mic_preamp().await?;
                println!("{}", preamp);
                Ok(())
            }
            VolumeAction::Set { value } => {
                let preamp = core.ui_set_mic_preamp(value).await?;
                println!("Mic preamp set to {}", preamp);
                Ok(())
            }
            VolumeAction::Up { amount } => {
                let preamp = core
                    .ui_adjust_mic_preamp(AdjDirection::Up, amount.unwrap_or(1))
                    .await?;
                println!("Mic preamp set to {}", preamp);
                Ok(())
            }
            VolumeAction::Down { amount } => {
                let preamp = core
                    .ui_adjust_mic_preamp(AdjDirection::Down, amount.unwrap_or(1))
                    .await?;
                println!("Mic preamp set to {}", preamp);
                Ok(())
            }
        },
    }
}

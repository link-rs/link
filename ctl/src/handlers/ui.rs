//! UI chip command handlers.

use crate::{App, GetSetHex, GetSetU32, LoopbackAction, UiAction};
use link::LoopbackMode;
use indicatif::{ProgressBar, ProgressStyle};
use link::ctl::{FlashPhase, SetTimeout};
use std::io::Write;
use std::thread;
use std::time::Duration;

pub fn handle_ui(action: UiAction, app: &mut App) -> Result<(), Box<dyn std::error::Error>> {
    match action {
        UiAction::Ping { data } => {
            println!("Sending UI ping with data: {}", data);
            app.ui_ping(data.as_bytes())?;
            println!("Received pong!");
            Ok(())
        }
        UiAction::Info => {
            println!("UI Bootloader Info");
            println!("==================\n");
            println!("Resetting UI chip to bootloader mode...");

            let delay = |ms| thread::sleep(Duration::from_millis(ms));
            let info = app
                .get_ui_bootloader_info(delay)
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

            // Hold NET chip in reset during UI flashing to avoid interference
            println!("Holding NET chip in reset...");
            if let Err(e) = app.hold_net_reset() {
                eprintln!("Warning: failed to hold NET in reset: {}", e);
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
            let delay = |ms| thread::sleep(Duration::from_millis(ms));
            let verify = !no_verify;
            let result = app.flash_ui(&firmware, delay, verify, |phase, progress, total| {
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
            });

            pb.finish_and_clear();

            // Release NET chip from reset
            println!("Releasing NET chip from reset...");
            if let Err(e) = app.reset_net_to_user() {
                eprintln!("Warning: failed to release NET from reset: {}", e);
            }

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
                let version = app.get_version()?;
                println!("{}", version);
                Ok(())
            }
            GetSetU32::Set { value } => {
                app.set_version(value)?;
                println!("Version set to {}", value);
                Ok(())
            }
        },
        UiAction::SFrameKey { action } => match action.unwrap_or_default() {
            GetSetHex::Get => {
                let key = app.get_sframe_key()?;
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
                app.set_sframe_key(&key_array)?;
                println!("SFrame key set to {}", value);
                Ok(())
            }
        },
        UiAction::Loopback { action } => match action.unwrap_or_default() {
            LoopbackAction::Get => {
                let mode = app.ui_get_loopback()?;
                println!("{:?}", mode);
                Ok(())
            }
            LoopbackAction::Off => {
                app.ui_set_loopback(LoopbackMode::Off)?;
                println!("UI loopback: off");
                Ok(())
            }
            LoopbackAction::Raw => {
                app.ui_set_loopback(LoopbackMode::Raw)?;
                println!("UI loopback: raw");
                Ok(())
            }
            LoopbackAction::Alaw => {
                app.ui_set_loopback(LoopbackMode::Alaw)?;
                println!("UI loopback: alaw");
                Ok(())
            }
            LoopbackAction::Sframe => {
                app.ui_set_loopback(LoopbackMode::Sframe)?;
                println!("UI loopback: sframe");
                Ok(())
            }
        },
        UiAction::Reset { action } => match action.as_deref() {
            Some("hold") => {
                app.hold_ui_reset()?;
                println!("UI chip held in reset");
                Ok(())
            }
            Some("release") => {
                app.reset_ui_to_user()?;
                println!("UI chip released from reset");
                Ok(())
            }
            _ => {
                app.reset_ui_to_user()?;
                println!("UI chip reset");
                Ok(())
            }
        },
        UiAction::Monitor { reset } => {
            if reset {
                println!("Resetting UI chip...");
                app.reset_ui_to_user()?;
            }
            println!("Monitoring UI chip logs (ESC to stop)...\n");

            // Set a short timeout for non-blocking reads
            if let Err(e) = app
                .reader_mut()
                .inner_mut()
                .set_timeout(Duration::from_millis(100))
            {
                eprintln!("Warning: couldn't set timeout: {}", e);
            }

            use crossterm::event::{self, Event, KeyCode, KeyEvent};
            use crossterm::terminal;

            // Enable raw mode to capture ESC
            terminal::enable_raw_mode()?;

            let result = (|| {
                loop {
                    // Check for key press (non-blocking)
                    if event::poll(Duration::from_millis(0))? {
                        if let Event::Key(KeyEvent {
                            code: KeyCode::Esc, ..
                        }) = event::read()?
                        {
                            return Ok(());
                        }
                    }

                    // Check for TLV data from UI
                    match app.reader_mut().read_ui_log() {
                        Ok(Some(msg)) => {
                            // Use \r\n for raw terminal mode
                            print!("[UI] {}\r\n", msg);
                            std::io::stdout().flush().ok();
                        }
                        Ok(None) => {
                            // Timeout or non-log TLV, continue
                        }
                        Err(e) => {
                            // Check if it's just a timeout
                            if let link::ctl::TlvReadError::Io(ref io_err) = e {
                                if io_err.kind() == std::io::ErrorKind::TimedOut {
                                    continue;
                                }
                            }
                            return Err(format!("Read error: {:?}", e).into());
                        }
                    }
                }
            })();

            // Always restore terminal mode and timeout
            terminal::disable_raw_mode()?;

            // Restore timeout to normal (3 seconds)
            if let Err(e) = app
                .reader_mut()
                .inner_mut()
                .set_timeout(Duration::from_secs(3))
            {
                eprintln!("Warning: couldn't restore timeout: {}", e);
            }

            println!("\nMonitor stopped.");

            result
        }
    }
}

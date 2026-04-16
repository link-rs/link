//! MGMT chip command handlers.

use super::Core;
use crate::{GetSetU8, MgmtAction, StackAction};
use indicatif::{ProgressBar, ProgressStyle};
use link::ctl::SetTimeout;
use link::ctl::flash::{FlashPhase, MgmtBootloaderEntry};
use link::protocol_config::timeouts;
use std::io::Write;
use std::time::Duration;
use tokio_serial::SerialPort;

/// Enter MGMT bootloader mode, handling auto-reset and manual fallback.
///
/// Returns whether init should be skipped (true if auto-reset succeeded).
pub(super) async fn enter_mgmt_bootloader(
    core: &mut Core,
) -> Result<bool, Box<dyn std::error::Error>> {
    // Switch to bootloader baud rate
    println!("Switching to bootloader baud rate (115200)...");
    core.port_mut().get_mut().set_baud_rate(115200)?;

    print!("Attempting automatic bootloader entry... ");
    std::io::stdout().flush()?;

    // Set short timeout for probing
    let _ = core
        .port_mut()
        .set_timeout(Duration::from_millis(timeouts::BOOTLOADER_PROBE_MS));

    let delay_ms = |ms| tokio::time::sleep(Duration::from_millis(ms));
    let skip_init = match core.try_enter_mgmt_bootloader(delay_ms).await {
        MgmtBootloaderEntry::AutoReset => {
            println!("success (EV16 detected)");
            true
        }
        MgmtBootloaderEntry::AlreadyActive => {
            println!("bootloader already active");
            true
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
            false
        }
    };

    // Restore normal timeout
    let _ = core
        .port_mut()
        .set_timeout(Duration::from_secs(timeouts::NORMAL_SECS));

    Ok(skip_init)
}

pub(super) async fn exit_mgmt_bootloader(
    core: &mut Core,
) -> Result<(), Box<dyn std::error::Error>> {
    let delay_ms = |ms| tokio::time::sleep(Duration::from_millis(ms));
    core.exit_mgmt_bootloader(delay_ms).await;

    core.port_mut()
        .get_mut()
        .set_baud_rate(link::uart_config::HIGH_SPEED.baudrate)?;

    // Drain any stale data from bootloader and wait for MGMT to be ready
    core.drain();
    core.wait_for_mgmt_ready(10).await;

    Ok(())
}

async fn handle_version(
    action: Option<GetSetU8>,
    core: &mut Core,
) -> Result<(), Box<dyn std::error::Error>> {
    let skip_init = enter_mgmt_bootloader(core).await?;

    let result = async {
        match action.unwrap_or_default() {
            GetSetU8::Get => {
                let value = core
                    .get_mgmt_data0_option_byte(skip_init)
                    .await
                    .map_err(|e| format!("Failed to read MGMT DATA0 option byte: {:?}", e))?;
                println!("{}", value);
            }
            GetSetU8::Set { value } => {
                core.set_mgmt_data0_option_byte(skip_init, value)
                    .await
                    .map_err(|e| format!("Failed to write MGMT DATA0 option byte: {:?}", e))?;
                println!("Version set to {}", value);
            }
        }

        Ok::<(), Box<dyn std::error::Error>>(())
    }
    .await;

    exit_mgmt_bootloader(core).await?;
    result
}

pub async fn handle_mgmt(
    action: MgmtAction,
    core: &mut Core,
) -> Result<(), Box<dyn std::error::Error>> {
    let needs_mgmt_firmware = matches!(
        &action,
        MgmtAction::Ping { .. } | MgmtAction::Board | MgmtAction::Stack { .. }
    );

    if needs_mgmt_firmware && !core.wait_for_mgmt_ready(50).await {
        return Err("MGMT chip not responding (timed out)".into());
    }

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

            let skip_init = enter_mgmt_bootloader(core).await?;

            println!("Querying bootloader information...\n");

            let info = match core.get_mgmt_bootloader_info(skip_init).await {
                Ok(info) => info,
                Err(e) => {
                    eprintln!("Failed to get bootloader info: {:?}", e);
                    eprintln!("\nMake sure the MGMT chip is in bootloader mode:");
                    eprintln!("  1. Set BOOT0 pin high");
                    eprintln!("  2. Reset the device");
                    return Err("Bootloader communication failed".into());
                }
            };

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

            println!(
                "Switching to normal operation baud rate ({})...",
                link::uart_config::HIGH_SPEED.baudrate
            );
            exit_mgmt_bootloader(core).await?;

            println!("\nDone!");
            Ok(())
        }
        MgmtAction::Board => {
            let version = core.mgmt_get_board_version().await?;
            println!("{}", version);
            Ok(())
        }
        MgmtAction::Version { action } => handle_version(action, core).await,
        MgmtAction::Flash { file } => {
            println!("MGMT Flash");
            println!("==========\n");

            let firmware = std::fs::read(&file)?;
            println!("Firmware: {} ({} bytes)", file.display(), firmware.len());

            let skip_init = enter_mgmt_bootloader(core).await?;

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
            let delay_ms = |ms| tokio::time::sleep(Duration::from_millis(ms));
            let result = core
                .flash_mgmt(
                    &firmware,
                    skip_init,
                    |phase, progress, total| {
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
                    },
                    delay_ms,
                )
                .await;

            pb.finish_and_clear();

            match result {
                Ok(()) => {
                    // Exit bootloader and reset to user code
                    exit_mgmt_bootloader(core).await?;

                    println!("\nFlash complete!");
                    println!(
                        "The MGMT chip should now be running the new firmware at {} baud.",
                        link::uart_config::HIGH_SPEED.baudrate
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
        MgmtAction::Stack { action } => match action.unwrap_or_default() {
            StackAction::Info => {
                let info = core.mgmt_get_stack_info().await?;
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
                core.mgmt_repaint_stack().await?;
                println!("Stack repainted");
                Ok(())
            }
        },
    }
}

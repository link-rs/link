//! NET chip command handlers.

use super::Core;
use crate::{
    GetSetString, LogsAction, NetAction, NetLoopbackAction, PinAction, PinLevel, ResetAction,
    WifiAction,
};
use indicatif::{ProgressBar, ProgressStyle};
use link::ctl::flash::StdDelay;
use link::ctl::{ProgressCallbacks, SetTimeout, escape_non_ascii};
use link::protocol_config::timeouts;
use link::{NetLoopbackMode, Pin, PinValue};
use std::io::Write;

/// Progress handler for NET chip flashing that wraps an indicatif ProgressBar.
struct FlashProgress {
    pb: ProgressBar,
    verifying: bool,
}

impl FlashProgress {
    fn new() -> Self {
        let pb = ProgressBar::new(0);
        pb.set_style(
            ProgressStyle::default_bar()
                .template("{prefix:>12} [{bar:40.cyan/blue}] {pos}/{len} ({percent}%)")
                .unwrap()
                .progress_chars("#>-"),
        );
        Self {
            pb,
            verifying: false,
        }
    }

    fn finish(self) {
        self.pb.finish_and_clear();
    }
}

impl ProgressCallbacks for FlashProgress {
    fn init(&mut self, _addr: u32, total: usize) {
        if !self.verifying {
            self.pb.set_prefix("Writing");
        }
        self.pb.set_length(total as u64);
        self.pb.set_position(0);
    }

    fn update(&mut self, current: usize) {
        self.pb.set_position(current as u64);
    }

    fn verifying(&mut self) {
        self.verifying = true;
        self.pb.set_prefix("Verifying");
        self.pb.set_position(0);
    }

    fn finish(&mut self, skipped: bool) {
        if skipped {
            self.pb.set_prefix("Skipped");
            self.pb.set_position(self.pb.length().unwrap_or(0));
        }
    }
}

pub async fn handle_net(
    action: NetAction,
    core: &mut Core,
) -> Result<(), Box<dyn std::error::Error>> {
    // Commands that communicate with NET firmware need it to be booted and ready.
    // The NET chip (ESP32-S3) is reset each time MGMT boots, and takes several
    // seconds to initialize (especially if connecting to WiFi).
    let needs_net_firmware = matches!(
        &action,
        NetAction::Ping { .. }
            | NetAction::Wifi { .. }
            | NetAction::RelayUrl { .. }
            | NetAction::Loopback { .. }
            | NetAction::Logs { .. }
            | NetAction::Language { .. }
            | NetAction::Channel { .. }
            | NetAction::Ai { .. }
            | NetAction::ClearStorage
            | NetAction::BurnJtagEfuse { .. }
    );

    if needs_net_firmware {
        // NET chip (ESP32-S3) takes several seconds to boot after MGMT releases
        // it from reset. Wait up to 30 seconds for WiFi connection + initialization.
        if !core.wait_for_net_ready(30).await {
            return Err("NET chip did not respond (is firmware flashed?)".into());
        }
    }

    match action {
        NetAction::Ping { data } => {
            println!("Sending NET ping with data: {}", data);
            core.net_ping(data.as_bytes()).await?;
            println!("Received pong!");
            Ok(())
        }
        NetAction::Info => {
            println!("Querying NET chip info...");
            let info = core
                .get_net_bootloader_info(StdDelay)
                .await
                .map_err(|e| format!("Failed to get bootloader info: {:?}", e))?;

            let dev = &info.device_info;
            let sec = &info.security_info;

            println!("\nNET Bootloader Info");
            println!("===================\n");

            // Device info
            println!("Chip Type:         {:?}", dev.chip);
            if let Some((major, minor)) = dev.revision {
                println!("Chip Revision:     {}.{}", major, minor);
            }
            println!("Crystal Freq:      {:?}", dev.crystal_frequency);
            println!("Flash Size:        {:?}", dev.flash_size);
            if !dev.features.is_empty() {
                println!("Features:          {}", dev.features.join(", "));
            }
            if let Some(ref mac) = dev.mac_address {
                println!("MAC Address:       {}", mac);
            }

            // Security info
            println!("\nSecurity Info:");
            println!("---------------");
            if let Some(chip_id) = sec.chip_id {
                println!("Chip ID:           {} (0x{:04X})", chip_id, chip_id);
            }
            if let Some(eco_ver) = sec.eco_version {
                println!("ECO Version:       {}", eco_ver);
            }
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

            let (secure_boot, flash_encryption) = link::ctl::interpret_esp32_security(sec);
            println!(
                "Secure Boot:       {}",
                if secure_boot { "enabled" } else { "disabled" }
            );
            println!(
                "Flash Encryption:  {}",
                if flash_encryption {
                    "enabled"
                } else {
                    "disabled"
                }
            );

            println!("\nNET chip reset back to user mode.");
            println!("Done!");
            Ok(())
        }
        NetAction::Wifi { action } => match action {
            None => {
                let ssids = core.get_wifi_ssids().await?;
                if ssids.is_empty() {
                    println!("No WiFi networks configured");
                } else {
                    for wifi in ssids {
                        println!("{}\t{}", wifi.ssid, wifi.password);
                    }
                }
                Ok(())
            }
            Some(WifiAction::Add { ssid, password }) => {
                core.add_wifi_ssid(&ssid, &password).await?;
                println!("Added WiFi network: {}", ssid);
                Ok(())
            }
            Some(WifiAction::Clear) => {
                core.clear_wifi_ssids().await?;
                println!("Cleared all WiFi networks");
                Ok(())
            }
        },
        NetAction::RelayUrl { action } => match action.unwrap_or_default() {
            GetSetString::Get => {
                let url = core.get_relay_url().await?;
                println!("{}", url);
                Ok(())
            }
            GetSetString::Set { value } => {
                core.set_relay_url(&value).await?;
                println!("Relay URL set to {}", value);
                Ok(())
            }
        },
        NetAction::Flash {
            file,
            partition_table,
        } => {
            println!("NET Flash (ESP32) - using espflash");
            println!("===================================\n");

            let firmware = std::fs::read(&file)?;
            println!("Firmware: {} ({} bytes)", file.display(), firmware.len());
            let partition_table_data = if let Some(ref pt) = partition_table {
                println!("Partition table: {}", pt.display());
                println!("  (app address determined by partition table)");
                Some(std::fs::read(pt)?)
            } else {
                println!("Partition table: default (single app at 0x10000)");
                None
            };

            println!("Flashing NET chip...\n");

            let mut progress = FlashProgress::new();
            let result = core
                .flash_net(
                    &firmware,
                    partition_table_data.as_deref(),
                    &mut progress,
                    StdDelay,
                    link::uart_config::HIGH_SPEED.baudrate,
                )
                .await;

            progress.finish();

            match result {
                Ok(()) => {
                    println!("Flash complete!");
                    println!("NET chip reset back to user mode.");
                    Ok(())
                }
                Err(e) => Err(format!("Flash failed: {}", e).into()),
            }
        }
        NetAction::Loopback { mode } => match mode.unwrap_or_default() {
            NetLoopbackAction::Get => {
                let loopback = core.net_get_loopback().await?;
                println!("{}", loopback);
                Ok(())
            }
            NetLoopbackAction::Off => {
                core.net_set_loopback(NetLoopbackMode::Off).await?;
                println!("NET loopback: off (normal PTT)");
                Ok(())
            }
            NetLoopbackAction::Raw => {
                core.net_set_loopback(NetLoopbackMode::Raw).await?;
                println!("NET loopback: raw (local bypass)");
                Ok(())
            }
            NetLoopbackAction::Moq => {
                core.net_set_loopback(NetLoopbackMode::Moq).await?;
                println!("NET loopback: moq (hear own audio via relay)");
                Ok(())
            }
        },
        NetAction::Boot {
            action: PinAction::Set { level },
        } => {
            let value = match level {
                PinLevel::High => PinValue::High,
                PinLevel::Low => PinValue::Low,
            };
            core.write_tlv_raw(link::CtlToMgmt::SetPin, &[Pin::NetBoot as u8, value as u8])
                .await?;
            println!("NET BOOT: {:?}", value);
            Ok(())
        }
        NetAction::Rst {
            action: PinAction::Set { level },
        } => {
            let value = match level {
                PinLevel::High => PinValue::High,
                PinLevel::Low => PinValue::Low,
            };
            core.write_tlv_raw(link::CtlToMgmt::SetPin, &[Pin::NetRst as u8, value as u8])
                .await?;
            println!("NET RST: {:?}", value);
            Ok(())
        }
        NetAction::Reset { action } => {
            let delay = |ms| tokio::time::sleep(std::time::Duration::from_millis(ms));
            match action.unwrap_or_default() {
                ResetAction::User => {
                    core.reset_net_to_user(delay).await?;
                    println!("NET chip reset to user mode");
                }
                ResetAction::Bootloader => {
                    core.reset_net_to_bootloader(delay).await?;
                    println!("NET chip reset to bootloader mode");
                }
                ResetAction::Hold => {
                    core.hold_net_reset().await?;
                    println!("NET chip held in reset");
                }
                ResetAction::Release => {
                    core.write_tlv_raw(
                        link::CtlToMgmt::SetPin,
                        &[Pin::NetRst as u8, PinValue::High as u8],
                    )
                    .await?;
                    println!("NET chip released from reset");
                }
            }
            Ok(())
        }
        NetAction::Erase => {
            println!("Erasing NET chip flash...");
            match core.erase_net(StdDelay).await {
                Ok(()) => {
                    println!("Flash erased successfully");
                    Ok(())
                }
                Err(e) => Err(format!("Failed to erase flash: {}", e).into()),
            }
        }
        NetAction::Monitor { reset } => {
            if reset {
                println!("Resetting NET chip...");
                let delay = |ms| tokio::time::sleep(std::time::Duration::from_millis(ms));
                core.reset_net_to_user(delay).await?;
            }
            println!("Monitoring NET chip (ESC to stop)...\n");

            // Set a short timeout for non-blocking reads
            if let Err(e) = core
                .port_mut()
                .set_timeout(std::time::Duration::from_millis(timeouts::MONITOR_MS))
            {
                eprintln!("Warning: couldn't set timeout: {}", e);
            }

            use crossterm::event::{self, Event, KeyCode, KeyEvent};
            use crossterm::terminal;
            use std::io::Write;

            // Enable raw mode to capture ESC
            terminal::enable_raw_mode()?;

            let result = async {
                loop {
                    // Check for key press (non-blocking)
                    if event::poll(std::time::Duration::from_millis(0))? {
                        if let Event::Key(KeyEvent {
                            code: KeyCode::Esc, ..
                        }) = event::read()?
                        {
                            return Ok::<(), Box<dyn std::error::Error>>(());
                        }
                    }

                    // Check for TLV data (timeout-aware: returns Ok(None) on timeout)
                    match core.read_tlv_raw().await {
                        Ok(Some(tlv)) => {
                            if tlv.tlv_type == link::MgmtToCtl::FromNet {
                                let text = escape_non_ascii(&tlv.value);
                                print!("{}", text);
                                std::io::stdout().flush().ok();
                            }
                        }
                        Ok(None) => {
                            // Timeout, continue
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
                .set_timeout(std::time::Duration::from_secs(timeouts::NORMAL_SECS))
            {
                eprintln!("Warning: couldn't restore timeout: {}", e);
            }

            println!("\nMonitor stopped.");

            result
        }
        NetAction::Logs { action } => match action.unwrap_or_default() {
            LogsAction::Get => {
                let enabled = core.net_get_logs_enabled().await?;
                println!("{}", if enabled { "on" } else { "off" });
                Ok(())
            }
            LogsAction::On => {
                core.net_set_logs_enabled(true).await?;
                println!("NET logs: on");
                Ok(())
            }
            LogsAction::Off => {
                core.net_set_logs_enabled(false).await?;
                println!("NET logs: off");
                Ok(())
            }
        },
        NetAction::Language { action } => match action.unwrap_or_default() {
            GetSetString::Get => {
                let lang = core.net_get_language().await?;
                println!("{}", lang);
                Ok(())
            }
            GetSetString::Set { value } => {
                core.net_set_language(&value).await?;
                println!("Language set to {}", value);
                Ok(())
            }
        },
        NetAction::Channel { action } => match action.unwrap_or_default() {
            GetSetString::Get => {
                let channel = core.net_get_channel().await?;
                println!("{}", channel);
                Ok(())
            }
            GetSetString::Set { value } => {
                core.net_set_channel(&value).await?;
                println!("Channel set");
                Ok(())
            }
        },
        NetAction::Ai { action } => match action.unwrap_or_default() {
            GetSetString::Get => {
                let config = core.net_get_ai().await?;
                println!("{}", config);
                Ok(())
            }
            GetSetString::Set { value } => {
                core.net_set_ai(&value).await?;
                println!("AI config set");
                Ok(())
            }
        },
        NetAction::ClearStorage => {
            core.net_clear_storage().await?;
            println!("NET storage cleared");
            Ok(())
        }
        NetAction::BurnJtagEfuse { yes } => {
            if !yes {
                println!("WARNING: This operation is IRREVERSIBLE!");
                println!("It will permanently disable JTAG/USB debugging on this device.");
                println!("");
                print!("Type 'BURN' to confirm: ");
                std::io::stdout().flush()?;

                let mut input = String::new();
                std::io::stdin().read_line(&mut input)?;

                if input.trim() != "BURN" {
                    println!("Aborted.");
                    return Ok(());
                }
            }

            match core.net_burn_jtag_efuse().await {
                Ok(()) => {
                    println!("JTAG/USB disable efuse burned successfully.");
                    Ok(())
                }
                Err(e) => {
                    println!("Failed to burn efuse: {}", e);
                    Err(e.into())
                }
            }
        }
    }
}

//! NET chip command handlers.

use super::Core;
use crate::{
    ChannelAction, GetSetString, NetAction, NetLoopbackAction, PinAction, PinLevel, ResetAction,
    WifiAction,
};
use indicatif::{ProgressBar, ProgressStyle};
use link::ctl::flash::StdDelay;
use link::ctl::{ChannelConfig, ProgressCallbacks, SetTimeout};
use link::NetLoopbackMode;

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
                    1_000_000,
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
                match loopback {
                    NetLoopbackMode::Off => println!("off"),
                    NetLoopbackMode::Raw => println!("raw"),
                    NetLoopbackMode::Moq => println!("moq"),
                }
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
                PinLevel::High => link::PinValue::High,
                PinLevel::Low => link::PinValue::Low,
            };
            core.write_tlv_raw(
                link::CtlToMgmt::SetPin,
                &[link::Pin::NetBoot as u8, value as u8],
            )
            .await?;
            println!("NET BOOT: {:?}", value);
            Ok(())
        }
        NetAction::Rst {
            action: PinAction::Set { level },
        } => {
            let value = match level {
                PinLevel::High => link::PinValue::High,
                PinLevel::Low => link::PinValue::Low,
            };
            core.write_tlv_raw(
                link::CtlToMgmt::SetPin,
                &[link::Pin::NetRst as u8, value as u8],
            )
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
                        &[link::Pin::NetRst as u8, link::PinValue::High as u8],
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
                .set_timeout(std::time::Duration::from_millis(100))
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
                                std::io::stdout().write_all(&tlv.value).ok();
                                std::io::stdout().flush().ok();
                            }
                        }
                        Ok(None) => {
                            // Timeout, continue
                        }
                        Err(e) => {
                            // Check if it's a timeout error (can happen during read_exact)
                            if let link::ctl::CtlError::Port(msg) = &e {
                                if msg.contains("TimedOut") || msg.contains("timeout") {
                                    // Timeout during partial read, continue
                                    continue;
                                }
                            }
                            return Err(format!("Read error: {:?}", e).into());
                        }
                    }
                }
            }
            .await;

            // Always restore terminal mode and timeout
            terminal::disable_raw_mode()?;

            // Restore timeout to normal (3 seconds)
            if let Err(e) = core
                .port_mut()
                .set_timeout(std::time::Duration::from_secs(3))
            {
                eprintln!("Warning: couldn't restore timeout: {}", e);
            }

            println!("\nMonitor stopped.");

            result
        }
        NetAction::Channel { action } => match action {
            None => {
                // List all channel configs by querying each known channel
                let channel_ids = [0u8, 1, 3]; // Ptt, PttAi, ChatAi
                let mut found_any = false;
                for &id in &channel_ids {
                    match core.get_channel_config(id).await {
                        Ok(config) => {
                            if !found_any {
                                println!("Channel configurations:");
                                found_any = true;
                            }
                            let channel_name = match config.channel_id {
                                0 => "Ptt",
                                1 => "PttAi",
                                3 => "ChatAi",
                                _ => "Unknown",
                            };
                            println!(
                                "  {} ({}): enabled={}, relay_url={}",
                                config.channel_id,
                                channel_name,
                                config.enabled,
                                if config.relay_url.is_empty() {
                                    "(global)"
                                } else {
                                    config.relay_url.as_str()
                                }
                            );
                        }
                        Err(_) => {
                            // Channel not configured, skip
                        }
                    }
                }
                if !found_any {
                    println!("No channel configurations");
                }
                Ok(())
            }
            Some(ChannelAction::Get { channel_id }) => {
                let config = core.get_channel_config(channel_id).await?;
                let channel_name = match config.channel_id {
                    0 => "Ptt",
                    1 => "PttAi",
                    3 => "ChatAi",
                    _ => "Unknown",
                };
                println!("Channel {} ({}):", config.channel_id, channel_name);
                println!("  enabled: {}", config.enabled);
                println!(
                    "  relay_url: {}",
                    if config.relay_url.is_empty() {
                        "(global)"
                    } else {
                        config.relay_url.as_str()
                    }
                );
                Ok(())
            }
            Some(ChannelAction::Set {
                channel_id,
                enabled,
                relay_url,
            }) => {
                let config = ChannelConfig {
                    channel_id,
                    enabled,
                    relay_url: relay_url
                        .as_str()
                        .try_into()
                        .map_err(|_| "relay_url too long")?,
                };
                core.set_channel_config(&config).await?;
                println!("Channel {} configuration updated", channel_id);
                Ok(())
            }
            Some(ChannelAction::Clear) => {
                core.clear_channel_configs().await?;
                println!("All channel configurations cleared");
                Ok(())
            }
        },
        NetAction::JitterStats { channel_id } => {
            let stats = core.get_jitter_stats(channel_id).await?;
            let channel_name = match channel_id {
                0 => "Ptt",
                1 => "PttAi",
                3 => "ChatAi",
                _ => "Unknown",
            };
            let state_name = match stats.state {
                0 => "Buffering",
                1 => "Playing",
                _ => "Unknown",
            };
            println!(
                "Jitter buffer stats for channel {} ({}):",
                channel_id, channel_name
            );
            println!("  received:  {}", stats.received);
            println!("  output:    {}", stats.output);
            println!("  underruns: {}", stats.underruns);
            println!("  overruns:  {}", stats.overruns);
            println!("  level:     {}", stats.level);
            println!("  state:     {} ({})", stats.state, state_name);
            Ok(())
        }
    }
}

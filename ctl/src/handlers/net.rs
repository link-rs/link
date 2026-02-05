//! NET chip command handlers.

use super::Core;
use crate::{ChannelAction, GetSetString, NetAction, NetLoopbackMode, WifiAction};
use futures::executor::block_on;
use indicatif::{ProgressBar, ProgressStyle};
use link::ctl::{ChannelConfig, ProgressCallbacks};
use link::NetLoopback;

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

    fn finish(&mut self, _skipped: bool) {
        // Progress bar will be cleared by FlashProgress::finish()
    }
}

pub fn handle_net(action: NetAction, core: &mut Core) -> Result<(), Box<dyn std::error::Error>> {
    match action {
        NetAction::Ping { data } => {
            println!("Sending NET ping with data: {}", data);
            block_on(core.net_ping(data.as_bytes()))?;
            println!("Received pong!");
            Ok(())
        }
        NetAction::Info => {
            println!("Querying NET chip info...");
            let info = block_on(core.get_net_bootloader_info())
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
                let ssids = block_on(core.get_wifi_ssids())?;
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
                block_on(core.add_wifi_ssid(&ssid, &password))?;
                println!("Added WiFi network: {}", ssid);
                Ok(())
            }
            Some(WifiAction::Clear) => {
                block_on(core.clear_wifi_ssids())?;
                println!("Cleared all WiFi networks");
                Ok(())
            }
        },
        NetAction::RelayUrl { action } => match action.unwrap_or_default() {
            GetSetString::Get => {
                let url = block_on(core.get_relay_url())?;
                println!("{}", url);
                Ok(())
            }
            GetSetString::Set { value } => {
                block_on(core.set_relay_url(&value))?;
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

            // Hold UI chip in reset during NET flashing to avoid interference
            println!("Holding UI chip in reset...");
            if let Err(e) = block_on(core.hold_ui_reset()) {
                eprintln!("Warning: failed to hold UI in reset: {}", e);
            }

            println!("Resetting NET chip to bootloader mode...\n");

            let mut progress = FlashProgress::new();
            let result = block_on(core.flash_net(&firmware, partition_table_data.as_deref(), &mut progress));

            progress.finish();

            // Release UI chip from reset
            println!("Releasing UI chip from reset...");
            if let Err(e) = block_on(core.reset_ui_to_user()) {
                eprintln!("Warning: failed to release UI from reset: {}", e);
            }

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
            NetLoopbackMode::Get => {
                let loopback = block_on(core.net_get_loopback())?;
                match loopback {
                    NetLoopback::Off => println!("off"),
                    NetLoopback::Raw => println!("raw"),
                    NetLoopback::Moq => println!("moq"),
                }
                Ok(())
            }
            NetLoopbackMode::Off => {
                block_on(core.net_set_loopback(NetLoopback::Off))?;
                println!("NET loopback: off (normal PTT)");
                Ok(())
            }
            NetLoopbackMode::Raw => {
                block_on(core.net_set_loopback(NetLoopback::Raw))?;
                println!("NET loopback: raw (local bypass)");
                Ok(())
            }
            NetLoopbackMode::Moq => {
                block_on(core.net_set_loopback(NetLoopback::Moq))?;
                println!("NET loopback: moq (hear own audio via relay)");
                Ok(())
            }
        },
        NetAction::Chat { message } => match block_on(core.send_chat_message(&message)) {
            Ok(()) => {
                println!("Chat message sent");
                Ok(())
            }
            Err(e) => Err(format!("Failed to send chat message: {}", e).into()),
        },
        NetAction::Reset { action } => match action.as_deref() {
            Some("bootloader") => {
                block_on(core.reset_net_to_bootloader())?;
                println!("NET chip reset to bootloader mode");
                Ok(())
            }
            _ => {
                block_on(core.reset_net_to_user())?;
                println!("NET chip reset");
                Ok(())
            }
        },
        NetAction::Erase => {
            println!("Erasing NET chip flash...");
            match block_on(core.erase_net()) {
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
                block_on(core.reset_net_to_user())?;
            }
            println!("Monitoring NET chip (ESC to stop)...\n");

            // Set a short timeout for non-blocking reads
            if let Err(e) = core.port_mut().get_mut().get_mut().set_timeout(std::time::Duration::from_millis(100)) {
                eprintln!("Warning: couldn't set timeout: {}", e);
            }

            use crossterm::event::{self, Event, KeyCode, KeyEvent};
            use crossterm::terminal;
            use std::io::Write;

            // Enable raw mode to capture ESC
            terminal::enable_raw_mode()?;

            let result = (|| {
                loop {
                    // Check for key press (non-blocking)
                    if event::poll(std::time::Duration::from_millis(0))? {
                        if let Event::Key(KeyEvent {
                            code: KeyCode::Esc, ..
                        }) = event::read()?
                        {
                            return Ok(());
                        }
                    }

                    // Check for TLV data (timeout-aware: returns Ok(None) on timeout)
                    match block_on(core.read_tlv_raw()) {
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
                            return Err(format!("Read error: {:?}", e).into());
                        }
                    }
                }
            })();

            // Always restore terminal mode and timeout
            terminal::disable_raw_mode()?;

            // Restore timeout to normal (3 seconds)
            if let Err(e) = core.port_mut().get_mut().get_mut().set_timeout(std::time::Duration::from_secs(3)) {
                eprintln!("Warning: couldn't restore timeout: {}", e);
            }

            println!("\nMonitor stopped.");

            result
        }
        NetAction::Channel { action } => match action {
            None => {
                // List all channel configs
                let configs = block_on(core.get_all_channel_configs())?;
                if configs.is_empty() {
                    println!("No channel configurations");
                } else {
                    println!("Channel configurations:");
                    for config in configs.iter() {
                        let channel_name = match config.channel_id {
                            0 => "Ptt",
                            1 => "PttAi",
                            3 => "ChatAi",
                            id => &format!("Unknown({})", id),
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
                }
                Ok(())
            }
            Some(ChannelAction::Get { channel_id }) => {
                let config = block_on(core.get_channel_config(channel_id))?;
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
                    relay_url: relay_url.as_str().try_into().map_err(|_| "relay_url too long")?,
                };
                block_on(core.set_channel_config(&config))?;
                println!("Channel {} configuration updated", channel_id);
                Ok(())
            }
            Some(ChannelAction::Clear) => {
                block_on(core.clear_channel_configs())?;
                println!("All channel configurations cleared");
                Ok(())
            }
        },
        NetAction::JitterStats { channel_id } => {
            let stats = block_on(core.get_jitter_stats(channel_id))?;
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
            println!("Jitter buffer stats for channel {} ({}):", channel_id, channel_name);
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

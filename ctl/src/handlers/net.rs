//! NET chip command handlers.

use crate::{App, ChannelAction, GetSetBool, GetSetString, GetSetU32, NetAction, WifiAction};
use link::ctl::ChannelConfig;
use indicatif::{ProgressBar, ProgressStyle};
use link::ctl::{ProgressCallbacks, SetTimeout};

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

pub fn handle_net(action: NetAction, app: &mut App) -> Result<(), Box<dyn std::error::Error>> {
    match action {
        NetAction::Ping { data } => {
            println!("Sending NET ping with data: {}", data);
            app.net_ping(data.as_bytes())?;
            println!("Received pong!");
            Ok(())
        }
        NetAction::Info => {
            println!("Querying NET chip info...");
            let info = app
                .get_net_bootloader_info()
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
                let ssids = app.get_wifi_ssids()?;
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
                app.add_wifi_ssid(&ssid, &password)?;
                println!("Added WiFi network: {}", ssid);
                Ok(())
            }
            Some(WifiAction::Clear) => {
                app.clear_wifi_ssids()?;
                println!("Cleared all WiFi networks");
                Ok(())
            }
        },
        NetAction::RelayUrl { action } => match action.unwrap_or_default() {
            GetSetString::Get => {
                let url = app.get_relay_url()?;
                println!("{}", url);
                Ok(())
            }
            GetSetString::Set { value } => {
                app.set_relay_url(&value)?;
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
            if let Some(ref pt) = partition_table {
                println!("Partition table: {}", pt.display());
                println!("  (app address determined by partition table)");
            } else {
                println!("Partition table: default (single app at 0x10000)");
            }

            // Hold UI chip in reset during NET flashing to avoid interference
            println!("Holding UI chip in reset...");
            if let Err(e) = app.hold_ui_reset() {
                eprintln!("Warning: failed to hold UI in reset: {}", e);
            }

            println!("Resetting NET chip to bootloader mode...\n");

            let mut progress = FlashProgress::new();
            let result = app.flash_net(&firmware, partition_table.as_deref(), &mut progress);

            progress.finish();

            // Release UI chip from reset
            println!("Releasing UI chip from reset...");
            if let Err(e) = app.reset_ui_to_user() {
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
        NetAction::WsPing { data } => {
            println!("Sending WebSocket ping with data: {}", data);
            app.ws_ping(data.as_bytes())?;
            println!("Received echo response!");
            Ok(())
        }
        NetAction::WsEchoTest => {
            println!("Running WebSocket echo test...");
            println!("  Sending 50 packets (640 bytes each) at 20ms intervals (50 fps)\n");

            let results = app.ws_echo_test()?;

            println!("Results:");
            println!("  Packets sent:           {}", results.sent);
            println!("  Packets received (raw): {}", results.received);
            println!("  Packets output (buf):   {}", results.buffered_output);
            println!("  Buffer underruns:       {}", results.underruns);

            if results.received > 0 && results.sent > 0 {
                let loss_pct =
                    ((results.sent - results.received) as f64 / results.sent as f64) * 100.0;
                println!("  Packet loss:            {:.1}%", loss_pct);
            }

            print_jitter_stats(
                "Raw jitter (before buffer)",
                results.raw_jitter_us.as_slice(),
            );
            print_jitter_stats(
                "Buffered jitter (after buffer)",
                results.buffered_jitter_us.as_slice(),
            );

            if !results.raw_jitter_us.is_empty() {
                println!("\nRaw timings (µs): {:?}", results.raw_jitter_us.as_slice());
            }
            if !results.buffered_jitter_us.is_empty() {
                println!(
                    "Buffered timings (µs): {:?}",
                    results.buffered_jitter_us.as_slice()
                );
            }

            Ok(())
        }
        NetAction::WsSpeedTest => {
            println!("Running WebSocket speed test...");
            println!("  Sending 50 packets (640 bytes each) as fast as possible\n");

            let results = app.ws_speed_test()?;

            println!("Results:");
            println!("  Packets sent:     {}", results.sent);
            println!("  Packets received: {}", results.received);
            println!("  Send time:        {} ms", results.send_time_ms);
            println!("  Receive time:     {} ms", results.recv_time_ms);

            if results.sent > 0 {
                let send_rate =
                    (results.sent as f64 * 640.0) / (results.send_time_ms as f64 / 1000.0) / 1024.0;
                println!("  Send rate:        {:.1} KB/s", send_rate);
                let fps = results.sent as f64 / (results.send_time_ms as f64 / 1000.0);
                println!("  Send FPS:         {:.1}", fps);
            }

            if results.received > 0 && results.sent > 0 {
                let loss_pct =
                    ((results.sent - results.received) as f64 / results.sent as f64) * 100.0;
                println!("  Packet loss:      {:.1}%", loss_pct);
            }

            Ok(())
        }
        NetAction::Loopback { action } => match action.unwrap_or_default() {
            GetSetBool::Get => {
                let enabled = app.net_get_loopback()?;
                println!("{}", enabled);
                Ok(())
            }
            GetSetBool::Set => {
                app.net_set_loopback(true)?;
                println!("NET loopback enabled");
                Ok(())
            }
            GetSetBool::Unset => {
                app.net_set_loopback(false)?;
                println!("NET loopback disabled");
                Ok(())
            }
        },
        // MoQ commands
        NetAction::BenchmarkFps { action } => match action.unwrap_or_default() {
            GetSetU32::Get => {
                let fps = app.get_benchmark_fps()?;
                if fps == 0 {
                    println!("0 (burst mode)");
                } else {
                    println!("{}", fps);
                }
                Ok(())
            }
            GetSetU32::Set { value } => {
                app.set_benchmark_fps(value)?;
                if value == 0 {
                    println!("Benchmark FPS set to burst mode");
                } else {
                    println!("Benchmark FPS set to {}", value);
                }
                Ok(())
            }
        },
        NetAction::BenchmarkPayloadSize { action } => match action.unwrap_or_default() {
            GetSetU32::Get => {
                let size = app.get_benchmark_payload_size()?;
                println!("{}", size);
                Ok(())
            }
            GetSetU32::Set { value } => {
                app.set_benchmark_payload_size(value)?;
                println!("Benchmark payload size set to {}", value);
                Ok(())
            }
        },
        NetAction::RunClock => match app.run_clock() {
            Ok(()) => {
                println!("Started clock mode (subscribing to clock track)");
                Ok(())
            }
            Err(e) => Err(format!("Failed to run clock: {}", e).into()),
        },
        NetAction::RunBenchmark => match app.run_benchmark() {
            Ok(()) => {
                println!("Started benchmark mode (publishing frames)");
                Ok(())
            }
            Err(e) => Err(format!("Failed to run benchmark: {}", e).into()),
        },
        NetAction::StopMode => match app.stop_mode() {
            Ok(()) => {
                println!("Stopped current mode");
                Ok(())
            }
            Err(e) => Err(format!("Failed to stop mode: {}", e).into()),
        },
        NetAction::RunMoqLoopback => match app.run_moq_loopback() {
            Ok(()) => {
                println!("Started MoQ loopback mode (publish and subscribe to same track)");
                Ok(())
            }
            Err(e) => Err(format!("Failed to run MoQ loopback: {}", e).into()),
        },
        NetAction::RunPublish => match app.run_publish() {
            Ok(()) => {
                println!("Started MoQ publish mode (publish only, no subscribe)");
                Ok(())
            }
            Err(e) => Err(format!("Failed to run MoQ publish: {}", e).into()),
        },
        NetAction::RunPtt => match app.run_ptt() {
            Ok(()) => {
                println!("Started PTT mode (hactar-compatible)");
                println!("  Button A -> PTT channel (gardening)");
                println!("  Button B -> AI channel");
                Ok(())
            }
            Err(e) => Err(format!("Failed to run PTT mode: {}", e).into()),
        },
        NetAction::Chat { message } => match app.send_chat_message(&message) {
            Ok(()) => {
                println!("Chat message sent");
                Ok(())
            }
            Err(e) => Err(format!("Failed to send chat message: {}", e).into()),
        },
        NetAction::Reset { action } => match action.as_deref() {
            Some("bootloader") => {
                app.reset_net_to_bootloader()?;
                println!("NET chip reset to bootloader mode");
                Ok(())
            }
            _ => {
                app.reset_net_to_user()?;
                println!("NET chip reset");
                Ok(())
            }
        },
        NetAction::Erase => {
            println!("Erasing NET chip flash...");
            match app.erase_net() {
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
                app.reset_net_to_user()?;
            }
            println!("Monitoring NET chip (ESC to stop)...\n");

            // Set a short timeout for non-blocking reads
            if let Err(e) = app
                .reader_mut()
                .inner_mut()
                .set_timeout(std::time::Duration::from_millis(100))
            {
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

                    // Check for TLV data
                    match app.reader_mut().read_tlv() {
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
                .set_timeout(std::time::Duration::from_secs(3))
            {
                eprintln!("Warning: couldn't restore timeout: {}", e);
            }

            println!("\nMonitor stopped.");

            result
        }
        NetAction::Channel { action } => match action {
            None => {
                // List all channel configs
                let configs = app.get_all_channel_configs()?;
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
                let config = app.get_channel_config(channel_id)?;
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
                app.set_channel_config(&config)?;
                println!("Channel {} configuration updated", channel_id);
                Ok(())
            }
            Some(ChannelAction::Clear) => {
                app.clear_channel_configs()?;
                println!("All channel configurations cleared");
                Ok(())
            }
        },
        NetAction::JitterStats { channel_id } => {
            let stats = app.get_jitter_stats(channel_id)?;
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

fn print_jitter_stats(label: &str, timings: &[u32]) {
    if timings.is_empty() {
        println!("\n{}: No data", label);
        return;
    }
    let min = timings.iter().min().copied().unwrap_or(0);
    let max = timings.iter().max().copied().unwrap_or(0);
    let sum: u64 = timings.iter().map(|&x| x as u64).sum();
    let avg = sum / timings.len() as u64;

    println!("\n{}:", label);
    println!("  Min: {:>6} µs ({:>5.1} ms)", min, min as f64 / 1000.0);
    println!("  Max: {:>6} µs ({:>5.1} ms)", max, max as f64 / 1000.0);
    println!("  Avg: {:>6} µs ({:>5.1} ms)", avg, avg as f64 / 1000.0);

    let target_us = 20000i64;
    let jitter: i64 = timings
        .iter()
        .map(|&x| (x as i64 - target_us).abs())
        .sum::<i64>()
        / timings.len() as i64;
    println!(
        "  Avg deviation from 20ms: {:>6} µs ({:>5.1} ms)",
        jitter,
        jitter as f64 / 1000.0
    );
}

//! NET chip command handlers.

use crate::{App, GetSetBool, GetSetString, NetAction, WifiAction};
use indicatif::{ProgressBar, ProgressStyle};
use link::ctl::FlashPhase;

pub async fn handle_net(
    action: NetAction,
    app: &mut App,
) -> Result<Option<String>, Box<dyn std::error::Error>> {
    match action {
        NetAction::Ping { data } => {
            println!("Sending NET ping with data: {}", data);
            app.net_ping(data.as_bytes()).await;
            Ok(Some("Received pong!".to_string()))
        }
        NetAction::Info => {
            println!("Resetting NET chip to bootloader mode...");
            let info = app
                .get_net_bootloader_info()
                .await
                .map_err(|e| format!("Failed to get bootloader info: {:?}", e))?;

            let sec = &info.security_info;
            println!("\nNET Bootloader Info");
            println!("===================\n");
            println!("Chip Type:         {}", sec.chip_type.name());
            println!("Chip ID:           {} (0x{:04X})", sec.chip_id, sec.chip_id);
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

            Ok(Some("NET chip reset back to user mode.\nDone!".to_string()))
        }
        NetAction::Wifi { action } => match action {
            None => {
                let ssids = app.get_wifi_ssids().await;
                if ssids.is_empty() {
                    Ok(Some("No WiFi networks configured".to_string()))
                } else {
                    let mut output = String::new();
                    for wifi in ssids {
                        output.push_str(&format!("{}\t{}\n", wifi.ssid, wifi.password));
                    }
                    Ok(Some(output.trim_end().to_string()))
                }
            }
            Some(WifiAction::Add { ssid, password }) => {
                app.add_wifi_ssid(&ssid, &password).await;
                Ok(Some(format!("Added WiFi network: {}", ssid)))
            }
            Some(WifiAction::Clear) => {
                app.clear_wifi_ssids().await;
                Ok(Some("Cleared all WiFi networks".to_string()))
            }
        },
        NetAction::RelayUrl { action } => match action.unwrap_or_default() {
            GetSetString::Get => {
                let url = app.get_relay_url().await;
                Ok(Some(url.to_string()))
            }
            GetSetString::Set { value } => {
                app.set_relay_url(&value).await;
                Ok(Some(format!("Relay URL set to {}", value)))
            }
        },
        NetAction::Flash {
            file,
            address,
            compress,
            no_verify,
        } => {
            println!("NET Flash (ESP32)");
            println!("=================\n");

            let address: u32 = if address.starts_with("0x") || address.starts_with("0X") {
                u32::from_str_radix(&address[2..], 16).map_err(|_| "Invalid hex address")?
            } else {
                address.parse().map_err(|_| "Invalid address")?
            };

            if address == 0x10000 {
                println!("Note: Using default app address 0x10000 (standard ESP-IDF layout)");
                println!("      Use --address to override if needed.\n");
            }

            if compress {
                println!("Mode: Compressed transfer enabled (-c)\n");
            }

            if no_verify {
                println!("Note: MD5 verification disabled (--no-verify)\n");
            }

            let firmware = std::fs::read(&file)?;
            println!("Firmware: {} ({} bytes)", file.display(), firmware.len());
            println!("Flash address: 0x{:08X}", address);
            println!("Resetting NET chip to bootloader mode...\n");

            let pb = ProgressBar::new(firmware.len() as u64);
            let bytes_style = ProgressStyle::default_bar()
                .template("{prefix:>12} [{bar:40.cyan/blue}] {bytes}/{total_bytes} ({percent}%)")
                .unwrap()
                .progress_chars("#>-");
            let erase_style = ProgressStyle::default_bar()
                .template("{prefix:>12} [{bar:40.cyan/blue}] {pos}/{len} ({percent}%)")
                .unwrap()
                .progress_chars("#>-");
            pb.set_style(erase_style.clone());

            let mut current_phase = None;
            let verify = !no_verify;
            let result = app
                .flash_net(
                    &firmware,
                    address,
                    compress,
                    verify,
                    |phase, progress, total| {
                        if current_phase != Some(phase) {
                            current_phase = Some(phase);
                            match phase {
                                FlashPhase::Compressing => {
                                    pb.set_style(bytes_style.clone());
                                    pb.set_prefix("Compressing");
                                }
                                FlashPhase::Erasing => {
                                    pb.set_style(erase_style.clone());
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
                )
                .await;

            pb.finish_and_clear();

            match result {
                Ok(()) => Ok(Some(
                    "Flash complete!\nNET chip reset back to user mode.".to_string(),
                )),
                Err(e) => Err(format!("Flash failed: {:?}", e).into()),
            }
        }
        NetAction::WsPing { data } => {
            println!("Sending WebSocket ping with data: {}", data);
            app.ws_ping(data.as_bytes()).await;
            Ok(Some("Received echo response!".to_string()))
        }
        NetAction::WsEchoTest => {
            println!("Running WebSocket echo test...");
            println!("  Sending 50 packets (640 bytes each) at 20ms intervals (50 fps)\n");

            let results = app.ws_echo_test().await;

            let mut output = String::new();
            output.push_str("Results:\n");
            output.push_str(&format!("  Packets sent:           {}\n", results.sent));
            output.push_str(&format!("  Packets received (raw): {}\n", results.received));
            output.push_str(&format!(
                "  Packets output (buf):   {}\n",
                results.buffered_output
            ));
            output.push_str(&format!(
                "  Buffer underruns:       {}\n",
                results.underruns
            ));

            if results.received > 0 && results.sent > 0 {
                let loss_pct =
                    ((results.sent - results.received) as f64 / results.sent as f64) * 100.0;
                output.push_str(&format!("  Packet loss:            {:.1}%\n", loss_pct));
            }

            output.push_str(&format_jitter_stats(
                "Raw jitter (before buffer)",
                results.raw_jitter_us.as_slice(),
            ));
            output.push_str(&format_jitter_stats(
                "Buffered jitter (after buffer)",
                results.buffered_jitter_us.as_slice(),
            ));

            if !results.raw_jitter_us.is_empty() {
                output.push_str(&format!(
                    "\nRaw timings (µs): {:?}\n",
                    results.raw_jitter_us.as_slice()
                ));
            }
            if !results.buffered_jitter_us.is_empty() {
                output.push_str(&format!(
                    "Buffered timings (µs): {:?}",
                    results.buffered_jitter_us.as_slice()
                ));
            }

            Ok(Some(output))
        }
        NetAction::WsSpeedTest => {
            println!("Running WebSocket speed test...");
            println!("  Sending 50 packets (640 bytes each) as fast as possible\n");

            let results = app.ws_speed_test().await;

            let mut output = String::new();
            output.push_str("Results:\n");
            output.push_str(&format!("  Packets sent:     {}\n", results.sent));
            output.push_str(&format!("  Packets received: {}\n", results.received));
            output.push_str(&format!(
                "  Send time:        {} ms\n",
                results.send_time_ms
            ));
            output.push_str(&format!(
                "  Receive time:     {} ms\n",
                results.recv_time_ms
            ));

            if results.sent > 0 {
                let send_rate =
                    (results.sent as f64 * 640.0) / (results.send_time_ms as f64 / 1000.0) / 1024.0;
                output.push_str(&format!("  Send rate:        {:.1} KB/s\n", send_rate));
                let fps = results.sent as f64 / (results.send_time_ms as f64 / 1000.0);
                output.push_str(&format!("  Send FPS:         {:.1}\n", fps));
            }

            if results.received > 0 && results.sent > 0 {
                let loss_pct =
                    ((results.sent - results.received) as f64 / results.sent as f64) * 100.0;
                output.push_str(&format!("  Packet loss:      {:.1}%", loss_pct));
            }

            Ok(Some(output))
        }
        NetAction::Loopback { action } => match action.unwrap_or_default() {
            GetSetBool::Get => {
                let enabled = app.net_get_loopback().await;
                Ok(Some(format!("{}", enabled)))
            }
            GetSetBool::Set { value } => {
                app.net_set_loopback(value).await;
                Ok(Some(format!("NET loopback set to {}", value)))
            }
        },
    }
}

fn format_jitter_stats(label: &str, timings: &[u32]) -> String {
    if timings.is_empty() {
        return format!("\n{}: No data\n", label);
    }
    let min = timings.iter().min().copied().unwrap_or(0);
    let max = timings.iter().max().copied().unwrap_or(0);
    let sum: u64 = timings.iter().map(|&x| x as u64).sum();
    let avg = sum / timings.len() as u64;

    let mut s = format!("\n{}:\n", label);
    s.push_str(&format!(
        "  Min: {:>6} µs ({:>5.1} ms)\n",
        min,
        min as f64 / 1000.0
    ));
    s.push_str(&format!(
        "  Max: {:>6} µs ({:>5.1} ms)\n",
        max,
        max as f64 / 1000.0
    ));
    s.push_str(&format!(
        "  Avg: {:>6} µs ({:>5.1} ms)\n",
        avg,
        avg as f64 / 1000.0
    ));

    let target_us = 20000i64;
    let jitter: i64 = timings
        .iter()
        .map(|&x| (x as i64 - target_us).abs())
        .sum::<i64>()
        / timings.len() as i64;
    s.push_str(&format!(
        "  Avg deviation from 20ms: {:>6} µs ({:>5.1} ms)\n",
        jitter,
        jitter as f64 / 1000.0
    ));
    s
}

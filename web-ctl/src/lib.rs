//! WebAssembly bindings for the Link CTL (Controller) interface.
//!
//! This crate provides a web-based interface to control Link devices
//! via the WebSerial API. It uses async I/O to communicate with the device.

mod serial;

use link::ctl::espflash::target::ProgressCallbacks;
use link::ctl::flash::{AsyncDelay, FlashPhase};
use link::ctl::stm;
use link::ctl::{CtlCore, CtlError, CtlPort, SetTimeout};
use wasm_bindgen_futures::JsFuture;
use link::{LoopbackMode, NetLoopback};
use serde::{Deserialize, Serialize};
use serial::{WebSerial, WebSerialAdapter};
use wasm_bindgen::prelude::*;

/// Initialize panic hook for better error messages.
#[wasm_bindgen(start)]
pub fn init() {
    console_error_panic_hook::set_once();
}

/// Log a message to the browser console.
fn log(msg: &str) {
    web_sys::console::log_1(&JsValue::from_str(msg));
}

/// Async sleep using JavaScript setTimeout.
async fn js_sleep(ms: u32) {
    let promise = js_sys::Promise::new(&mut |resolve, _reject| {
        let window = web_sys::window().expect("no global window");
        let _ = window.set_timeout_with_callback_and_timeout_and_arguments_0(&resolve, ms as i32);
    });
    let _ = JsFuture::from(promise).await;
}

/// JavaScript-based async delay implementation.
///
/// This is WASM-compatible and uses `setTimeout` instead of `std::thread::sleep`.
struct JsDelay;

impl AsyncDelay for JsDelay {
    async fn delay_ms(&self, ms: u32) {
        js_sleep(ms).await;
    }
}

/// WiFi network configuration.
#[derive(Serialize, Deserialize, Clone)]
pub struct WifiNetwork {
    pub ssid: String,
    pub password: String,
}

/// Convert CtlError to JsValue
fn ctl_error_to_js(e: CtlError) -> JsValue {
    JsValue::from_str(&format!("{}", e))
}

// ============================================================================
// LinkController
// ============================================================================

/// The main controller interface exposed to JavaScript.
#[wasm_bindgen]
pub struct LinkController {
    core: Option<CtlCore<WebSerialAdapter>>,
}

#[wasm_bindgen]
impl LinkController {
    /// Create a new LinkController instance.
    #[wasm_bindgen(constructor)]
    pub fn new() -> Self {
        Self {
            core: None,
        }
    }

    /// Connect to a Link device via WebSerial.
    /// This will prompt the user to select a serial port.
    #[wasm_bindgen]
    pub async fn connect(&mut self, baud_rate: u32) -> Result<(), JsValue> {
        let serial = WebSerial::new();
        serial
            .connect(baud_rate)
            .await
            .map_err(|e| JsValue::from_str(&format!("{}", e)))?;

        // Wrap in adapter and create CtlCore
        let adapter = WebSerialAdapter::new(serial);
        self.core = Some(CtlCore::new(adapter));

        log("Connected to Link device");
        Ok(())
    }

    /// Check if connected to a device.
    #[wasm_bindgen]
    pub fn is_connected(&self) -> bool {
        self.core.is_some()
    }

    /// Disconnect from the device.
    #[wasm_bindgen]
    pub async fn disconnect(&mut self) -> Result<(), JsValue> {
        if let Some(core) = self.core.take() {
            // Get the serial port from the core and disconnect
            let adapter = core.into_inner();
            let serial = adapter.into_inner();
            serial
                .disconnect()
                .await
                .map_err(|e| JsValue::from_str(&format!("{}", e)))?;
        }
        log("Disconnected from Link device");
        Ok(())
    }

    /// Get CtlCore, returning error if not connected.
    fn core_mut(&mut self) -> Result<&mut CtlCore<WebSerialAdapter>, JsValue> {
        self.core.as_mut().ok_or_else(|| JsValue::from_str("Not connected"))
    }

    /// Test connection with a Hello handshake.
    /// Returns true if the device responds correctly.
    #[wasm_bindgen]
    pub async fn hello(&mut self) -> Result<bool, JsValue> {
        // Generate a random challenge
        let challenge: [u8; 4] = [
            (js_sys::Math::random() * 256.0) as u8,
            (js_sys::Math::random() * 256.0) as u8,
            (js_sys::Math::random() * 256.0) as u8,
            (js_sys::Math::random() * 256.0) as u8,
        ];

        let core = self.core_mut()?;
        let result = core.hello(&challenge).await;
        Ok(result)
    }

    /// Get the firmware version stored in UI chip EEPROM.
    #[wasm_bindgen]
    pub async fn get_version(&mut self) -> Result<u32, JsValue> {
        let core = self.core_mut()?;
        core.get_version().await.map_err(ctl_error_to_js)
    }

    /// Set the firmware version in UI chip EEPROM.
    #[wasm_bindgen]
    pub async fn set_version(&mut self, version: u32) -> Result<(), JsValue> {
        let core = self.core_mut()?;
        core.set_version(version).await.map_err(ctl_error_to_js)
    }

    /// Get the SFrame key from UI chip EEPROM.
    /// Returns the key as a hex string.
    #[wasm_bindgen]
    pub async fn get_sframe_key(&mut self) -> Result<String, JsValue> {
        let core = self.core_mut()?;
        let key = core.get_sframe_key().await.map_err(ctl_error_to_js)?;
        Ok(hex::encode(key))
    }

    /// Set the SFrame key in UI chip EEPROM.
    /// Takes the key as a hex string (32 hex chars = 16 bytes).
    #[wasm_bindgen]
    pub async fn set_sframe_key(&mut self, key_hex: &str) -> Result<(), JsValue> {
        let key_bytes = hex::decode(key_hex)
            .map_err(|e| JsValue::from_str(&format!("Invalid hex string: {}", e)))?;

        if key_bytes.len() != 16 {
            return Err(JsValue::from_str(
                "SFrame key must be 16 bytes (32 hex chars)",
            ));
        }

        let mut key = [0u8; 16];
        key.copy_from_slice(&key_bytes);

        let core = self.core_mut()?;
        core.set_sframe_key(&key).await.map_err(ctl_error_to_js)
    }

    /// Get the relay URL from NET chip storage.
    #[wasm_bindgen]
    pub async fn get_relay_url(&mut self) -> Result<String, JsValue> {
        let core = self.core_mut()?;
        let url = core.get_relay_url().await.map_err(ctl_error_to_js)?;
        Ok(url.to_string())
    }

    /// Set the relay URL in NET chip storage.
    #[wasm_bindgen]
    pub async fn set_relay_url(&mut self, url: &str) -> Result<(), JsValue> {
        let core = self.core_mut()?;
        core.set_relay_url(url).await.map_err(ctl_error_to_js)
    }

    /// Get all WiFi networks from NET chip storage.
    /// Returns a JSON array of {ssid, password} objects.
    #[wasm_bindgen]
    pub async fn get_wifi_networks(&mut self) -> Result<JsValue, JsValue> {
        let core = self.core_mut()?;
        let ssids = core.get_wifi_ssids().await.map_err(ctl_error_to_js)?;

        let networks: Vec<WifiNetwork> = ssids
            .iter()
            .map(|s| WifiNetwork {
                ssid: s.ssid.to_string(),
                password: s.password.to_string(),
            })
            .collect();

        serde_wasm_bindgen::to_value(&networks)
            .map_err(|e| JsValue::from_str(&format!("Serialization error: {}", e)))
    }

    /// Add a WiFi network to NET chip storage.
    #[wasm_bindgen]
    pub async fn add_wifi_network(&mut self, ssid: &str, password: &str) -> Result<(), JsValue> {
        let core = self.core_mut()?;
        core.add_wifi_ssid(ssid, password).await.map_err(ctl_error_to_js)
    }

    /// Clear all WiFi networks from NET chip storage.
    #[wasm_bindgen]
    pub async fn clear_wifi_networks(&mut self) -> Result<(), JsValue> {
        let core = self.core_mut()?;
        core.clear_wifi_ssids().await.map_err(ctl_error_to_js)
    }

    /// Reset the UI chip into bootloader mode.
    #[wasm_bindgen]
    pub async fn reset_ui_to_bootloader(&mut self) -> Result<(), JsValue> {
        let core = self.core_mut()?;
        core.reset_ui_to_bootloader().await.map_err(ctl_error_to_js)
    }

    /// Reset the UI chip into user mode.
    #[wasm_bindgen]
    pub async fn reset_ui_to_user(&mut self) -> Result<(), JsValue> {
        let core = self.core_mut()?;
        core.reset_ui_to_user().await.map_err(ctl_error_to_js)
    }

    /// Reset the NET chip into bootloader mode.
    #[wasm_bindgen]
    pub async fn reset_net_to_bootloader(&mut self) -> Result<(), JsValue> {
        let core = self.core_mut()?;
        core.reset_net_to_bootloader().await.map_err(ctl_error_to_js)
    }

    /// Reset the NET chip into user mode.
    #[wasm_bindgen]
    pub async fn reset_net_to_user(&mut self) -> Result<(), JsValue> {
        let core = self.core_mut()?;
        core.reset_net_to_user().await.map_err(ctl_error_to_js)
    }

    /// Get UI chip loopback mode as string.
    /// Returns: "off", "raw", "alaw", or "sframe"
    #[wasm_bindgen]
    pub async fn get_ui_loopback_mode(&mut self) -> Result<String, JsValue> {
        let core = self.core_mut()?;
        let mode = core.ui_get_loopback().await.map_err(ctl_error_to_js)?;
        let mode_str = match mode {
            LoopbackMode::Off => "off",
            LoopbackMode::Raw => "raw",
            LoopbackMode::Alaw => "alaw",
            LoopbackMode::Sframe => "sframe",
        };
        Ok(mode_str.to_string())
    }

    /// Set UI chip loopback mode.
    /// Modes: 0=off, 1=raw, 2=alaw, 3=sframe
    #[wasm_bindgen]
    pub async fn set_ui_loopback_mode(&mut self, mode: u8) -> Result<(), JsValue> {
        if mode > 3 {
            return Err(JsValue::from_str("Invalid loopback mode (0-3)"));
        }

        let loopback_mode = match mode {
            0 => LoopbackMode::Off,
            1 => LoopbackMode::Raw,
            2 => LoopbackMode::Alaw,
            3 => LoopbackMode::Sframe,
            _ => unreachable!(),
        };

        let core = self.core_mut()?;
        core.ui_set_loopback(loopback_mode).await.map_err(ctl_error_to_js)
    }

    /// Get NET chip loopback mode as string.
    /// Returns: "off", "raw", or "moq"
    #[wasm_bindgen]
    pub async fn get_net_loopback_mode(&mut self) -> Result<String, JsValue> {
        let core = self.core_mut()?;
        let mode = core.net_get_loopback().await.map_err(ctl_error_to_js)?;
        let mode_str = match mode {
            NetLoopback::Off => "off",
            NetLoopback::Raw => "raw",
            NetLoopback::Moq => "moq",
        };
        Ok(mode_str.to_string())
    }

    /// Set NET chip loopback mode.
    /// Modes: 0=off, 1=raw, 2=moq
    #[wasm_bindgen]
    pub async fn set_net_loopback_mode(&mut self, mode: u8) -> Result<(), JsValue> {
        if mode > 2 {
            return Err(JsValue::from_str("Invalid loopback mode (0-2)"));
        }

        let loopback_mode = match mode {
            0 => NetLoopback::Off,
            1 => NetLoopback::Raw,
            2 => NetLoopback::Moq,
            _ => unreachable!(),
        };

        let core = self.core_mut()?;
        core.net_set_loopback(loopback_mode).await.map_err(ctl_error_to_js)
    }

    /// Ping the MGMT chip.
    #[wasm_bindgen]
    pub async fn ping_mgmt(&mut self, data: Vec<u8>) -> Result<(), JsValue> {
        let core = self.core_mut()?;
        core.mgmt_ping(&data).await.map_err(ctl_error_to_js)
    }

    /// Ping the UI chip.
    #[wasm_bindgen]
    pub async fn ping_ui(&mut self, data: Vec<u8>) -> Result<(), JsValue> {
        let core = self.core_mut()?;
        core.ui_ping(&data).await.map_err(ctl_error_to_js)
    }

    /// Ping the NET chip.
    #[wasm_bindgen]
    pub async fn ping_net(&mut self, data: Vec<u8>) -> Result<(), JsValue> {
        let core = self.core_mut()?;
        core.net_ping(&data).await.map_err(ctl_error_to_js)
    }

    // ==================== STACK DIAGNOSTICS ====================

    /// Get MGMT chip stack usage information.
    /// Returns JSON with {stackBase, stackTop, stackSize, stackUsed, stackFree, usagePercent}.
    #[wasm_bindgen]
    pub async fn get_mgmt_stack_info(&mut self) -> Result<JsValue, JsValue> {
        let core = self.core_mut()?;
        let info = core.mgmt_get_stack_info().await.map_err(ctl_error_to_js)?;

        let stack_free = info.stack_size.saturating_sub(info.stack_used);
        let usage_percent = if info.stack_size > 0 {
            (info.stack_used as f64 / info.stack_size as f64) * 100.0
        } else {
            0.0
        };

        let obj = js_sys::Object::new();
        js_sys::Reflect::set(&obj, &"stackBase".into(), &info.stack_base.into())?;
        js_sys::Reflect::set(&obj, &"stackTop".into(), &info.stack_top.into())?;
        js_sys::Reflect::set(&obj, &"stackSize".into(), &info.stack_size.into())?;
        js_sys::Reflect::set(&obj, &"stackUsed".into(), &info.stack_used.into())?;
        js_sys::Reflect::set(&obj, &"stackFree".into(), &stack_free.into())?;
        js_sys::Reflect::set(&obj, &"usagePercent".into(), &usage_percent.into())?;
        Ok(obj.into())
    }

    /// Repaint MGMT chip stack (for fresh high-water mark measurement).
    #[wasm_bindgen]
    pub async fn repaint_mgmt_stack(&mut self) -> Result<(), JsValue> {
        let core = self.core_mut()?;
        core.mgmt_repaint_stack().await.map_err(ctl_error_to_js)
    }

    /// Get UI chip stack usage information.
    #[wasm_bindgen]
    pub async fn get_ui_stack_info(&mut self) -> Result<JsValue, JsValue> {
        let core = self.core_mut()?;
        let info = core.ui_get_stack_info().await.map_err(ctl_error_to_js)?;

        let stack_free = info.stack_size.saturating_sub(info.stack_used);
        let usage_percent = if info.stack_size > 0 {
            (info.stack_used as f64 / info.stack_size as f64) * 100.0
        } else {
            0.0
        };

        let obj = js_sys::Object::new();
        js_sys::Reflect::set(&obj, &"stackBase".into(), &info.stack_base.into())?;
        js_sys::Reflect::set(&obj, &"stackTop".into(), &info.stack_top.into())?;
        js_sys::Reflect::set(&obj, &"stackSize".into(), &info.stack_size.into())?;
        js_sys::Reflect::set(&obj, &"stackUsed".into(), &info.stack_used.into())?;
        js_sys::Reflect::set(&obj, &"stackFree".into(), &stack_free.into())?;
        js_sys::Reflect::set(&obj, &"usagePercent".into(), &usage_percent.into())?;
        Ok(obj.into())
    }

    /// Repaint UI chip stack (for fresh high-water mark measurement).
    #[wasm_bindgen]
    pub async fn repaint_ui_stack(&mut self) -> Result<(), JsValue> {
        let core = self.core_mut()?;
        core.ui_repaint_stack().await.map_err(ctl_error_to_js)
    }

    // ==================== CHAT ====================

    /// Send a chat message through the NET chip.
    #[wasm_bindgen]
    pub async fn send_chat_message(&mut self, message: &str) -> Result<(), JsValue> {
        let core = self.core_mut()?;
        core.send_chat_message(message).await.map_err(ctl_error_to_js)
    }

    // ==================== CIRCULAR PING ====================

    /// Send a circular ping starting from UI (UI → NET → MGMT → CTL).
    #[wasm_bindgen]
    pub async fn circular_ping_via_ui(&mut self, data: Vec<u8>) -> Result<(), JsValue> {
        let core = self.core_mut()?;
        core.ui_first_circular_ping(&data).await.map_err(ctl_error_to_js)
    }

    /// Send a circular ping starting from NET (NET → UI → MGMT → CTL).
    #[wasm_bindgen]
    pub async fn circular_ping_via_net(&mut self, data: Vec<u8>) -> Result<(), JsValue> {
        let core = self.core_mut()?;
        core.net_first_circular_ping(&data).await.map_err(ctl_error_to_js)
    }

    // ==================== JITTER STATS ====================

    /// Get jitter buffer statistics for a channel.
    /// Returns JSON with {received, output, underruns, overruns, level, state}.
    #[wasm_bindgen]
    pub async fn get_jitter_stats(&mut self, channel_id: u8) -> Result<JsValue, JsValue> {
        let core = self.core_mut()?;
        let stats = core.get_jitter_stats(channel_id).await.map_err(ctl_error_to_js)?;

        let obj = js_sys::Object::new();
        js_sys::Reflect::set(&obj, &"received".into(), &stats.received.into())?;
        js_sys::Reflect::set(&obj, &"output".into(), &stats.output.into())?;
        js_sys::Reflect::set(&obj, &"underruns".into(), &stats.underruns.into())?;
        js_sys::Reflect::set(&obj, &"overruns".into(), &stats.overruns.into())?;
        js_sys::Reflect::set(&obj, &"level".into(), &stats.level.into())?;
        js_sys::Reflect::set(&obj, &"state".into(), &(if stats.state == 0 { "buffering" } else { "playing" }).into())?;
        Ok(obj.into())
    }

    // ==================== FLASHING ====================

    /// Get MGMT chip bootloader information.
    ///
    /// This assumes the MGMT chip is already in bootloader mode (press BOOT0 button
    /// while powering on the device).
    #[wasm_bindgen]
    pub async fn get_mgmt_bootloader_info(&mut self) -> Result<JsValue, JsValue> {
        let core = self.core_mut()?;
        let info = core.get_mgmt_bootloader_info().await
            .map_err(|e| JsValue::from_str(&format!("Bootloader error: {:?}", e)))?;

        let obj = js_sys::Object::new();
        js_sys::Reflect::set(&obj, &"bootloaderVersion".into(), &info.bootloader_version.into())?;
        js_sys::Reflect::set(&obj, &"chipId".into(), &info.chip_id.into())?;
        js_sys::Reflect::set(&obj, &"chipName".into(), &stm::chip_name(info.chip_id).into())?;
        js_sys::Reflect::set(&obj, &"commandCount".into(), &(info.command_count as u32).into())?;

        let commands = js_sys::Array::new();
        for i in 0..info.command_count {
            let cmd_obj = js_sys::Object::new();
            let code = info.commands[i];
            js_sys::Reflect::set(&cmd_obj, &"code".into(), &code.into())?;
            js_sys::Reflect::set(&cmd_obj, &"name".into(), &stm::command_name(code).into())?;
            commands.push(&cmd_obj);
        }
        js_sys::Reflect::set(&obj, &"commands".into(), &commands)?;

        if let Some(flash_sample) = info.flash_sample {
            js_sys::Reflect::set(&obj, &"flashSample".into(), &hex::encode(flash_sample).into())?;
            js_sys::Reflect::set(&obj, &"readProtected".into(), &false.into())?;
        } else {
            js_sys::Reflect::set(&obj, &"readProtected".into(), &true.into())?;
        }

        Ok(obj.into())
    }

    /// Try to enter MGMT bootloader mode automatically (EV16).
    ///
    /// Returns a string indicating the result:
    /// - "auto_reset": Successfully entered bootloader via DTR/RTS (EV16)
    /// - "already_active": Bootloader was already active
    /// - "not_detected": Could not detect bootloader - manual intervention required
    #[wasm_bindgen]
    pub async fn try_enter_mgmt_bootloader(&mut self) -> Result<String, JsValue> {
        let core = self.core_mut()?;

        // Set short timeout for probing
        let _ = core.port_mut().set_timeout(std::time::Duration::from_millis(200));

        // Clear any stale data
        core.drain();

        // Try DTR/RTS reset sequence (EV16)
        // RTS=high sets BOOT0 high (bootloader mode)
        // DTR pulse triggers reset
        let _ = core.port_mut().write_rts(true).await;
        let _ = core.port_mut().write_dtr(true).await;
        let _ = core.port_mut().write_dtr(false).await;

        // Wait for bootloader to initialize using JS setTimeout
        js_sleep(100).await;

        // Clear buffer again after reset
        core.drain();

        // Probe for bootloader - send 0x7F and wait for ACK
        let init_byte = [0x7F];
        let probe_result = async {
            core.port_mut().write_all(&init_byte).await?;
            core.port_mut().flush().await?;

            let mut response = [0u8; 1];
            core.port_mut().read_exact(&mut response).await?;
            Ok::<bool, std::io::Error>(response[0] == 0x79)
        }.await;

        // Restore normal timeout
        let _ = core.port_mut().set_timeout(std::time::Duration::from_secs(3));

        let result_str = match probe_result {
            Ok(true) => "auto_reset",
            _ => "not_detected",
        };

        Ok(result_str.to_string())
    }

    /// Flash firmware to the MGMT chip (STM32F072CB).
    ///
    /// This method will first try to enter bootloader mode automatically (EV16).
    /// If auto-reset fails, it returns an error with "manual_reset_required" to
    /// indicate the UI should prompt the user.
    ///
    /// Pass firmware as Uint8Array.
    /// The progress callback receives (phase: string, current: number, total: number).
    #[wasm_bindgen]
    pub async fn flash_mgmt(&mut self, firmware: js_sys::Uint8Array, progress_callback: js_sys::Function) -> Result<(), JsValue> {
        // Try automatic bootloader entry first
        let entry_result = self.try_enter_mgmt_bootloader().await?;

        if entry_result == "not_detected" {
            // Return special error to signal UI should prompt user
            return Err(JsValue::from_str("manual_reset_required"));
        }

        log(&format!("Bootloader entry: {}", entry_result));

        let firmware_data = firmware.to_vec();
        let core = self.core_mut()?;

        core.flash_mgmt(&firmware_data, |phase, current, total| {
            let phase_str = match phase {
                FlashPhase::Compressing => "compressing",
                FlashPhase::Erasing => "erasing",
                FlashPhase::Writing => "writing",
                FlashPhase::Verifying => "verifying",
            };
            let _ = progress_callback.call3(
                &JsValue::NULL,
                &JsValue::from_str(phase_str),
                &JsValue::from(current as u32),
                &JsValue::from(total as u32),
            );
        }).await.map_err(|e| JsValue::from_str(&format!("Flash error: {:?}", e)))
    }

    /// Probe if MGMT bootloader is currently active (without reset).
    ///
    /// Returns true if the bootloader responds to the init byte (0x7F).
    /// Use this to verify manual bootloader entry before flashing.
    #[wasm_bindgen]
    pub async fn probe_mgmt_bootloader(&mut self) -> Result<bool, JsValue> {
        let core = self.core_mut()?;

        // Set short timeout for probing
        let _ = core.port_mut().set_timeout(std::time::Duration::from_millis(500));

        // Clear any stale data
        core.drain();

        // Send 0x7F init byte and wait for ACK
        let init_byte = [0x7F];
        let probe_result = async {
            core.port_mut().write_all(&init_byte).await?;
            core.port_mut().flush().await?;

            let mut response = [0u8; 1];
            core.port_mut().read_exact(&mut response).await?;
            Ok::<bool, std::io::Error>(response[0] == 0x79)
        }.await;

        // Restore normal timeout
        let _ = core.port_mut().set_timeout(std::time::Duration::from_secs(3));

        Ok(probe_result.unwrap_or(false))
    }

    /// Flash firmware to the MGMT chip after manual bootloader entry.
    ///
    /// Use this after the user has manually reset the device to bootloader mode.
    /// Pass firmware as Uint8Array.
    /// The progress callback receives (phase: string, current: number, total: number).
    #[wasm_bindgen]
    pub async fn flash_mgmt_manual(&mut self, firmware: js_sys::Uint8Array, progress_callback: js_sys::Function) -> Result<(), JsValue> {
        let firmware_data = firmware.to_vec();
        let core = self.core_mut()?;

        // Aggressive buffer clearing - multiple rounds with delays to catch any incoming data
        let mut all_discarded = Vec::new();
        for round in 0..3 {
            // Clear internal buffers
            core.drain();

            // Read and discard any data pending in the stream
            // Use longer timeout to catch data that might still be arriving
            let _ = core.port_mut().set_timeout(std::time::Duration::from_millis(100));
            let mut discard_buf = [0u8; 256];
            let mut round_discarded = 0;
            loop {
                match core.port_mut().read(&mut discard_buf).await {
                    Ok(0) => break,
                    Ok(n) => {
                        // Capture the data for logging
                        all_discarded.extend_from_slice(&discard_buf[..n]);
                        round_discarded += n;
                        continue;
                    }
                    Err(_) => break, // Timeout or error, buffer is clear
                }
            }

            if round_discarded > 0 {
                log(&format!("Round {}: discarded {} bytes", round + 1, round_discarded));
            }

            // Short delay between rounds to let more data arrive if it's still coming
            js_sleep(50).await;
        }

        // Log the discarded data content
        if !all_discarded.is_empty() {
            // Show as text if it looks like ASCII, otherwise show hex
            let text_preview: String = all_discarded.iter()
                .take(200)
                .map(|&b| if b >= 0x20 && b < 0x7f { b as char } else { '.' })
                .collect();
            log(&format!("Discarded data preview: {}", text_preview));

            // Also show first 32 bytes as hex
            let hex_preview: String = all_discarded.iter()
                .take(32)
                .map(|b| format!("{:02x}", b))
                .collect::<Vec<_>>()
                .join(" ");
            log(&format!("Discarded data hex: {}", hex_preview));
        }

        // Restore normal timeout
        let _ = core.port_mut().set_timeout(std::time::Duration::from_secs(3));

        // Diagnostic: try to probe the bootloader directly
        log("Probing bootloader...");

        // DON'T drain here - we already drained above, and the bootloader
        // might have responded to something already

        // Send the STM32 bootloader init byte (0x7F) directly via WebSerial
        // Bypass CtlCore to ensure we're doing raw I/O
        log("Sending 0x7F init byte...");

        // Small delay to let bootloader settle after reset
        js_sleep(50).await;

        core.port_mut().write_all(&[0x7F]).await
            .map_err(|e| JsValue::from_str(&format!("Failed to send init byte: {:?}", e)))?;
        core.port_mut().flush().await
            .map_err(|e| JsValue::from_str(&format!("Failed to flush: {:?}", e)))?;

        log("Init byte sent, waiting for response...");

        // Wait for response - try multiple short reads to see what comes back
        let _ = core.port_mut().set_timeout(std::time::Duration::from_millis(200));

        for attempt in 1..=5 {
            let mut response = [0u8; 64];
            match core.port_mut().read(&mut response).await {
                Ok(0) => {
                    log(&format!("Attempt {}: got 0 bytes (EOF?)", attempt));
                }
                Ok(n) => {
                    let hex: String = response[..n].iter()
                        .map(|b| format!("{:02x}", b))
                        .collect::<Vec<_>>()
                        .join(" ");
                    log(&format!("Attempt {}: got {} bytes: {}", attempt, n, hex));

                    // Check for ACK anywhere in the response
                    if response[..n].contains(&0x79) {
                        log("Found ACK (0x79) - bootloader is active!");
                        break;
                    } else if response[..n].contains(&0x1f) {
                        log("Found NACK (0x1F) - bootloader rejected");
                        break;
                    }
                }
                Err(_) => {
                    log(&format!("Attempt {}: timeout", attempt));
                }
            }
            js_sleep(100).await;
        }

        // Now try the actual flash - if bootloader is active, it should work
        log("Proceeding with flash attempt...");

        // Restore normal timeout for flashing
        let _ = core.port_mut().set_timeout(std::time::Duration::from_secs(3));

        core.flash_mgmt(&firmware_data, |phase, current, total| {
            let phase_str = match phase {
                FlashPhase::Compressing => "compressing",
                FlashPhase::Erasing => "erasing",
                FlashPhase::Writing => "writing",
                FlashPhase::Verifying => "verifying",
            };
            let _ = progress_callback.call3(
                &JsValue::NULL,
                &JsValue::from_str(phase_str),
                &JsValue::from(current as u32),
                &JsValue::from(total as u32),
            );
        }).await.map_err(|e| JsValue::from_str(&format!("Flash error: {:?}", e)))
    }

    /// Get UI chip bootloader information.
    ///
    /// This resets the UI chip into bootloader mode, queries info, then resets back.
    #[wasm_bindgen]
    pub async fn get_ui_bootloader_info(&mut self) -> Result<JsValue, JsValue> {
        let core = self.core_mut()?;

        // Reset UI chip into bootloader mode
        let _ = core.reset_ui_to_bootloader().await;

        // Wait for bootloader to be ready (use js_sleep instead of std::thread::sleep)
        js_sleep(1000).await;

        // Query bootloader info
        let result = core.query_ui_bootloader().await;

        // Always reset UI chip back to user mode
        let _ = core.reset_ui_to_user().await;

        let info = result.map_err(|e| JsValue::from_str(&format!("Bootloader error: {:?}", e)))?;

        let obj = js_sys::Object::new();
        js_sys::Reflect::set(&obj, &"bootloaderVersion".into(), &info.bootloader_version.into())?;
        js_sys::Reflect::set(&obj, &"chipId".into(), &info.chip_id.into())?;
        js_sys::Reflect::set(&obj, &"chipName".into(), &stm::chip_name(info.chip_id).into())?;
        js_sys::Reflect::set(&obj, &"commandCount".into(), &(info.command_count as u32).into())?;

        let commands = js_sys::Array::new();
        for i in 0..info.command_count {
            let cmd_obj = js_sys::Object::new();
            let code = info.commands[i];
            js_sys::Reflect::set(&cmd_obj, &"code".into(), &code.into())?;
            js_sys::Reflect::set(&cmd_obj, &"name".into(), &stm::command_name(code).into())?;
            commands.push(&cmd_obj);
        }
        js_sys::Reflect::set(&obj, &"commands".into(), &commands)?;

        if let Some(flash_sample) = info.flash_sample {
            js_sys::Reflect::set(&obj, &"flashSample".into(), &hex::encode(flash_sample).into())?;
            js_sys::Reflect::set(&obj, &"readProtected".into(), &false.into())?;
        } else {
            js_sys::Reflect::set(&obj, &"readProtected".into(), &true.into())?;
        }

        Ok(obj.into())
    }

    /// Flash firmware to the UI chip (STM32F405RG).
    ///
    /// This will reset the UI chip to bootloader mode, flash, and reset back.
    /// Pass firmware as Uint8Array.
    /// The progress callback receives (phase: string, current: number, total: number).
    #[wasm_bindgen]
    pub async fn flash_ui(&mut self, firmware: js_sys::Uint8Array, verify: bool, progress_callback: js_sys::Function) -> Result<(), JsValue> {
        let firmware_data = firmware.to_vec();
        let core = self.core_mut()?;

        // Reset UI chip into bootloader mode
        let _ = core.reset_ui_to_bootloader().await;

        // Wait for bootloader to be ready (use js_sleep instead of std::thread::sleep)
        js_sleep(100).await;

        // Flash the firmware
        let result = core.flash_ui_in_bootloader_mode(
            &firmware_data,
            verify,
            &mut |phase, current, total| {
                let phase_str = match phase {
                    FlashPhase::Compressing => "compressing",
                    FlashPhase::Erasing => "erasing",
                    FlashPhase::Writing => "writing",
                    FlashPhase::Verifying => "verifying",
                };
                let _ = progress_callback.call3(
                    &JsValue::NULL,
                    &JsValue::from_str(phase_str),
                    &JsValue::from(current as u32),
                    &JsValue::from(total as u32),
                );
            },
        ).await;

        // Always reset UI chip back to user mode
        let _ = core.reset_ui_to_user().await;

        result.map_err(|e| JsValue::from_str(&format!("Flash error: {:?}", e)))
    }

    /// Get NET chip (ESP32) bootloader information.
    ///
    /// Returns device info including chip type, flash size, MAC address, and security info.
    #[wasm_bindgen]
    pub async fn get_net_bootloader_info(&mut self) -> Result<JsValue, JsValue> {
        let core = self.core_mut()?;
        let info = core.get_net_bootloader_info(JsDelay).await
            .map_err(|e| JsValue::from_str(&format!("Bootloader error: {:?}", e)))?;

        let obj = js_sys::Object::new();

        // Device info
        let device = &info.device_info;
        js_sys::Reflect::set(&obj, &"chip".into(), &format!("{:?}", device.chip).into())?;
        js_sys::Reflect::set(&obj, &"flashSize".into(), &format!("{:?}", device.flash_size).into())?;
        js_sys::Reflect::set(&obj, &"crystalFrequency".into(), &format!("{:?}", device.crystal_frequency).into())?;

        let features = js_sys::Array::new();
        for feature in &device.features {
            features.push(&JsValue::from_str(feature));
        }
        js_sys::Reflect::set(&obj, &"features".into(), &features)?;

        if let Some(mac) = &device.mac_address {
            js_sys::Reflect::set(&obj, &"macAddress".into(), &mac.clone().into())?;
        }

        // Security info
        let security = &info.security_info;
        let secure_boot = (security.flags & 1) != 0;
        let flash_encryption = security.flash_crypt_cnt.count_ones() % 2 != 0;
        js_sys::Reflect::set(&obj, &"secureBoot".into(), &secure_boot.into())?;
        js_sys::Reflect::set(&obj, &"flashEncryption".into(), &flash_encryption.into())?;

        Ok(obj.into())
    }

    /// Flash firmware to the NET chip (ESP32).
    ///
    /// Pass an ELF file as Uint8Array - it will be converted to ESP-IDF bootloader format.
    /// The progress callback receives (phase: string, current: number, total: number).
    #[wasm_bindgen]
    pub async fn flash_net(&mut self, elf_data: js_sys::Uint8Array, progress_callback: js_sys::Function) -> Result<(), JsValue> {
        let elf_bytes = elf_data.to_vec();
        let core = self.core_mut()?;

        // Create a progress callback adapter
        // Callback signature: (phase: string, current: number, total: number)
        // where phase is "writing" or "verifying"
        struct JsProgressCallbacks {
            callback: js_sys::Function,
            total: usize,
            verifying: bool,
        }

        impl ProgressCallbacks for JsProgressCallbacks {
            fn init(&mut self, _addr: u32, total: usize) {
                self.total = total;
                self.verifying = false;
                // Report initial state
                let phase = if self.verifying { "verifying" } else { "writing" };
                let _ = self.callback.call3(
                    &JsValue::NULL,
                    &JsValue::from_str(phase),
                    &JsValue::from(0u32),
                    &JsValue::from(total as u32),
                );
            }
            fn update(&mut self, current: usize) {
                let phase = if self.verifying { "verifying" } else { "writing" };
                let _ = self.callback.call3(
                    &JsValue::NULL,
                    &JsValue::from_str(phase),
                    &JsValue::from(current as u32),
                    &JsValue::from(self.total as u32),
                );
            }
            fn finish(&mut self, _skipped: bool) {}
            fn verifying(&mut self) {
                self.verifying = true;
                // Reset position for verification phase
                let _ = self.callback.call3(
                    &JsValue::NULL,
                    &JsValue::from_str("verifying"),
                    &JsValue::from(0u32),
                    &JsValue::from(self.total as u32),
                );
            }
        }

        let mut progress = JsProgressCallbacks { callback: progress_callback, total: 0, verifying: false };
        core.flash_net(&elf_bytes, None, &mut progress, JsDelay).await
            .map_err(|e| JsValue::from_str(&format!("Flash error: {:?}", e)))
    }

    /// Erase the entire NET chip (ESP32) flash.
    #[wasm_bindgen]
    pub async fn erase_net(&mut self) -> Result<(), JsValue> {
        let core = self.core_mut()?;
        core.erase_net(JsDelay).await.map_err(|e| JsValue::from_str(&format!("Erase error: {:?}", e)))
    }

    // ==================== STATE AGGREGATION ====================

    /// Get all state variables from all chips.
    /// Returns a JSON object with all current device state.
    #[wasm_bindgen]
    pub async fn get_all_state(&mut self) -> Result<JsValue, JsValue> {
        let obj = js_sys::Object::new();

        // NET chip state (loaded first to diagnose ordering issues)
        let net_obj = js_sys::Object::new();
        match self.get_net_loopback_mode().await {
            Ok(loopback_mode) => {
                js_sys::Reflect::set(&net_obj, &"loopbackMode".into(), &loopback_mode.into())?;
            }
            Err(e) => {
                log(&format!("get_net_loopback_mode failed: {:?}", e));
            }
        }
        if let Ok(relay_url) = self.get_relay_url().await {
            js_sys::Reflect::set(&net_obj, &"relayUrl".into(), &relay_url.into())?;
        }
        if let Ok(networks) = self.get_wifi_networks().await {
            js_sys::Reflect::set(&net_obj, &"wifiNetworks".into(), &networks)?;
        }
        js_sys::Reflect::set(&obj, &"net".into(), &net_obj)?;

        // UI chip state
        let ui_obj = js_sys::Object::new();
        if let Ok(version) = self.get_version().await {
            js_sys::Reflect::set(&ui_obj, &"version".into(), &version.into())?;
        }
        if let Ok(sframe_key) = self.get_sframe_key().await {
            js_sys::Reflect::set(&ui_obj, &"sframeKey".into(), &sframe_key.into())?;
        }
        if let Ok(loopback_mode) = self.get_ui_loopback_mode().await {
            js_sys::Reflect::set(&ui_obj, &"loopbackMode".into(), &loopback_mode.into())?;
        }
        js_sys::Reflect::set(&obj, &"ui".into(), &ui_obj)?;

        Ok(obj.into())
    }
}

impl Default for LinkController {
    fn default() -> Self {
        Self::new()
    }
}

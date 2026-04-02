//! WebAssembly bindings for the Link CTL (Controller) interface.
//!
//! This crate provides a web-based interface to control Link devices
//! via the WebSerial API. It uses async I/O to communicate with the device.

mod serial;

use link::ctl::espflash::target::ProgressCallbacks;
use link::ctl::flash::{AsyncDelay, MgmtBootloaderEntry};
use link::ctl::stm;
use link::ctl::{CtlCore, CtlError, SetTimeout, escape_non_ascii};
use link::protocol_config::timeouts;
use link::{MgmtToCtl, NetLoopbackMode, UiLoopbackMode};
use serde::{Deserialize, Serialize};
use serial::{WebSerial, WebSerialAdapter};
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::JsFuture;

/// Initialize panic hook for better error messages.
#[wasm_bindgen(start)]
pub fn init() {
    console_error_panic_hook::set_once();
}

/// Log to browser console for debugging
fn console_log(s: &str) {
    web_sys::console::log_1(&JsValue::from_str(s));
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
#[derive(Clone, Copy)]
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
        Self { core: None }
    }

    /// Connect to a Link device via WebSerial.
    /// This will prompt the user to select a serial port.
    #[wasm_bindgen]
    pub async fn connect(&mut self, baud_rate: u32) -> Result<(), JsValue> {
        console_log(&format!("[web-ctl] connect() starting, baud_rate={}", baud_rate));
        let serial = WebSerial::new();
        console_log("[web-ctl] WebSerial created, calling connect...");
        serial
            .connect(baud_rate)
            .await
            .map_err(|e| JsValue::from_str(&format!("{}", e)))?;
        console_log("[web-ctl] WebSerial connected");

        // Wrap in adapter and create CtlCore
        let adapter = WebSerialAdapter::new(serial);
        self.core = Some(CtlCore::new(adapter));
        console_log("[web-ctl] CtlCore created");

        // Initialize DTR/RTS to known good state and wait for stabilization
        console_log("[web-ctl] calling init_port()...");
        let core = self.core.as_mut().unwrap();
        core.init_port(|ms| js_sleep(ms as u32)).await;
        console_log("[web-ctl] init_port() complete");

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
        Ok(())
    }

    /// Reconnect the port (close and reopen) to reset the MGMT chip.
    ///
    /// This preserves the port reference (no user gesture needed) while
    /// resetting the MGMT chip and clearing buffers. Use after MGMT flashing.
    #[wasm_bindgen]
    pub async fn reconnect(&mut self, baud_rate: u32) -> Result<(), JsValue> {
        let core = self.core_mut()?;
        core.port_mut()
            .reconnect(baud_rate)
            .await
            .map_err(|e| JsValue::from_str(&format!("{}", e)))?;
        Ok(())
    }

    /// Get CtlCore, returning error if not connected.
    fn core_mut(&mut self) -> Result<&mut CtlCore<WebSerialAdapter>, JsValue> {
        self.core
            .as_mut()
            .ok_or_else(|| JsValue::from_str("Not connected"))
    }

    /// Test connection with a Hello handshake.
    /// Returns true if the device responds correctly.
    #[wasm_bindgen]
    pub async fn hello(&mut self) -> Result<bool, JsValue> {
        console_log("[web-ctl] hello() starting");
        // Generate a random challenge
        let challenge: [u8; 4] = [
            (js_sys::Math::random() * 256.0) as u8,
            (js_sys::Math::random() * 256.0) as u8,
            (js_sys::Math::random() * 256.0) as u8,
            (js_sys::Math::random() * 256.0) as u8,
        ];
        console_log(&format!("[web-ctl] challenge: {:02x}{:02x}{:02x}{:02x}",
            challenge[0], challenge[1], challenge[2], challenge[3]));

        let core = self.core_mut()?;
        console_log("[web-ctl] calling core.hello()...");
        let result = core.hello(&challenge).await;
        console_log(&format!("[web-ctl] core.hello() returned: {}", result));
        if result {
            // Clear stale data from buffers and the WebSerial stream after
            // hello exchange, matching native ctl behavior. Without this,
            // accumulated FromNet/FromUi data causes subsequent operations
            // to hang or fail (read_tlv_mgmt spins through stale TLVs).
            console_log("[web-ctl] draining buffers...");
            core.drain();
            core.port_mut().drain().await.ok();
            console_log("[web-ctl] drain complete");
        }
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

        let core = self.core_mut()?;
        core.set_sframe_key(&key_bytes)
            .await
            .map_err(ctl_error_to_js)
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
        core.add_wifi_ssid(ssid, password)
            .await
            .map_err(ctl_error_to_js)
    }

    /// Clear all WiFi networks from NET chip storage.
    #[wasm_bindgen]
    pub async fn clear_wifi_networks(&mut self) -> Result<(), JsValue> {
        let core = self.core_mut()?;
        core.clear_wifi_ssids().await.map_err(ctl_error_to_js)
    }

    // ==================== UI PIN CONTROL ====================

    /// Set UI BOOT0 pin. 1 = high, 0 = low.
    #[wasm_bindgen]
    pub async fn set_ui_boot0(&mut self, high: bool) -> Result<(), JsValue> {
        use link::PinValue;
        let core = self.core_mut()?;
        let value = if high { PinValue::High } else { PinValue::Low };
        core.set_ui_boot0(value).await.map_err(ctl_error_to_js)
    }

    /// Set UI BOOT1 pin. 1 = high, 0 = low.
    #[wasm_bindgen]
    pub async fn set_ui_boot1(&mut self, high: bool) -> Result<(), JsValue> {
        use link::PinValue;
        let core = self.core_mut()?;
        let value = if high { PinValue::High } else { PinValue::Low };
        core.set_ui_boot1(value).await.map_err(ctl_error_to_js)
    }

    /// Set UI RST pin. 1 = high, 0 = low.
    #[wasm_bindgen]
    pub async fn set_ui_rst(&mut self, high: bool) -> Result<(), JsValue> {
        use link::PinValue;
        let core = self.core_mut()?;
        let value = if high { PinValue::High } else { PinValue::Low };
        core.set_ui_rst(value).await.map_err(ctl_error_to_js)
    }

    /// Reset the UI chip into bootloader mode.
    #[wasm_bindgen]
    pub async fn reset_ui_to_bootloader(&mut self) -> Result<(), JsValue> {
        let core = self.core_mut()?;
        core.reset_ui_to_bootloader(|ms| js_sleep(ms as u32))
            .await
            .map_err(ctl_error_to_js)
    }

    /// Reset the UI chip into user mode.
    #[wasm_bindgen]
    pub async fn reset_ui_to_user(&mut self) -> Result<(), JsValue> {
        let core = self.core_mut()?;
        core.reset_ui_to_user(|ms| js_sleep(ms as u32))
            .await
            .map_err(ctl_error_to_js)
    }

    // ==================== NET PIN CONTROL ====================

    /// Set NET BOOT pin (GPIO0). 1 = high, 0 = low.
    #[wasm_bindgen]
    pub async fn set_net_boot(&mut self, high: bool) -> Result<(), JsValue> {
        use link::PinValue;
        let core = self.core_mut()?;
        let value = if high { PinValue::High } else { PinValue::Low };
        core.set_net_boot(value).await.map_err(ctl_error_to_js)
    }

    /// Set NET RST pin (EN). 1 = high, 0 = low.
    #[wasm_bindgen]
    pub async fn set_net_rst(&mut self, high: bool) -> Result<(), JsValue> {
        use link::PinValue;
        let core = self.core_mut()?;
        let value = if high { PinValue::High } else { PinValue::Low };
        core.set_net_rst(value).await.map_err(ctl_error_to_js)
    }

    /// Reset the NET chip into bootloader mode.
    #[wasm_bindgen]
    pub async fn reset_net_to_bootloader(&mut self) -> Result<(), JsValue> {
        let core = self.core_mut()?;
        core.reset_net_to_bootloader(|ms| js_sleep(ms as u32))
            .await
            .map_err(ctl_error_to_js)
    }

    /// Reset the NET chip into user mode.
    #[wasm_bindgen]
    pub async fn reset_net_to_user(&mut self) -> Result<(), JsValue> {
        let core = self.core_mut()?;
        core.reset_net_to_user(|ms| js_sleep(ms as u32))
            .await
            .map_err(ctl_error_to_js)
    }

    /// Get UI chip loopback mode as string.
    /// Returns: "off", "raw", "alaw", or "sframe"
    #[wasm_bindgen]
    pub async fn get_ui_loopback_mode(&mut self) -> Result<String, JsValue> {
        let core = self.core_mut()?;
        let mode = core.ui_get_loopback().await.map_err(ctl_error_to_js)?;
        Ok(mode.to_string())
    }

    /// Set UI chip loopback mode.
    /// Modes: 0=off, 1=raw, 2=alaw, 3=sframe
    #[wasm_bindgen]
    pub async fn set_ui_loopback_mode(&mut self, mode: u8) -> Result<(), JsValue> {
        if mode > 3 {
            return Err(JsValue::from_str("Invalid loopback mode (0-3)"));
        }

        let loopback_mode = match mode {
            0 => UiLoopbackMode::Off,
            1 => UiLoopbackMode::Raw,
            2 => UiLoopbackMode::Alaw,
            3 => UiLoopbackMode::Sframe,
            _ => unreachable!(),
        };

        let core = self.core_mut()?;
        core.ui_set_loopback(loopback_mode)
            .await
            .map_err(ctl_error_to_js)
    }

    /// Get UI chip logs enabled state.
    #[wasm_bindgen]
    pub async fn get_ui_logs_enabled(&mut self) -> Result<bool, JsValue> {
        let core = self.core_mut()?;
        core.ui_get_logs_enabled().await.map_err(ctl_error_to_js)
    }

    /// Set UI chip logs enabled state.
    #[wasm_bindgen]
    pub async fn set_ui_logs_enabled(&mut self, enabled: bool) -> Result<(), JsValue> {
        let core = self.core_mut()?;
        core.ui_set_logs_enabled(enabled)
            .await
            .map_err(ctl_error_to_js)
    }

    /// Clear UI chip storage (EEPROM).
    #[wasm_bindgen]
    pub async fn clear_ui_storage(&mut self) -> Result<(), JsValue> {
        let core = self.core_mut()?;
        core.ui_clear_storage().await.map_err(ctl_error_to_js)
    }

    /// Get NET chip loopback mode as string.
    /// Returns: "off", "raw", or "moq"
    #[wasm_bindgen]
    pub async fn get_net_loopback_mode(&mut self) -> Result<String, JsValue> {
        let core = self.core_mut()?;
        let mode = core.net_get_loopback().await.map_err(ctl_error_to_js)?;
        Ok(mode.to_string())
    }

    /// Set NET chip loopback mode.
    /// Modes: 0=off, 1=raw, 2=moq
    #[wasm_bindgen]
    pub async fn set_net_loopback_mode(&mut self, mode: u8) -> Result<(), JsValue> {
        if mode > 2 {
            return Err(JsValue::from_str("Invalid loopback mode (0-2)"));
        }

        let loopback_mode = match mode {
            0 => NetLoopbackMode::Off,
            1 => NetLoopbackMode::Raw,
            2 => NetLoopbackMode::Moq,
            _ => unreachable!(),
        };

        let core = self.core_mut()?;
        core.net_set_loopback(loopback_mode)
            .await
            .map_err(ctl_error_to_js)
    }

    /// Get NET chip logs enabled state.
    #[wasm_bindgen]
    pub async fn get_net_logs_enabled(&mut self) -> Result<bool, JsValue> {
        let core = self.core_mut()?;
        core.net_get_logs_enabled().await.map_err(ctl_error_to_js)
    }

    /// Set NET chip logs enabled state.
    #[wasm_bindgen]
    pub async fn set_net_logs_enabled(&mut self, enabled: bool) -> Result<(), JsValue> {
        let core = self.core_mut()?;
        core.net_set_logs_enabled(enabled)
            .await
            .map_err(ctl_error_to_js)
    }

    /// Clear NET chip storage (NVS).
    #[wasm_bindgen]
    pub async fn clear_net_storage(&mut self) -> Result<(), JsValue> {
        let core = self.core_mut()?;
        core.net_clear_storage().await.map_err(ctl_error_to_js)
    }

    /// Get NET chip language setting.
    #[wasm_bindgen]
    pub async fn get_net_language(&mut self) -> Result<String, JsValue> {
        let core = self.core_mut()?;
        core.net_get_language().await.map_err(ctl_error_to_js)
    }

    /// Set NET chip language setting.
    #[wasm_bindgen]
    pub async fn set_net_language(&mut self, language: &str) -> Result<(), JsValue> {
        let core = self.core_mut()?;
        core.net_set_language(language)
            .await
            .map_err(ctl_error_to_js)
    }

    /// Get NET chip channel configuration (JSON string).
    #[wasm_bindgen]
    pub async fn get_net_channel(&mut self) -> Result<String, JsValue> {
        let core = self.core_mut()?;
        core.net_get_channel().await.map_err(ctl_error_to_js)
    }

    /// Set NET chip channel configuration (JSON string).
    #[wasm_bindgen]
    pub async fn set_net_channel(&mut self, channel: &str) -> Result<(), JsValue> {
        let core = self.core_mut()?;
        core.net_set_channel(channel).await.map_err(ctl_error_to_js)
    }

    /// Get NET chip AI configuration (JSON string).
    #[wasm_bindgen]
    pub async fn get_net_ai(&mut self) -> Result<String, JsValue> {
        let core = self.core_mut()?;
        core.net_get_ai().await.map_err(ctl_error_to_js)
    }

    /// Set NET chip AI configuration (JSON string).
    #[wasm_bindgen]
    pub async fn set_net_ai(&mut self, config: &str) -> Result<(), JsValue> {
        let core = self.core_mut()?;
        core.net_set_ai(config).await.map_err(ctl_error_to_js)
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

        let obj = js_sys::Object::new();
        js_sys::Reflect::set(&obj, &"stackBase".into(), &info.stack_base.into())?;
        js_sys::Reflect::set(&obj, &"stackTop".into(), &info.stack_top.into())?;
        js_sys::Reflect::set(&obj, &"stackSize".into(), &info.stack_size.into())?;
        js_sys::Reflect::set(&obj, &"stackUsed".into(), &info.stack_used.into())?;
        js_sys::Reflect::set(&obj, &"stackFree".into(), &info.stack_free().into())?;
        js_sys::Reflect::set(&obj, &"usagePercent".into(), &info.usage_percent().into())?;
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

        let obj = js_sys::Object::new();
        js_sys::Reflect::set(&obj, &"stackBase".into(), &info.stack_base.into())?;
        js_sys::Reflect::set(&obj, &"stackTop".into(), &info.stack_top.into())?;
        js_sys::Reflect::set(&obj, &"stackSize".into(), &info.stack_size.into())?;
        js_sys::Reflect::set(&obj, &"stackUsed".into(), &info.stack_used.into())?;
        js_sys::Reflect::set(&obj, &"stackFree".into(), &info.stack_free().into())?;
        js_sys::Reflect::set(&obj, &"usagePercent".into(), &info.usage_percent().into())?;
        Ok(obj.into())
    }

    /// Repaint UI chip stack (for fresh high-water mark measurement).
    #[wasm_bindgen]
    pub async fn repaint_ui_stack(&mut self) -> Result<(), JsValue> {
        let core = self.core_mut()?;
        core.ui_repaint_stack().await.map_err(ctl_error_to_js)
    }

    // ==================== CIRCULAR PING ====================

    /// Send a circular ping starting from UI (UI → NET → MGMT → CTL).
    #[wasm_bindgen]
    pub async fn circular_ping_via_ui(&mut self, data: Vec<u8>) -> Result<(), JsValue> {
        let core = self.core_mut()?;
        core.ui_first_circular_ping(&data)
            .await
            .map_err(ctl_error_to_js)
    }

    /// Send a circular ping starting from NET (NET → UI → MGMT → CTL).
    #[wasm_bindgen]
    pub async fn circular_ping_via_net(&mut self, data: Vec<u8>) -> Result<(), JsValue> {
        let core = self.core_mut()?;
        core.net_first_circular_ping(&data)
            .await
            .map_err(ctl_error_to_js)
    }

    // ==================== RESET HOLD ====================

    /// Hold the UI chip in reset.
    #[wasm_bindgen]
    pub async fn hold_ui_reset(&mut self) -> Result<(), JsValue> {
        let core = self.core_mut()?;
        core.hold_ui_reset().await.map_err(ctl_error_to_js)
    }

    /// Hold the NET chip in reset.
    #[wasm_bindgen]
    pub async fn hold_net_reset(&mut self) -> Result<(), JsValue> {
        let core = self.core_mut()?;
        core.hold_net_reset().await.map_err(ctl_error_to_js)
    }

    // ==================== FLASHING ====================

    /// Get MGMT chip bootloader information.
    ///
    /// Set `skip_init` to `true` if `try_enter_mgmt_bootloader` returned "auto_reset"
    /// (the probe already consumed the 0x7F init byte).
    #[wasm_bindgen]
    pub async fn get_mgmt_bootloader_info(&mut self, skip_init: bool) -> Result<JsValue, JsValue> {
        let core = self.core_mut()?;
        let info = core
            .get_mgmt_bootloader_info(skip_init)
            .await
            .map_err(|e| JsValue::from_str(&format!("Bootloader error: {:?}", e)))?;

        let obj = js_sys::Object::new();
        js_sys::Reflect::set(
            &obj,
            &"bootloaderVersion".into(),
            &info.bootloader_version.into(),
        )?;
        js_sys::Reflect::set(&obj, &"chipId".into(), &info.chip_id.into())?;
        js_sys::Reflect::set(
            &obj,
            &"chipName".into(),
            &stm::chip_name(info.chip_id).into(),
        )?;
        js_sys::Reflect::set(
            &obj,
            &"commandCount".into(),
            &(info.command_count as u32).into(),
        )?;

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
            js_sys::Reflect::set(
                &obj,
                &"flashSample".into(),
                &hex::encode(flash_sample).into(),
            )?;
            js_sys::Reflect::set(&obj, &"readProtected".into(), &false.into())?;
        } else {
            js_sys::Reflect::set(&obj, &"readProtected".into(), &true.into())?;
        }

        Ok(obj.into())
    }

    /// Drain any pending data from the serial buffer.
    ///
    /// Call this after a delay following manual reset to clear any
    /// stale data before communicating with the bootloader. This reads
    /// and discards data from both internal buffers and the WebSerial stream.
    #[wasm_bindgen]
    pub async fn drain_buffer(&mut self) -> Result<(), JsValue> {
        let core = self.core_mut()?;
        // Clear internal TLV buffers
        core.drain();
        // Also drain the underlying WebSerial stream
        core.port_mut()
            .drain()
            .await
            .map_err(|e| JsValue::from_str(&format!("Drain error: {:?}", e)))?;
        Ok(())
    }

    /// Try to enter MGMT bootloader mode automatically (EV16).
    ///
    /// This performs the DTR/RTS reset sequence, waits for the bootloader,
    /// and probes with 0x7F. Returns "auto_reset" if the bootloader responds,
    /// or "not_detected" if it doesn't (manual intervention required).
    #[wasm_bindgen]
    pub async fn try_enter_mgmt_bootloader(&mut self) -> Result<String, JsValue> {
        let core = self.core_mut()?;

        // Set short timeout for probing
        let _ = core
            .port_mut()
            .set_timeout(std::time::Duration::from_millis(
                timeouts::BOOTLOADER_PROBE_MS,
            ));

        let result = core
            .try_enter_mgmt_bootloader(|ms| js_sleep(ms as u32))
            .await;

        // Restore normal timeout
        let _ = core
            .port_mut()
            .set_timeout(std::time::Duration::from_secs(timeouts::NORMAL_SECS));

        match result {
            MgmtBootloaderEntry::AutoReset => Ok("auto_reset".to_string()),
            MgmtBootloaderEntry::AlreadyActive => Ok("auto_reset".to_string()),
            MgmtBootloaderEntry::NotDetected => Ok("not_detected".to_string()),
        }
    }

    /// Exit MGMT bootloader and jump to user code.
    ///
    /// Issues the STM32 Go command to jump to the application, and releases
    /// BOOT0 (RTS low). Call this after bootloader operations (info, flash).
    #[wasm_bindgen]
    pub async fn exit_mgmt_bootloader(&mut self) -> Result<(), JsValue> {
        let core = self.core_mut()?;
        core.exit_mgmt_bootloader(|ms| js_sleep(ms as u32)).await;
        Ok(())
    }

    /// Flash firmware to the MGMT chip (STM32F072CB).
    ///
    /// This method will first try to enter bootloader mode automatically (EV16).
    /// If the bootloader init fails, it returns an error - the JS should catch this
    /// and show the manual reset dialog, then call flash_mgmt_manual.
    ///
    /// Pass firmware as Uint8Array.
    /// The progress callback receives (phase: string, current: number, total: number).
    #[wasm_bindgen]
    pub async fn flash_mgmt(
        &mut self,
        firmware: js_sys::Uint8Array,
        progress_callback: js_sys::Function,
    ) -> Result<(), JsValue> {
        // Try automatic bootloader entry (DTR/RTS reset)
        let result = self.try_enter_mgmt_bootloader().await?;
        let skip_init = result == "auto_reset";

        let firmware_data = firmware.to_vec();
        let core = self.core_mut()?;

        core.flash_mgmt(
            &firmware_data,
            skip_init,
            |phase, current, total| {
                let phase_str = phase.to_string();
                let _ = progress_callback.call3(
                    &JsValue::NULL,
                    &JsValue::from_str(&phase_str),
                    &JsValue::from(current as u32),
                    &JsValue::from(total as u32),
                );
            },
            |ms| js_sleep(ms as u32),
        )
        .await
        .map_err(|e| JsValue::from_str(&format!("Flash error: {:?}", e)))
    }

    /// Flash firmware to the MGMT chip after manual bootloader entry.
    ///
    /// Use this after the user has manually reset the device to bootloader mode
    /// and drain_buffer() has been called. This just calls flash_mgmt directly.
    /// Pass firmware as Uint8Array.
    /// The progress callback receives (phase: string, current: number, total: number).
    #[wasm_bindgen]
    pub async fn flash_mgmt_manual(
        &mut self,
        firmware: js_sys::Uint8Array,
        progress_callback: js_sys::Function,
    ) -> Result<(), JsValue> {
        let firmware_data = firmware.to_vec();
        let core = self.core_mut()?;

        core.flash_mgmt(
            &firmware_data,
            false,
            |phase, current, total| {
                let phase_str = phase.to_string();
                let _ = progress_callback.call3(
                    &JsValue::NULL,
                    &JsValue::from_str(&phase_str),
                    &JsValue::from(current as u32),
                    &JsValue::from(total as u32),
                );
            },
            |ms| js_sleep(ms as u32),
        )
        .await
        .map_err(|e| JsValue::from_str(&format!("Flash error: {:?}", e)))
    }

    /// Get UI chip bootloader information.
    ///
    /// This resets the UI chip into bootloader mode, queries info, then resets back.
    #[wasm_bindgen]
    pub async fn get_ui_bootloader_info(&mut self) -> Result<JsValue, JsValue> {
        let core = self.core_mut()?;

        let info = core
            .get_ui_bootloader_info(|ms| js_sleep(ms as u32))
            .await
            .map_err(|e| JsValue::from_str(&format!("Bootloader error: {:?}", e)))?;

        let obj = js_sys::Object::new();
        js_sys::Reflect::set(
            &obj,
            &"bootloaderVersion".into(),
            &info.bootloader_version.into(),
        )?;
        js_sys::Reflect::set(&obj, &"chipId".into(), &info.chip_id.into())?;
        js_sys::Reflect::set(
            &obj,
            &"chipName".into(),
            &stm::chip_name(info.chip_id).into(),
        )?;
        js_sys::Reflect::set(
            &obj,
            &"commandCount".into(),
            &(info.command_count as u32).into(),
        )?;

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
            js_sys::Reflect::set(
                &obj,
                &"flashSample".into(),
                &hex::encode(flash_sample).into(),
            )?;
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
    pub async fn flash_ui(
        &mut self,
        firmware: js_sys::Uint8Array,
        verify: bool,
        progress_callback: js_sys::Function,
    ) -> Result<(), JsValue> {
        let firmware_data = firmware.to_vec();
        let core = self.core_mut()?;

        core.flash_ui(
            &firmware_data,
            |ms| js_sleep(ms as u32),
            verify,
            |phase, current, total| {
                let phase_str = phase.to_string();
                let _ = progress_callback.call3(
                    &JsValue::NULL,
                    &JsValue::from_str(&phase_str),
                    &JsValue::from(current as u32),
                    &JsValue::from(total as u32),
                );
            },
        )
        .await
        .map_err(|e| JsValue::from_str(&format!("Flash error: {:?}", e)))
    }

    /// Get NET chip (ESP32) bootloader information.
    ///
    /// Returns device info including chip type, flash size, MAC address, and security info.
    #[wasm_bindgen]
    pub async fn get_net_bootloader_info(&mut self) -> Result<JsValue, JsValue> {
        let core = self.core_mut()?;
        let info = core
            .get_net_bootloader_info(JsDelay)
            .await
            .map_err(|e| JsValue::from_str(&format!("Bootloader error: {:?}", e)))?;

        let obj = js_sys::Object::new();

        // Device info
        let device = &info.device_info;
        js_sys::Reflect::set(&obj, &"chip".into(), &format!("{:?}", device.chip).into())?;
        js_sys::Reflect::set(
            &obj,
            &"flashSize".into(),
            &format!("{:?}", device.flash_size)
                .trim_start_matches('_')
                .into(),
        )?;
        js_sys::Reflect::set(
            &obj,
            &"crystalFrequency".into(),
            &format!("{:?}", device.crystal_frequency)
                .trim_start_matches('_')
                .into(),
        )?;

        let features = js_sys::Array::new();
        for feature in &device.features {
            features.push(&JsValue::from_str(feature));
        }
        js_sys::Reflect::set(&obj, &"features".into(), &features)?;

        if let Some(mac) = &device.mac_address {
            js_sys::Reflect::set(&obj, &"macAddress".into(), &mac.clone().into())?;
        }

        // Security info
        let (secure_boot, flash_encryption) =
            link::ctl::interpret_esp32_security(&info.security_info);
        js_sys::Reflect::set(&obj, &"secureBoot".into(), &secure_boot.into())?;
        js_sys::Reflect::set(&obj, &"flashEncryption".into(), &flash_encryption.into())?;

        Ok(obj.into())
    }

    /// Flash firmware to the NET chip (ESP32).
    ///
    /// Pass an ELF file as Uint8Array - it will be converted to ESP-IDF bootloader format.
    /// Optionally pass a partition table as Uint8Array (CSV or binary format, auto-detected
    /// by espflash). When `None`, espflash uses its default single-app layout at 0x10000.
    /// The progress callback receives (phase: string, current: number, total: number).
    #[wasm_bindgen]
    pub async fn flash_net(
        &mut self,
        elf_data: js_sys::Uint8Array,
        partition_table: Option<js_sys::Uint8Array>,
        progress_callback: js_sys::Function,
    ) -> Result<(), JsValue> {
        let elf_bytes = elf_data.to_vec();
        let partition_table_bytes = partition_table.map(|pt| pt.to_vec());
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
                let phase = if self.verifying {
                    "verifying"
                } else {
                    "writing"
                };
                let _ = self.callback.call3(
                    &JsValue::NULL,
                    &JsValue::from_str(phase),
                    &JsValue::from(0u32),
                    &JsValue::from(total as u32),
                );
            }
            fn update(&mut self, current: usize) {
                let phase = if self.verifying {
                    "verifying"
                } else {
                    "writing"
                };
                let _ = self.callback.call3(
                    &JsValue::NULL,
                    &JsValue::from_str(phase),
                    &JsValue::from(current as u32),
                    &JsValue::from(self.total as u32),
                );
            }
            fn finish(&mut self, skipped: bool) {
                let phase = if skipped { "skipped" } else { "done" };
                let _ = self.callback.call3(
                    &JsValue::NULL,
                    &JsValue::from_str(phase),
                    &JsValue::from(self.total as u32),
                    &JsValue::from(self.total as u32),
                );
            }
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

        let mut progress = JsProgressCallbacks {
            callback: progress_callback,
            total: 0,
            verifying: false,
        };
        core.flash_net(
            &elf_bytes,
            partition_table_bytes.as_deref(),
            &mut progress,
            JsDelay,
            link::uart_config::HIGH_SPEED.baudrate,
        )
        .await
        .map_err(|e| JsValue::from_str(&format!("Flash error: {:?}", e)))
    }

    /// Erase the entire NET chip (ESP32) flash.
    #[wasm_bindgen]
    pub async fn erase_net(&mut self) -> Result<(), JsValue> {
        let core = self.core_mut()?;
        core.erase_net(JsDelay)
            .await
            .map_err(|e| JsValue::from_str(&format!("Erase error: {:?}", e)))
    }

    // ==================== MONITOR ====================

    /// Set port read timeout to 100ms for monitor polling.
    #[wasm_bindgen]
    pub fn set_monitor_timeout(&mut self) -> Result<(), JsValue> {
        let core = self.core_mut()?;
        let _ = core
            .port_mut()
            .set_timeout(std::time::Duration::from_millis(timeouts::MONITOR_MS));
        Ok(())
    }

    /// Restore port read timeout to normal operation.
    #[wasm_bindgen]
    pub fn restore_timeout(&mut self) -> Result<(), JsValue> {
        let core = self.core_mut()?;
        let _ = core
            .port_mut()
            .set_timeout(std::time::Duration::from_secs(timeouts::NORMAL_SECS));
        Ok(())
    }

    /// Read one UI log message (non-blocking with short timeout).
    /// Returns the message string or null if no data.
    #[wasm_bindgen]
    pub async fn monitor_read_ui_log(&mut self) -> Result<JsValue, JsValue> {
        let core = self.core_mut()?;
        match core.try_read_ui_log().await {
            Ok(Some(msg)) => Ok(JsValue::from_str(&msg)),
            Ok(None) => Ok(JsValue::NULL),
            Err(e) => Err(JsValue::from_str(&format!("{}", e))),
        }
    }

    /// Read one raw NET TLV (non-blocking with short timeout).
    /// Filters for MgmtToCtl::FromNet, returns payload as string or null.
    #[wasm_bindgen]
    pub async fn monitor_read_net_raw(&mut self) -> Result<JsValue, JsValue> {
        let core = self.core_mut()?;
        match core.read_tlv_raw().await {
            Ok(Some(tlv)) => {
                if tlv.tlv_type == MgmtToCtl::FromNet {
                    let text = escape_non_ascii(&tlv.value);
                    Ok(JsValue::from_str(&text))
                } else {
                    // Not a FromNet TLV, ignore
                    Ok(JsValue::NULL)
                }
            }
            Ok(None) => Ok(JsValue::NULL),
            Err(e) => Err(JsValue::from_str(&format!("{}", e))),
        }
    }
}

impl Default for LinkController {
    fn default() -> Self {
        Self::new()
    }
}

//! WebAssembly bindings for the Link CTL (Controller) interface.
//!
//! This crate provides a web-based interface to control Link devices
//! via the WebSerial API.

mod serial;

use link::ctl::{App, FlashPhase};
use serde::Serialize;
use serial::WebSerial;
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

/// The main controller interface exposed to JavaScript.
#[wasm_bindgen]
pub struct LinkController {
    serial: WebSerial,
    app: Option<App<WebSerial, WebSerial>>,
}

/// WiFi network configuration.
#[derive(Serialize)]
pub struct WifiNetwork {
    pub ssid: String,
    pub password: String,
}

/// Flash progress information.
#[derive(Serialize)]
pub struct FlashProgress {
    pub phase: String,
    pub current: usize,
    pub total: usize,
}

#[wasm_bindgen]
impl LinkController {
    /// Create a new LinkController instance.
    #[wasm_bindgen(constructor)]
    pub fn new() -> Self {
        Self {
            serial: WebSerial::new(),
            app: None,
        }
    }

    /// Connect to a Link device via WebSerial.
    /// This will prompt the user to select a serial port.
    #[wasm_bindgen]
    pub async fn connect(&mut self, baud_rate: u32) -> Result<(), JsValue> {
        self.serial
            .connect(baud_rate)
            .await
            .map_err(|e| JsValue::from_str(&format!("{}", e)))?;

        // Create the CTL app with the serial transport
        self.app = Some(App::new(self.serial.clone(), self.serial.clone()));

        log("Connected to Link device");
        Ok(())
    }

    /// Check if connected to a device.
    #[wasm_bindgen]
    pub fn is_connected(&self) -> bool {
        self.serial.is_connected()
    }

    /// Disconnect from the device.
    #[wasm_bindgen]
    pub async fn disconnect(&mut self) -> Result<(), JsValue> {
        self.app = None;
        self.serial
            .disconnect()
            .await
            .map_err(|e| JsValue::from_str(&format!("{}", e)))?;
        log("Disconnected from Link device");
        Ok(())
    }

    /// Test connection with a Hello handshake.
    /// Returns true if the device responds correctly.
    #[wasm_bindgen]
    pub async fn hello(&mut self) -> Result<bool, JsValue> {
        let app = self
            .app
            .as_mut()
            .ok_or_else(|| JsValue::from_str("Not connected"))?;

        // Generate a random challenge
        let challenge: [u8; 4] = [
            (js_sys::Math::random() * 256.0) as u8,
            (js_sys::Math::random() * 256.0) as u8,
            (js_sys::Math::random() * 256.0) as u8,
            (js_sys::Math::random() * 256.0) as u8,
        ];

        let result = app.hello(&challenge).await;
        Ok(result)
    }

    /// Get the firmware version stored in UI chip EEPROM.
    #[wasm_bindgen]
    pub async fn get_version(&mut self) -> Result<u32, JsValue> {
        let app = self
            .app
            .as_mut()
            .ok_or_else(|| JsValue::from_str("Not connected"))?;
        Ok(app.get_version().await)
    }

    /// Set the firmware version in UI chip EEPROM.
    #[wasm_bindgen]
    pub async fn set_version(&mut self, version: u32) -> Result<(), JsValue> {
        let app = self
            .app
            .as_mut()
            .ok_or_else(|| JsValue::from_str("Not connected"))?;
        app.set_version(version).await;
        Ok(())
    }

    /// Get the SFrame key from UI chip EEPROM.
    /// Returns the key as a hex string.
    #[wasm_bindgen]
    pub async fn get_sframe_key(&mut self) -> Result<String, JsValue> {
        let app = self
            .app
            .as_mut()
            .ok_or_else(|| JsValue::from_str("Not connected"))?;
        let key = app.get_sframe_key().await;
        Ok(hex::encode(key))
    }

    /// Set the SFrame key in UI chip EEPROM.
    /// Takes the key as a hex string (32 hex chars = 16 bytes).
    #[wasm_bindgen]
    pub async fn set_sframe_key(&mut self, key_hex: &str) -> Result<(), JsValue> {
        let app = self
            .app
            .as_mut()
            .ok_or_else(|| JsValue::from_str("Not connected"))?;

        let key_bytes = hex::decode(key_hex)
            .map_err(|e| JsValue::from_str(&format!("Invalid hex string: {}", e)))?;

        if key_bytes.len() != 16 {
            return Err(JsValue::from_str(
                "SFrame key must be 16 bytes (32 hex chars)",
            ));
        }

        let mut key = [0u8; 16];
        key.copy_from_slice(&key_bytes);
        app.set_sframe_key(&key).await;
        Ok(())
    }

    /// Get the relay URL from NET chip storage.
    #[wasm_bindgen]
    pub async fn get_relay_url(&mut self) -> Result<String, JsValue> {
        let app = self
            .app
            .as_mut()
            .ok_or_else(|| JsValue::from_str("Not connected"))?;
        let url = app.get_relay_url().await;
        Ok(url.to_string())
    }

    /// Set the relay URL in NET chip storage.
    #[wasm_bindgen]
    pub async fn set_relay_url(&mut self, url: &str) -> Result<(), JsValue> {
        let app = self
            .app
            .as_mut()
            .ok_or_else(|| JsValue::from_str("Not connected"))?;
        app.set_relay_url(url).await;
        Ok(())
    }

    /// Get all WiFi networks from NET chip storage.
    /// Returns a JSON array of {ssid, password} objects.
    #[wasm_bindgen]
    pub async fn get_wifi_networks(&mut self) -> Result<JsValue, JsValue> {
        let app = self
            .app
            .as_mut()
            .ok_or_else(|| JsValue::from_str("Not connected"))?;
        let ssids = app.get_wifi_ssids().await;

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
        let app = self
            .app
            .as_mut()
            .ok_or_else(|| JsValue::from_str("Not connected"))?;
        app.add_wifi_ssid(ssid, password).await;
        Ok(())
    }

    /// Clear all WiFi networks from NET chip storage.
    #[wasm_bindgen]
    pub async fn clear_wifi_networks(&mut self) -> Result<(), JsValue> {
        let app = self
            .app
            .as_mut()
            .ok_or_else(|| JsValue::from_str("Not connected"))?;
        app.clear_wifi_ssids().await;
        Ok(())
    }

    /// Reset the UI chip into bootloader mode.
    #[wasm_bindgen]
    pub async fn reset_ui_to_bootloader(&mut self) -> Result<(), JsValue> {
        let app = self
            .app
            .as_mut()
            .ok_or_else(|| JsValue::from_str("Not connected"))?;
        app.reset_ui_to_bootloader().await;
        Ok(())
    }

    /// Reset the UI chip into user mode.
    #[wasm_bindgen]
    pub async fn reset_ui_to_user(&mut self) -> Result<(), JsValue> {
        let app = self
            .app
            .as_mut()
            .ok_or_else(|| JsValue::from_str("Not connected"))?;
        app.reset_ui_to_user().await;
        Ok(())
    }

    /// Reset the NET chip into bootloader mode.
    #[wasm_bindgen]
    pub async fn reset_net_to_bootloader(&mut self) -> Result<(), JsValue> {
        let app = self
            .app
            .as_mut()
            .ok_or_else(|| JsValue::from_str("Not connected"))?;
        app.reset_net_to_bootloader().await;
        Ok(())
    }

    /// Reset the NET chip into user mode.
    #[wasm_bindgen]
    pub async fn reset_net_to_user(&mut self) -> Result<(), JsValue> {
        let app = self
            .app
            .as_mut()
            .ok_or_else(|| JsValue::from_str("Not connected"))?;
        app.reset_net_to_user().await;
        Ok(())
    }

    /// Flash firmware to the UI chip.
    /// The progress_callback receives JSON with {phase, current, total}.
    #[wasm_bindgen]
    pub async fn flash_ui(
        &mut self,
        firmware: Vec<u8>,
        progress_callback: js_sys::Function,
    ) -> Result<(), JsValue> {
        let app = self
            .app
            .as_mut()
            .ok_or_else(|| JsValue::from_str("Not connected"))?;

        // Create a delay function using setTimeout
        let delay_ms = |ms: u64| async move {
            let promise = js_sys::Promise::new(&mut |resolve, _reject| {
                let window = web_sys::window().unwrap();
                window
                    .set_timeout_with_callback_and_timeout_and_arguments_0(&resolve, ms as i32)
                    .unwrap();
            });
            wasm_bindgen_futures::JsFuture::from(promise).await.unwrap();
        };

        // Progress callback wrapper
        let progress = |phase: FlashPhase, current: usize, total: usize| {
            let phase_str = match phase {
                FlashPhase::Compressing => "compressing",
                FlashPhase::Erasing => "erasing",
                FlashPhase::Writing => "writing",
                FlashPhase::Verifying => "verifying",
            };

            let progress = FlashProgress {
                phase: phase_str.to_string(),
                current,
                total,
            };

            if let Ok(value) = serde_wasm_bindgen::to_value(&progress) {
                let _ = progress_callback.call1(&JsValue::NULL, &value);
            }
        };

        app.flash_ui(&firmware, delay_ms, true, progress)
            .await
            .map_err(|e| JsValue::from_str(&format!("Flash failed: {:?}", e)))?;

        Ok(())
    }

    /// Flash firmware to the NET chip.
    /// The progress_callback receives JSON with {phase, current, total}.
    #[wasm_bindgen]
    pub async fn flash_net(
        &mut self,
        firmware: Vec<u8>,
        address: u32,
        compress: bool,
        verify: bool,
        progress_callback: js_sys::Function,
    ) -> Result<(), JsValue> {
        let app = self
            .app
            .as_mut()
            .ok_or_else(|| JsValue::from_str("Not connected"))?;

        // Progress callback wrapper
        let progress = |phase: FlashPhase, current: usize, total: usize| {
            let phase_str = match phase {
                FlashPhase::Compressing => "compressing",
                FlashPhase::Erasing => "erasing",
                FlashPhase::Writing => "writing",
                FlashPhase::Verifying => "verifying",
            };

            let progress = FlashProgress {
                phase: phase_str.to_string(),
                current,
                total,
            };

            if let Ok(value) = serde_wasm_bindgen::to_value(&progress) {
                let _ = progress_callback.call1(&JsValue::NULL, &value);
            }
        };

        app.flash_net(&firmware, address, compress, verify, progress)
            .await
            .map_err(|e| JsValue::from_str(&format!("Flash failed: {:?}", e)))?;

        Ok(())
    }

    /// Get bootloader info from the UI chip.
    /// Returns JSON with {bootloader_version, chip_id, commands, flash_sample}.
    #[wasm_bindgen]
    pub async fn get_ui_bootloader_info(&mut self) -> Result<JsValue, JsValue> {
        let app = self
            .app
            .as_mut()
            .ok_or_else(|| JsValue::from_str("Not connected"))?;

        // Create a delay function
        let delay_ms = |ms: u64| async move {
            let promise = js_sys::Promise::new(&mut |resolve, _reject| {
                let window = web_sys::window().unwrap();
                window
                    .set_timeout_with_callback_and_timeout_and_arguments_0(&resolve, ms as i32)
                    .unwrap();
            });
            wasm_bindgen_futures::JsFuture::from(promise).await.unwrap();
        };

        let info = app
            .get_ui_bootloader_info(delay_ms)
            .await
            .map_err(|e| JsValue::from_str(&format!("Failed to get bootloader info: {:?}", e)))?;

        // Convert to a JS object
        let obj = js_sys::Object::new();
        js_sys::Reflect::set(
            &obj,
            &"bootloaderVersion".into(),
            &(info.bootloader_version as u32).into(),
        )?;
        js_sys::Reflect::set(
            &obj,
            &"chipId".into(),
            &format!("0x{:04X}", info.chip_id).into(),
        )?;

        let commands: Vec<String> = info.commands[..info.command_count]
            .iter()
            .map(|c| format!("0x{:02X}", c))
            .collect();
        let commands_array = js_sys::Array::new();
        for cmd in commands {
            commands_array.push(&cmd.into());
        }
        js_sys::Reflect::set(&obj, &"commands".into(), &commands_array)?;

        if let Some(sample) = info.flash_sample {
            js_sys::Reflect::set(&obj, &"flashSample".into(), &hex::encode(sample).into())?;
        }

        Ok(obj.into())
    }

    /// Get bootloader info from the NET chip.
    /// Returns JSON with security_info including chip type.
    #[wasm_bindgen]
    pub async fn get_net_bootloader_info(&mut self) -> Result<JsValue, JsValue> {
        let app = self
            .app
            .as_mut()
            .ok_or_else(|| JsValue::from_str("Not connected"))?;

        let info = app
            .get_net_bootloader_info()
            .await
            .map_err(|e| JsValue::from_str(&format!("Failed to get bootloader info: {:?}", e)))?;

        // Convert to a JS object
        let obj = js_sys::Object::new();
        js_sys::Reflect::set(
            &obj,
            &"chipType".into(),
            &format!("{:?}", info.security_info.chip_type).into(),
        )?;
        js_sys::Reflect::set(&obj, &"flags".into(), &info.security_info.flags.into())?;
        js_sys::Reflect::set(
            &obj,
            &"flashCryptCnt".into(),
            &info.security_info.flash_crypt_cnt.into(),
        )?;
        js_sys::Reflect::set(
            &obj,
            &"chipId".into(),
            &format!("0x{:08X}", info.security_info.chip_id).into(),
        )?;
        js_sys::Reflect::set(
            &obj,
            &"ecoVersion".into(),
            &info.security_info.eco_version.into(),
        )?;

        Ok(obj.into())
    }

    /// Ping the MGMT chip.
    #[wasm_bindgen]
    pub async fn ping_mgmt(&mut self, data: Vec<u8>) -> Result<(), JsValue> {
        let app = self
            .app
            .as_mut()
            .ok_or_else(|| JsValue::from_str("Not connected"))?;
        app.mgmt_ping(&data).await;
        Ok(())
    }

    /// Ping the UI chip.
    #[wasm_bindgen]
    pub async fn ping_ui(&mut self, data: Vec<u8>) -> Result<(), JsValue> {
        let app = self
            .app
            .as_mut()
            .ok_or_else(|| JsValue::from_str("Not connected"))?;
        app.ui_ping(&data).await;
        Ok(())
    }

    /// Ping the NET chip.
    #[wasm_bindgen]
    pub async fn ping_net(&mut self, data: Vec<u8>) -> Result<(), JsValue> {
        let app = self
            .app
            .as_mut()
            .ok_or_else(|| JsValue::from_str("Not connected"))?;
        app.net_ping(&data).await;
        Ok(())
    }

    /// Get bootloader info from the MGMT chip.
    /// NOTE: MGMT chip must already be in bootloader mode (BOOT0 high + reset).
    /// Returns JSON with {bootloaderVersion, chipId, commands, flashSample}.
    #[wasm_bindgen]
    pub async fn get_mgmt_bootloader_info(&mut self) -> Result<JsValue, JsValue> {
        let app = self
            .app
            .as_mut()
            .ok_or_else(|| JsValue::from_str("Not connected"))?;

        let info = app
            .get_mgmt_bootloader_info()
            .await
            .map_err(|e| JsValue::from_str(&format!("Failed to get bootloader info: {:?}", e)))?;

        // Convert to a JS object
        let obj = js_sys::Object::new();
        js_sys::Reflect::set(
            &obj,
            &"bootloaderVersion".into(),
            &(info.bootloader_version as u32).into(),
        )?;
        js_sys::Reflect::set(
            &obj,
            &"chipId".into(),
            &format!("0x{:04X}", info.chip_id).into(),
        )?;

        let commands: Vec<String> = info.commands[..info.command_count]
            .iter()
            .map(|c| format!("0x{:02X}", c))
            .collect();
        let commands_array = js_sys::Array::new();
        for cmd in commands {
            commands_array.push(&cmd.into());
        }
        js_sys::Reflect::set(&obj, &"commands".into(), &commands_array)?;

        if let Some(sample) = info.flash_sample {
            js_sys::Reflect::set(&obj, &"flashSample".into(), &hex::encode(sample).into())?;
        }

        Ok(obj.into())
    }

    /// Flash firmware to the MGMT chip.
    /// NOTE: MGMT chip must already be in bootloader mode (BOOT0 high + reset).
    /// The progress_callback receives JSON with {phase, current, total}.
    #[wasm_bindgen]
    pub async fn flash_mgmt(
        &mut self,
        firmware: Vec<u8>,
        progress_callback: js_sys::Function,
    ) -> Result<(), JsValue> {
        let app = self
            .app
            .as_mut()
            .ok_or_else(|| JsValue::from_str("Not connected"))?;

        // Progress callback wrapper
        let progress = |phase: FlashPhase, current: usize, total: usize| {
            let phase_str = match phase {
                FlashPhase::Compressing => "compressing",
                FlashPhase::Erasing => "erasing",
                FlashPhase::Writing => "writing",
                FlashPhase::Verifying => "verifying",
            };

            let progress = FlashProgress {
                phase: phase_str.to_string(),
                current,
                total,
            };

            if let Ok(value) = serde_wasm_bindgen::to_value(&progress) {
                let _ = progress_callback.call1(&JsValue::NULL, &value);
            }
        };

        app.flash_mgmt(&firmware, progress)
            .await
            .map_err(|e| JsValue::from_str(&format!("Flash failed: {:?}", e)))?;

        Ok(())
    }

    /// Send a WebSocket ping through the NET chip.
    #[wasm_bindgen]
    pub async fn ws_ping(&mut self, data: Vec<u8>) -> Result<(), JsValue> {
        let app = self
            .app
            .as_mut()
            .ok_or_else(|| JsValue::from_str("Not connected"))?;
        app.ws_ping(&data).await;
        Ok(())
    }

    /// Get UI chip loopback mode.
    #[wasm_bindgen]
    pub async fn get_ui_loopback(&mut self) -> Result<bool, JsValue> {
        let app = self
            .app
            .as_mut()
            .ok_or_else(|| JsValue::from_str("Not connected"))?;
        Ok(app.ui_get_loopback().await)
    }

    /// Set UI chip loopback mode.
    #[wasm_bindgen]
    pub async fn set_ui_loopback(&mut self, enabled: bool) -> Result<(), JsValue> {
        let app = self
            .app
            .as_mut()
            .ok_or_else(|| JsValue::from_str("Not connected"))?;
        app.ui_set_loopback(enabled).await;
        Ok(())
    }

    /// Get NET chip loopback mode.
    #[wasm_bindgen]
    pub async fn get_net_loopback(&mut self) -> Result<bool, JsValue> {
        let app = self
            .app
            .as_mut()
            .ok_or_else(|| JsValue::from_str("Not connected"))?;
        Ok(app.net_get_loopback().await)
    }

    /// Set NET chip loopback mode.
    #[wasm_bindgen]
    pub async fn set_net_loopback(&mut self, enabled: bool) -> Result<(), JsValue> {
        let app = self
            .app
            .as_mut()
            .ok_or_else(|| JsValue::from_str("Not connected"))?;
        app.net_set_loopback(enabled).await;
        Ok(())
    }

    /// Get all state variables from all chips.
    /// Returns a JSON object with all current device state.
    #[wasm_bindgen]
    pub async fn get_all_state(&mut self) -> Result<JsValue, JsValue> {
        let app = self
            .app
            .as_mut()
            .ok_or_else(|| JsValue::from_str("Not connected"))?;

        let obj = js_sys::Object::new();

        // UI chip state
        let ui_obj = js_sys::Object::new();
        let version = app.get_version().await;
        js_sys::Reflect::set(&ui_obj, &"version".into(), &version.into())?;

        let sframe_key = app.get_sframe_key().await;
        js_sys::Reflect::set(
            &ui_obj,
            &"sframeKey".into(),
            &hex::encode(sframe_key).into(),
        )?;

        let ui_loopback = app.ui_get_loopback().await;
        js_sys::Reflect::set(&ui_obj, &"loopback".into(), &ui_loopback.into())?;

        js_sys::Reflect::set(&obj, &"ui".into(), &ui_obj)?;

        // NET chip state
        let net_obj = js_sys::Object::new();
        let relay_url = app.get_relay_url().await;
        js_sys::Reflect::set(&net_obj, &"relayUrl".into(), &relay_url.to_string().into())?;

        let ssids = app.get_wifi_ssids().await;
        let networks: Vec<WifiNetwork> = ssids
            .iter()
            .map(|s| WifiNetwork {
                ssid: s.ssid.to_string(),
                password: s.password.to_string(),
            })
            .collect();
        let networks_value = serde_wasm_bindgen::to_value(&networks)
            .map_err(|e| JsValue::from_str(&format!("Serialization error: {}", e)))?;
        js_sys::Reflect::set(&net_obj, &"wifiNetworks".into(), &networks_value)?;

        let net_loopback = app.net_get_loopback().await;
        js_sys::Reflect::set(&net_obj, &"loopback".into(), &net_loopback.into())?;

        js_sys::Reflect::set(&obj, &"net".into(), &net_obj)?;

        Ok(obj.into())
    }
}

impl Default for LinkController {
    fn default() -> Self {
        Self::new()
    }
}

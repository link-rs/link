//! WebAssembly bindings for the Link CTL (Controller) interface.
//!
//! This crate provides a web-based interface to control Link devices
//! via the WebSerial API. It uses async I/O to communicate with the device.

mod serial;

use embedded_io_async::{Read, Write};
use link::{
    CtlToMgmt, LoopbackMode, MgmtToCtl, MgmtToNet, MgmtToUi, NetLoopback, NetToMgmt, UiToMgmt,
    ReadTlv, WriteTlv, Tlv, MAX_VALUE_SIZE, HEADER_SIZE, SYNC_WORD,
};
use serde::{Deserialize, Serialize};
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

/// WiFi network configuration.
#[derive(Serialize, Deserialize, Clone)]
pub struct WifiNetwork {
    pub ssid: String,
    pub password: String,
}

// ============================================================================
// Async TLV I/O helpers
// ============================================================================

/// Write a TLV message with sync word prefix.
async fn write_tlv<T, W>(writer: &mut W, tlv_type: T, value: &[u8]) -> Result<(), W::Error>
where
    T: Into<u16> + Copy,
    W: Write,
{
    writer.write_tlv(tlv_type, value).await
}

/// Write a tunneled TLV message to UI through MGMT.
async fn write_tlv_ui<W>(writer: &mut W, tlv_type: MgmtToUi, value: &[u8]) -> Result<(), W::Error>
where
    W: Write,
{
    // Create the inner TLV (sync word + header + value)
    let inner_type: u16 = tlv_type.into();
    let mut inner = heapless::Vec::<u8, MAX_VALUE_SIZE>::new();
    let _ = inner.extend_from_slice(&SYNC_WORD);
    let _ = inner.extend_from_slice(&inner_type.to_be_bytes());
    let _ = inner.extend_from_slice(&(value.len() as u32).to_be_bytes());
    let _ = inner.extend_from_slice(value);

    // Wrap in CtlToMgmt::ToUi
    writer.write_tlv(CtlToMgmt::ToUi, &inner).await
}

/// Write a tunneled TLV message to NET through MGMT.
async fn write_tlv_net<W>(writer: &mut W, tlv_type: MgmtToNet, value: &[u8]) -> Result<(), W::Error>
where
    W: Write,
{
    // Create the inner TLV (sync word + header + value)
    let inner_type: u16 = tlv_type.into();
    let mut inner = heapless::Vec::<u8, MAX_VALUE_SIZE>::new();
    let _ = inner.extend_from_slice(&SYNC_WORD);
    let _ = inner.extend_from_slice(&inner_type.to_be_bytes());
    let _ = inner.extend_from_slice(&(value.len() as u32).to_be_bytes());
    let _ = inner.extend_from_slice(value);

    // Wrap in CtlToMgmt::ToNet
    writer.write_tlv(CtlToMgmt::ToNet, &inner).await
}

/// Read a TLV response, optionally extracting tunneled responses.
async fn read_tlv_mgmt<R>(reader: &mut R) -> Option<Tlv<MgmtToCtl>>
where
    R: Read,
{
    reader.read_tlv().await.ok().flatten()
}

/// Read a TLV from UI, unwrapping the FromUi tunnel.
async fn read_tlv_ui<R>(reader: &mut R) -> Option<Tlv<UiToMgmt>>
where
    R: Read,
{
    loop {
        let tlv: Tlv<MgmtToCtl> = reader.read_tlv().await.ok()??;
        if tlv.tlv_type == MgmtToCtl::FromUi {
            // Parse the inner TLV from the value
            return parse_inner_tlv(&tlv.value);
        }
        // Skip other messages
    }
}

/// Read a TLV from NET, unwrapping the FromNet tunnel.
async fn read_tlv_net<R>(reader: &mut R) -> Option<Tlv<NetToMgmt>>
where
    R: Read,
{
    loop {
        let tlv: Tlv<MgmtToCtl> = reader.read_tlv().await.ok()??;
        if tlv.tlv_type == MgmtToCtl::FromNet {
            // Parse the inner TLV from the value
            return parse_inner_tlv(&tlv.value);
        }
        // Skip other messages
    }
}

/// Parse an inner TLV from a tunneled message value.
fn parse_inner_tlv<T>(data: &[u8]) -> Option<Tlv<T>>
where
    T: TryFrom<u16>,
{
    // Data format: [sync_word (4)] [type (2)] [length (4)] [value...]
    if data.len() < SYNC_WORD.len() + HEADER_SIZE {
        return None;
    }

    // Skip sync word, parse header
    let offset = SYNC_WORD.len();
    let tlv_type_raw = u16::from_be_bytes([data[offset], data[offset + 1]]);
    let length = u32::from_be_bytes([
        data[offset + 2],
        data[offset + 3],
        data[offset + 4],
        data[offset + 5],
    ]) as usize;

    let value_start = offset + HEADER_SIZE;
    if data.len() < value_start + length {
        return None;
    }

    let tlv_type = T::try_from(tlv_type_raw).ok()?;
    let mut value = heapless::Vec::new();
    let _ = value.extend_from_slice(&data[value_start..value_start + length]);

    Some(Tlv { tlv_type, value })
}

// ============================================================================
// LinkController
// ============================================================================

/// The main controller interface exposed to JavaScript.
#[wasm_bindgen]
pub struct LinkController {
    serial: WebSerial,
}

#[wasm_bindgen]
impl LinkController {
    /// Create a new LinkController instance.
    #[wasm_bindgen(constructor)]
    pub fn new() -> Self {
        Self {
            serial: WebSerial::new(),
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
        const MAGIC: &[u8; 4] = b"LINK";

        // Generate a random challenge
        let challenge: [u8; 4] = [
            (js_sys::Math::random() * 256.0) as u8,
            (js_sys::Math::random() * 256.0) as u8,
            (js_sys::Math::random() * 256.0) as u8,
            (js_sys::Math::random() * 256.0) as u8,
        ];

        let _ = write_tlv(&mut self.serial, CtlToMgmt::Hello, &challenge).await;

        let tlv: Tlv<MgmtToCtl> = match read_tlv_mgmt(&mut self.serial).await {
            Some(tlv) => tlv,
            None => return Ok(false),
        };

        if tlv.tlv_type != MgmtToCtl::Hello || tlv.value.len() != 4 {
            return Ok(false);
        }

        // Verify response is challenge XOR'd with MAGIC
        for i in 0..4 {
            if tlv.value[i] != (challenge[i] ^ MAGIC[i]) {
                return Ok(false);
            }
        }
        Ok(true)
    }

    /// Get the firmware version stored in UI chip EEPROM.
    #[wasm_bindgen]
    pub async fn get_version(&mut self) -> Result<u32, JsValue> {
        let _ = write_tlv_ui(&mut self.serial, MgmtToUi::GetVersion, &[]).await;

        let tlv = read_tlv_ui(&mut self.serial)
            .await
            .ok_or_else(|| JsValue::from_str("No response"))?;

        if tlv.tlv_type != UiToMgmt::Version || tlv.value.len() != 4 {
            return Err(JsValue::from_str("Invalid response"));
        }

        Ok(u32::from_be_bytes([
            tlv.value[0],
            tlv.value[1],
            tlv.value[2],
            tlv.value[3],
        ]))
    }

    /// Set the firmware version in UI chip EEPROM.
    #[wasm_bindgen]
    pub async fn set_version(&mut self, version: u32) -> Result<(), JsValue> {
        let _ = write_tlv_ui(&mut self.serial, MgmtToUi::SetVersion, &version.to_be_bytes()).await;

        let tlv = read_tlv_ui(&mut self.serial)
            .await
            .ok_or_else(|| JsValue::from_str("No response"))?;

        if tlv.tlv_type != UiToMgmt::Ack {
            return Err(JsValue::from_str("Expected Ack"));
        }
        Ok(())
    }

    /// Get the SFrame key from UI chip EEPROM.
    /// Returns the key as a hex string.
    #[wasm_bindgen]
    pub async fn get_sframe_key(&mut self) -> Result<String, JsValue> {
        let _ = write_tlv_ui(&mut self.serial, MgmtToUi::GetSFrameKey, &[]).await;

        let tlv = read_tlv_ui(&mut self.serial)
            .await
            .ok_or_else(|| JsValue::from_str("No response"))?;

        if tlv.tlv_type != UiToMgmt::SFrameKey || tlv.value.len() != 16 {
            return Err(JsValue::from_str("Invalid response"));
        }

        Ok(hex::encode(&tlv.value))
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

        let _ = write_tlv_ui(&mut self.serial, MgmtToUi::SetSFrameKey, &key_bytes).await;

        let tlv = read_tlv_ui(&mut self.serial)
            .await
            .ok_or_else(|| JsValue::from_str("No response"))?;

        if tlv.tlv_type != UiToMgmt::Ack {
            return Err(JsValue::from_str("Expected Ack"));
        }
        Ok(())
    }

    /// Get the relay URL from NET chip storage.
    #[wasm_bindgen]
    pub async fn get_relay_url(&mut self) -> Result<String, JsValue> {
        let _ = write_tlv_net(&mut self.serial, MgmtToNet::GetRelayUrl, &[]).await;

        let tlv = read_tlv_net(&mut self.serial)
            .await
            .ok_or_else(|| JsValue::from_str("No response"))?;

        if tlv.tlv_type != NetToMgmt::RelayUrl {
            return Err(JsValue::from_str("Expected RelayUrl"));
        }

        String::from_utf8(tlv.value.to_vec())
            .map_err(|e| JsValue::from_str(&format!("Invalid UTF-8: {}", e)))
    }

    /// Set the relay URL in NET chip storage.
    #[wasm_bindgen]
    pub async fn set_relay_url(&mut self, url: &str) -> Result<(), JsValue> {
        let _ = write_tlv_net(&mut self.serial, MgmtToNet::SetRelayUrl, url.as_bytes()).await;

        let tlv = read_tlv_net(&mut self.serial)
            .await
            .ok_or_else(|| JsValue::from_str("No response"))?;

        if tlv.tlv_type != NetToMgmt::Ack {
            return Err(JsValue::from_str("Expected Ack"));
        }
        Ok(())
    }

    /// Get all WiFi networks from NET chip storage.
    /// Returns a JSON array of {ssid, password} objects.
    #[wasm_bindgen]
    pub async fn get_wifi_networks(&mut self) -> Result<JsValue, JsValue> {
        let _ = write_tlv_net(&mut self.serial, MgmtToNet::GetWifiSsids, &[]).await;

        let tlv = read_tlv_net(&mut self.serial)
            .await
            .ok_or_else(|| JsValue::from_str("No response"))?;

        if tlv.tlv_type != NetToMgmt::WifiSsids {
            return Err(JsValue::from_str("Expected WifiSsids"));
        }

        // Parse the WiFi SSIDs from postcard format
        let ssids: heapless::Vec<link::WifiSsid, 8> =
            postcard::from_bytes(&tlv.value).unwrap_or_default();

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
        // Serialize as postcard: ssid string + password string
        let wifi = link::WifiSsid {
            ssid: ssid.into(),
            password: password.into(),
        };
        let mut buf = [0u8; 256];
        let data = postcard::to_slice(&wifi, &mut buf)
            .map_err(|e| JsValue::from_str(&format!("Serialization error: {:?}", e)))?;

        let _ = write_tlv_net(&mut self.serial, MgmtToNet::AddWifiSsid, data).await;

        let tlv = read_tlv_net(&mut self.serial)
            .await
            .ok_or_else(|| JsValue::from_str("No response"))?;

        if tlv.tlv_type != NetToMgmt::Ack {
            return Err(JsValue::from_str("Expected Ack"));
        }
        Ok(())
    }

    /// Clear all WiFi networks from NET chip storage.
    #[wasm_bindgen]
    pub async fn clear_wifi_networks(&mut self) -> Result<(), JsValue> {
        let _ = write_tlv_net(&mut self.serial, MgmtToNet::ClearWifiSsids, &[]).await;

        let tlv = read_tlv_net(&mut self.serial)
            .await
            .ok_or_else(|| JsValue::from_str("No response"))?;

        if tlv.tlv_type != NetToMgmt::Ack {
            return Err(JsValue::from_str("Expected Ack"));
        }
        Ok(())
    }

    /// Reset the UI chip into bootloader mode.
    #[wasm_bindgen]
    pub async fn reset_ui_to_bootloader(&mut self) -> Result<(), JsValue> {
        let _ = write_tlv(&mut self.serial, CtlToMgmt::ResetUiToBootloader, &[]).await;

        // Wait for Ack
        loop {
            let tlv = read_tlv_mgmt(&mut self.serial)
                .await
                .ok_or_else(|| JsValue::from_str("No response"))?;
            match tlv.tlv_type {
                MgmtToCtl::FromUi | MgmtToCtl::FromNet => continue, // Skip tunneled messages
                MgmtToCtl::Ack => return Ok(()),
                _ => return Err(JsValue::from_str("Unexpected response")),
            }
        }
    }

    /// Reset the UI chip into user mode.
    #[wasm_bindgen]
    pub async fn reset_ui_to_user(&mut self) -> Result<(), JsValue> {
        let _ = write_tlv(&mut self.serial, CtlToMgmt::ResetUiToUser, &[]).await;

        loop {
            let tlv = read_tlv_mgmt(&mut self.serial)
                .await
                .ok_or_else(|| JsValue::from_str("No response"))?;
            match tlv.tlv_type {
                MgmtToCtl::FromUi | MgmtToCtl::FromNet => continue,
                MgmtToCtl::Ack => return Ok(()),
                _ => return Err(JsValue::from_str("Unexpected response")),
            }
        }
    }

    /// Reset the NET chip into bootloader mode.
    #[wasm_bindgen]
    pub async fn reset_net_to_bootloader(&mut self) -> Result<(), JsValue> {
        let _ = write_tlv(&mut self.serial, CtlToMgmt::ResetNetToBootloader, &[]).await;

        loop {
            let tlv = read_tlv_mgmt(&mut self.serial)
                .await
                .ok_or_else(|| JsValue::from_str("No response"))?;
            match tlv.tlv_type {
                MgmtToCtl::FromUi | MgmtToCtl::FromNet => continue,
                MgmtToCtl::Ack => return Ok(()),
                _ => return Err(JsValue::from_str("Unexpected response")),
            }
        }
    }

    /// Reset the NET chip into user mode.
    #[wasm_bindgen]
    pub async fn reset_net_to_user(&mut self) -> Result<(), JsValue> {
        let _ = write_tlv(&mut self.serial, CtlToMgmt::ResetNetToUser, &[]).await;

        loop {
            let tlv = read_tlv_mgmt(&mut self.serial)
                .await
                .ok_or_else(|| JsValue::from_str("No response"))?;
            match tlv.tlv_type {
                MgmtToCtl::FromUi | MgmtToCtl::FromNet => continue,
                MgmtToCtl::Ack => return Ok(()),
                _ => return Err(JsValue::from_str("Unexpected response")),
            }
        }
    }

    /// Get UI chip loopback mode as string.
    /// Returns: "off", "raw", "alaw", or "sframe"
    #[wasm_bindgen]
    pub async fn get_ui_loopback_mode(&mut self) -> Result<String, JsValue> {
        let _ = write_tlv_ui(&mut self.serial, MgmtToUi::GetLoopback, &[]).await;

        let tlv = read_tlv_ui(&mut self.serial)
            .await
            .ok_or_else(|| JsValue::from_str("No response"))?;

        if tlv.tlv_type != UiToMgmt::Loopback || tlv.value.is_empty() {
            return Err(JsValue::from_str("Invalid response"));
        }

        let mode = LoopbackMode::try_from(tlv.value[0]).unwrap_or(LoopbackMode::Off);
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

        let _ = write_tlv_ui(&mut self.serial, MgmtToUi::SetLoopback, &[mode]).await;

        let tlv = read_tlv_ui(&mut self.serial)
            .await
            .ok_or_else(|| JsValue::from_str("No response"))?;

        if tlv.tlv_type != UiToMgmt::Ack {
            return Err(JsValue::from_str("Expected Ack"));
        }
        Ok(())
    }

    /// Get UI chip loopback mode (legacy boolean API).
    #[wasm_bindgen]
    pub async fn get_ui_loopback(&mut self) -> Result<bool, JsValue> {
        let mode = self.get_ui_loopback_mode().await?;
        Ok(mode != "off")
    }

    /// Set UI chip loopback mode (legacy boolean API).
    #[wasm_bindgen]
    pub async fn set_ui_loopback(&mut self, enabled: bool) -> Result<(), JsValue> {
        let mode = if enabled { 1 } else { 0 };
        self.set_ui_loopback_mode(mode).await
    }

    /// Get NET chip loopback mode as string.
    /// Returns: "off", "raw", or "moq"
    #[wasm_bindgen]
    pub async fn get_net_loopback_mode(&mut self) -> Result<String, JsValue> {
        let _ = write_tlv_net(&mut self.serial, MgmtToNet::GetLoopback, &[]).await;

        let tlv = read_tlv_net(&mut self.serial)
            .await
            .ok_or_else(|| JsValue::from_str("No response"))?;

        if tlv.tlv_type != NetToMgmt::Loopback || tlv.value.is_empty() {
            return Err(JsValue::from_str("Invalid response"));
        }

        let mode = NetLoopback::try_from(tlv.value[0]).unwrap_or(NetLoopback::Off);
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

        let _ = write_tlv_net(&mut self.serial, MgmtToNet::SetLoopback, &[mode]).await;

        let tlv = read_tlv_net(&mut self.serial)
            .await
            .ok_or_else(|| JsValue::from_str("No response"))?;

        if tlv.tlv_type != NetToMgmt::Ack {
            return Err(JsValue::from_str("Expected Ack"));
        }
        Ok(())
    }

    /// Get NET chip loopback mode (legacy boolean API).
    #[wasm_bindgen]
    pub async fn get_net_loopback(&mut self) -> Result<bool, JsValue> {
        let mode = self.get_net_loopback_mode().await?;
        Ok(mode != "off")
    }

    /// Set NET chip loopback mode (legacy boolean API).
    #[wasm_bindgen]
    pub async fn set_net_loopback(&mut self, enabled: bool) -> Result<(), JsValue> {
        let mode = if enabled { 1 } else { 0 };
        self.set_net_loopback_mode(mode).await
    }

    /// Ping the MGMT chip.
    #[wasm_bindgen]
    pub async fn ping_mgmt(&mut self, data: Vec<u8>) -> Result<(), JsValue> {
        let _ = write_tlv(&mut self.serial, CtlToMgmt::Ping, &data).await;

        let tlv = read_tlv_mgmt(&mut self.serial)
            .await
            .ok_or_else(|| JsValue::from_str("No response"))?;

        if tlv.tlv_type != MgmtToCtl::Pong || tlv.value.as_slice() != data.as_slice() {
            return Err(JsValue::from_str("Ping failed"));
        }
        Ok(())
    }

    /// Ping the UI chip.
    #[wasm_bindgen]
    pub async fn ping_ui(&mut self, data: Vec<u8>) -> Result<(), JsValue> {
        let _ = write_tlv_ui(&mut self.serial, MgmtToUi::Ping, &data).await;

        let tlv = read_tlv_ui(&mut self.serial)
            .await
            .ok_or_else(|| JsValue::from_str("No response"))?;

        if tlv.tlv_type != UiToMgmt::Pong || tlv.value.as_slice() != data.as_slice() {
            return Err(JsValue::from_str("Ping failed"));
        }
        Ok(())
    }

    /// Ping the NET chip.
    #[wasm_bindgen]
    pub async fn ping_net(&mut self, data: Vec<u8>) -> Result<(), JsValue> {
        let _ = write_tlv_net(&mut self.serial, MgmtToNet::Ping, &data).await;

        let tlv = read_tlv_net(&mut self.serial)
            .await
            .ok_or_else(|| JsValue::from_str("No response"))?;

        if tlv.tlv_type != NetToMgmt::Pong || tlv.value.as_slice() != data.as_slice() {
            return Err(JsValue::from_str("Ping failed"));
        }
        Ok(())
    }

    // ==================== STACK DIAGNOSTICS ====================

    /// Get MGMT chip stack usage information.
    /// Returns JSON with {stack_base, stack_top, stack_size, stack_used, stack_free, usage_percent}.
    #[wasm_bindgen]
    pub async fn get_mgmt_stack_info(&mut self) -> Result<JsValue, JsValue> {
        let _ = write_tlv(&mut self.serial, CtlToMgmt::GetStackInfo, &[]).await;

        loop {
            let tlv = read_tlv_mgmt(&mut self.serial)
                .await
                .ok_or_else(|| JsValue::from_str("No response"))?;
            match tlv.tlv_type {
                MgmtToCtl::FromUi | MgmtToCtl::FromNet => continue,
                MgmtToCtl::StackInfo if tlv.value.len() >= 16 => {
                    let stack_base = u32::from_le_bytes([tlv.value[0], tlv.value[1], tlv.value[2], tlv.value[3]]);
                    let stack_top = u32::from_le_bytes([tlv.value[4], tlv.value[5], tlv.value[6], tlv.value[7]]);
                    let stack_size = u32::from_le_bytes([tlv.value[8], tlv.value[9], tlv.value[10], tlv.value[11]]);
                    let stack_used = u32::from_le_bytes([tlv.value[12], tlv.value[13], tlv.value[14], tlv.value[15]]);
                    let stack_free = stack_size.saturating_sub(stack_used);
                    let usage_percent = if stack_size > 0 {
                        (stack_used as f64 / stack_size as f64) * 100.0
                    } else {
                        0.0
                    };

                    let obj = js_sys::Object::new();
                    js_sys::Reflect::set(&obj, &"stackBase".into(), &stack_base.into())?;
                    js_sys::Reflect::set(&obj, &"stackTop".into(), &stack_top.into())?;
                    js_sys::Reflect::set(&obj, &"stackSize".into(), &stack_size.into())?;
                    js_sys::Reflect::set(&obj, &"stackUsed".into(), &stack_used.into())?;
                    js_sys::Reflect::set(&obj, &"stackFree".into(), &stack_free.into())?;
                    js_sys::Reflect::set(&obj, &"usagePercent".into(), &usage_percent.into())?;
                    return Ok(obj.into());
                }
                _ => return Err(JsValue::from_str("Unexpected response")),
            }
        }
    }

    /// Repaint MGMT chip stack (for fresh high-water mark measurement).
    #[wasm_bindgen]
    pub async fn repaint_mgmt_stack(&mut self) -> Result<(), JsValue> {
        let _ = write_tlv(&mut self.serial, CtlToMgmt::RepaintStack, &[]).await;

        loop {
            let tlv = read_tlv_mgmt(&mut self.serial)
                .await
                .ok_or_else(|| JsValue::from_str("No response"))?;
            match tlv.tlv_type {
                MgmtToCtl::FromUi | MgmtToCtl::FromNet => continue,
                MgmtToCtl::Ack => return Ok(()),
                _ => return Err(JsValue::from_str("Unexpected response")),
            }
        }
    }

    /// Get UI chip stack usage information.
    #[wasm_bindgen]
    pub async fn get_ui_stack_info(&mut self) -> Result<JsValue, JsValue> {
        let _ = write_tlv_ui(&mut self.serial, MgmtToUi::GetStackInfo, &[]).await;

        let tlv = read_tlv_ui(&mut self.serial)
            .await
            .ok_or_else(|| JsValue::from_str("No response"))?;

        if tlv.tlv_type != UiToMgmt::StackInfo || tlv.value.len() < 16 {
            return Err(JsValue::from_str("Invalid response"));
        }

        let stack_base = u32::from_le_bytes([tlv.value[0], tlv.value[1], tlv.value[2], tlv.value[3]]);
        let stack_top = u32::from_le_bytes([tlv.value[4], tlv.value[5], tlv.value[6], tlv.value[7]]);
        let stack_size = u32::from_le_bytes([tlv.value[8], tlv.value[9], tlv.value[10], tlv.value[11]]);
        let stack_used = u32::from_le_bytes([tlv.value[12], tlv.value[13], tlv.value[14], tlv.value[15]]);
        let stack_free = stack_size.saturating_sub(stack_used);
        let usage_percent = if stack_size > 0 {
            (stack_used as f64 / stack_size as f64) * 100.0
        } else {
            0.0
        };

        let obj = js_sys::Object::new();
        js_sys::Reflect::set(&obj, &"stackBase".into(), &stack_base.into())?;
        js_sys::Reflect::set(&obj, &"stackTop".into(), &stack_top.into())?;
        js_sys::Reflect::set(&obj, &"stackSize".into(), &stack_size.into())?;
        js_sys::Reflect::set(&obj, &"stackUsed".into(), &stack_used.into())?;
        js_sys::Reflect::set(&obj, &"stackFree".into(), &stack_free.into())?;
        js_sys::Reflect::set(&obj, &"usagePercent".into(), &usage_percent.into())?;
        Ok(obj.into())
    }

    /// Repaint UI chip stack (for fresh high-water mark measurement).
    #[wasm_bindgen]
    pub async fn repaint_ui_stack(&mut self) -> Result<(), JsValue> {
        let _ = write_tlv_ui(&mut self.serial, MgmtToUi::RepaintStack, &[]).await;

        let tlv = read_tlv_ui(&mut self.serial)
            .await
            .ok_or_else(|| JsValue::from_str("No response"))?;

        if tlv.tlv_type != UiToMgmt::Ack {
            return Err(JsValue::from_str("Expected Ack"));
        }
        Ok(())
    }

    // ==================== WS TESTS ====================

    /// Run WebSocket echo test.
    /// Returns JSON with test results.
    #[wasm_bindgen]
    pub async fn ws_echo_test(&mut self) -> Result<JsValue, JsValue> {
        let _ = write_tlv(&mut self.serial, CtlToMgmt::WsEchoTest, &[]).await;

        loop {
            let tlv = read_tlv_mgmt(&mut self.serial)
                .await
                .ok_or_else(|| JsValue::from_str("No response"))?;
            match tlv.tlv_type {
                MgmtToCtl::FromUi | MgmtToCtl::FromNet => continue,
                MgmtToCtl::WsEchoTestResult => {
                    // Parse results from NET chip format
                    let obj = js_sys::Object::new();
                    js_sys::Reflect::set(&obj, &"raw".into(), &hex::encode(&tlv.value).into())?;
                    return Ok(obj.into());
                }
                _ => return Err(JsValue::from_str("Unexpected response")),
            }
        }
    }

    /// Run WebSocket speed test.
    /// Returns JSON with test results.
    #[wasm_bindgen]
    pub async fn ws_speed_test(&mut self) -> Result<JsValue, JsValue> {
        let _ = write_tlv(&mut self.serial, CtlToMgmt::WsSpeedTest, &[]).await;

        loop {
            let tlv = read_tlv_mgmt(&mut self.serial)
                .await
                .ok_or_else(|| JsValue::from_str("No response"))?;
            match tlv.tlv_type {
                MgmtToCtl::FromUi | MgmtToCtl::FromNet => continue,
                MgmtToCtl::WsSpeedTestResult => {
                    let obj = js_sys::Object::new();
                    js_sys::Reflect::set(&obj, &"raw".into(), &hex::encode(&tlv.value).into())?;
                    return Ok(obj.into());
                }
                _ => return Err(JsValue::from_str("Unexpected response")),
            }
        }
    }

    // ==================== CHAT ====================

    /// Send a chat message through the NET chip.
    #[wasm_bindgen]
    pub async fn send_chat_message(&mut self, message: &str) -> Result<(), JsValue> {
        let _ = write_tlv_net(&mut self.serial, MgmtToNet::SendChatMessage, message.as_bytes()).await;

        let tlv = read_tlv_net(&mut self.serial)
            .await
            .ok_or_else(|| JsValue::from_str("No response"))?;

        if tlv.tlv_type != NetToMgmt::ChatMessageSent && tlv.tlv_type != NetToMgmt::Ack {
            return Err(JsValue::from_str("Expected ChatMessageSent or Ack"));
        }
        Ok(())
    }

    // ==================== CIRCULAR PING ====================

    /// Send a circular ping starting from UI (UI → NET → MGMT → CTL).
    #[wasm_bindgen]
    pub async fn circular_ping_via_ui(&mut self, data: Vec<u8>) -> Result<(), JsValue> {
        let _ = write_tlv_ui(&mut self.serial, MgmtToUi::CircularPing, &data).await;

        let tlv = read_tlv_net(&mut self.serial)
            .await
            .ok_or_else(|| JsValue::from_str("No response"))?;

        if tlv.tlv_type != NetToMgmt::CircularPing || tlv.value.as_slice() != data.as_slice() {
            return Err(JsValue::from_str("Circular ping failed"));
        }
        Ok(())
    }

    /// Send a circular ping starting from NET (NET → UI → MGMT → CTL).
    #[wasm_bindgen]
    pub async fn circular_ping_via_net(&mut self, data: Vec<u8>) -> Result<(), JsValue> {
        let _ = write_tlv_net(&mut self.serial, MgmtToNet::CircularPing, &data).await;

        let tlv = read_tlv_ui(&mut self.serial)
            .await
            .ok_or_else(|| JsValue::from_str("No response"))?;

        if tlv.tlv_type != UiToMgmt::CircularPing || tlv.value.as_slice() != data.as_slice() {
            return Err(JsValue::from_str("Circular ping failed"));
        }
        Ok(())
    }

    // ==================== JITTER STATS ====================

    /// Get jitter buffer statistics for a channel.
    /// Returns JSON with {received, output, underruns, overruns, level, state}.
    #[wasm_bindgen]
    pub async fn get_jitter_stats(&mut self, channel_id: u8) -> Result<JsValue, JsValue> {
        let _ = write_tlv_net(&mut self.serial, MgmtToNet::GetJitterStats, &[channel_id]).await;

        let tlv = read_tlv_net(&mut self.serial)
            .await
            .ok_or_else(|| JsValue::from_str("No response"))?;

        if tlv.tlv_type != NetToMgmt::JitterStats || tlv.value.len() < 19 {
            return Err(JsValue::from_str("Invalid response"));
        }

        // Parse: received u32, output u32, underruns u32, overruns u32, level u16, state u8
        let received = u32::from_le_bytes([tlv.value[0], tlv.value[1], tlv.value[2], tlv.value[3]]);
        let output = u32::from_le_bytes([tlv.value[4], tlv.value[5], tlv.value[6], tlv.value[7]]);
        let underruns = u32::from_le_bytes([tlv.value[8], tlv.value[9], tlv.value[10], tlv.value[11]]);
        let overruns = u32::from_le_bytes([tlv.value[12], tlv.value[13], tlv.value[14], tlv.value[15]]);
        let level = u16::from_le_bytes([tlv.value[16], tlv.value[17]]);
        let state = tlv.value[18];

        let obj = js_sys::Object::new();
        js_sys::Reflect::set(&obj, &"received".into(), &received.into())?;
        js_sys::Reflect::set(&obj, &"output".into(), &output.into())?;
        js_sys::Reflect::set(&obj, &"underruns".into(), &underruns.into())?;
        js_sys::Reflect::set(&obj, &"overruns".into(), &overruns.into())?;
        js_sys::Reflect::set(&obj, &"level".into(), &level.into())?;
        js_sys::Reflect::set(&obj, &"state".into(), &(if state == 0 { "buffering" } else { "playing" }).into())?;
        Ok(obj.into())
    }

    // ==================== STATE AGGREGATION ====================

    /// Get all state variables from all chips.
    /// Returns a JSON object with all current device state.
    #[wasm_bindgen]
    pub async fn get_all_state(&mut self) -> Result<JsValue, JsValue> {
        let obj = js_sys::Object::new();

        // UI chip state
        let ui_obj = js_sys::Object::new();
        if let Ok(version) = self.get_version().await {
            js_sys::Reflect::set(&ui_obj, &"version".into(), &version.into())?;
        }
        if let Ok(sframe_key) = self.get_sframe_key().await {
            js_sys::Reflect::set(&ui_obj, &"sframeKey".into(), &sframe_key.into())?;
        }
        if let Ok(loopback_mode) = self.get_ui_loopback_mode().await {
            let is_loopback = loopback_mode != "off";
            js_sys::Reflect::set(&ui_obj, &"loopbackMode".into(), &loopback_mode.into())?;
            js_sys::Reflect::set(&ui_obj, &"loopback".into(), &is_loopback.into())?;
        }
        js_sys::Reflect::set(&obj, &"ui".into(), &ui_obj)?;

        // NET chip state
        let net_obj = js_sys::Object::new();
        if let Ok(relay_url) = self.get_relay_url().await {
            js_sys::Reflect::set(&net_obj, &"relayUrl".into(), &relay_url.into())?;
        }
        if let Ok(networks) = self.get_wifi_networks().await {
            js_sys::Reflect::set(&net_obj, &"wifiNetworks".into(), &networks)?;
        }
        if let Ok(loopback_mode) = self.get_net_loopback_mode().await {
            let is_loopback = loopback_mode != "off";
            js_sys::Reflect::set(&net_obj, &"loopbackMode".into(), &loopback_mode.into())?;
            js_sys::Reflect::set(&net_obj, &"loopback".into(), &is_loopback.into())?;
        }
        js_sys::Reflect::set(&obj, &"net".into(), &net_obj)?;

        Ok(obj.into())
    }
}

impl Default for LinkController {
    fn default() -> Self {
        Self::new()
    }
}

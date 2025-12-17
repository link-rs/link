//! web-link: Virtual Link device running in WebAssembly.
//!
//! This crate provides a browser-based simulation of the Link device,
//! with all three chips (MGMT, UI, NET) running as async tasks.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::Arc;

use async_channel::{Receiver, Sender};
use futures::future::select;
use futures::pin_mut;
use js_sys::Function;
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::spawn_local;
use web_sys::console;

use link::shared::led::Color;
use link::shared::protocol::*;

mod channel_io;

/// Log a message to the browser console.
macro_rules! log {
    ($($t:tt)*) => {
        console::log_1(&format!($($t)*).into())
    };
}

/// Color as a u8 for atomic access.
fn color_to_u8(color: Color) -> u8 {
    match color {
        Color::Black => 0,
        Color::Red => 1,
        Color::Green => 2,
        Color::Blue => 3,
        Color::Yellow => 4,
        Color::Cyan => 5,
        Color::Magenta => 6,
        Color::White => 7,
    }
}

fn u8_to_color(val: u8) -> Color {
    match val {
        0 => Color::Black,
        1 => Color::Red,
        2 => Color::Green,
        3 => Color::Blue,
        4 => Color::Yellow,
        5 => Color::Cyan,
        6 => Color::Magenta,
        _ => Color::White,
    }
}

/// Convert Color to a CSS color string.
fn color_to_css(color: Color) -> &'static str {
    match color {
        Color::Black => "#000000",
        Color::Red => "#ff0000",
        Color::Green => "#00ff00",
        Color::Blue => "#0000ff",
        Color::Yellow => "#ffff00",
        Color::Cyan => "#00ffff",
        Color::Magenta => "#ff00ff",
        Color::White => "#ffffff",
    }
}

/// Shared state for LED colors (atomic for thread-safe access).
struct LedState {
    mgmt_a: AtomicU8,
    mgmt_b: AtomicU8,
    ui: AtomicU8,
    net: AtomicU8,
}

impl LedState {
    fn new() -> Self {
        Self {
            mgmt_a: AtomicU8::new(0),
            mgmt_b: AtomicU8::new(0),
            ui: AtomicU8::new(0),
            net: AtomicU8::new(0),
        }
    }
}

/// Button event type.
#[derive(Clone, Copy, Debug)]
pub enum ButtonEvent {
    APressed,
    AReleased,
    BPressed,
    BReleased,
    MicPressed,
    MicReleased,
}

/// WebSocket command from NET chip to JS.
#[derive(Clone, Debug)]
pub enum WsCommand {
    Connect(String),
    Disconnect,
    Send(Vec<u8>),
}

/// WebSocket event from JS to NET chip.
#[derive(Clone, Debug)]
pub enum WsEvent {
    Connected,
    Disconnected,
    Received(Vec<u8>),
}

/// Audio frame from microphone (320 i16 samples = 640 bytes).
#[derive(Clone, Debug)]
pub struct AudioFrame(pub Vec<i16>);

/// Audio event for UI chip.
#[derive(Clone, Debug)]
pub enum AudioEvent {
    /// Microphone frame received from JS.
    MicFrame(AudioFrame),
}

/// Command to NET chip for configuration.
#[derive(Clone, Debug)]
pub enum NetCommand {
    SetRelayUrl(String),
}

/// The virtual Link device.
#[wasm_bindgen]
pub struct WebLink {
    led_state: Arc<LedState>,
    button_tx: Sender<ButtonEvent>,
    audio_tx: Sender<AudioEvent>,
    ws_event_tx: Sender<WsEvent>,
    net_cmd_tx: Sender<NetCommand>,
    // Callback for NET chip to request WebSocket operations from JS
    ws_command_callback: Rc<RefCell<Option<Function>>>,
    // Callback for UI chip to send audio frames to JS for playback
    audio_playback_callback: Rc<RefCell<Option<Function>>>,
}

#[wasm_bindgen]
impl WebLink {
    /// Create a new virtual Link device.
    #[wasm_bindgen(constructor)]
    pub fn new() -> Self {
        console_error_panic_hook::set_once();

        let led_state = Arc::new(LedState::new());
        let (button_tx, button_rx) = async_channel::unbounded();
        let (audio_tx, audio_rx) = async_channel::unbounded();
        let (ws_event_tx, ws_event_rx) = async_channel::unbounded();
        let (ws_cmd_tx, ws_cmd_rx) = async_channel::unbounded();
        let (net_cmd_tx, net_cmd_rx) = async_channel::unbounded();
        let (playback_tx, playback_rx) = async_channel::unbounded::<AudioFrame>();
        let ws_command_callback: Rc<RefCell<Option<Function>>> = Rc::new(RefCell::new(None));
        let audio_playback_callback: Rc<RefCell<Option<Function>>> = Rc::new(RefCell::new(None));

        // Create inter-chip channels
        // MGMT <-> UI
        let (mgmt_to_ui_tx, mgmt_to_ui_rx) = async_channel::bounded(16);
        let (ui_to_mgmt_tx, ui_to_mgmt_rx) = async_channel::bounded(16);
        // MGMT <-> NET
        let (mgmt_to_net_tx, mgmt_to_net_rx) = async_channel::bounded(16);
        let (net_to_mgmt_tx, net_to_mgmt_rx) = async_channel::bounded(16);
        // UI <-> NET (direct for audio)
        let (ui_to_net_tx, ui_to_net_rx) = async_channel::bounded(16);
        let (net_to_ui_tx, net_to_ui_rx) = async_channel::bounded(16);

        // Start MGMT chip task
        let mgmt_led_state = led_state.clone();
        spawn_local(async move {
            run_mgmt(
                mgmt_led_state,
                mgmt_to_ui_tx,
                ui_to_mgmt_rx,
                mgmt_to_net_tx,
                net_to_mgmt_rx,
            )
            .await;
        });

        // Start UI chip task
        let ui_led_state = led_state.clone();
        spawn_local(async move {
            run_ui(
                ui_led_state,
                button_rx,
                audio_rx,
                playback_tx,
                ui_to_mgmt_tx,
                mgmt_to_ui_rx,
                ui_to_net_tx,
                net_to_ui_rx,
            )
            .await;
        });

        // Start NET chip task
        let net_led_state = led_state.clone();
        spawn_local(async move {
            run_net(
                net_led_state,
                net_to_mgmt_tx,
                mgmt_to_net_rx,
                net_to_ui_tx,
                ui_to_net_rx,
                ws_event_rx,
                ws_cmd_tx,
                net_cmd_rx,
            )
            .await;
        });

        // Start WebSocket command handler (forwards commands from NET to JS)
        let ws_cb = ws_command_callback.clone();
        spawn_local(async move {
            loop {
                let cmd = ws_cmd_rx.recv().await;
                if let Ok(cmd) = cmd {
                    if let Some(ref callback) = *ws_cb.borrow() {
                        let (cmd_type, arg) = match cmd {
                            WsCommand::Connect(url) => ("connect", url),
                            WsCommand::Disconnect => ("disconnect", String::new()),
                            WsCommand::Send(data) => {
                                // Encode as hex for simplicity
                                ("send", hex::encode(&data))
                            }
                        };
                        let _ = callback.call2(
                            &JsValue::NULL,
                            &JsValue::from_str(cmd_type),
                            &JsValue::from_str(&arg),
                        );
                    }
                }
            }
        });

        // Start audio playback handler (forwards frames from UI to JS)
        let audio_cb = audio_playback_callback.clone();
        spawn_local(async move {
            loop {
                let frame = playback_rx.recv().await;
                if let Ok(frame) = frame {
                    if let Some(ref callback) = *audio_cb.borrow() {
                        // Convert i16 samples to JS array
                        let arr = js_sys::Int16Array::new_with_length(frame.0.len() as u32);
                        for (i, &sample) in frame.0.iter().enumerate() {
                            arr.set_index(i as u32, sample);
                        }
                        let _ = callback.call1(&JsValue::NULL, &arr);
                    }
                }
            }
        });

        log!("WebLink device initialized");

        Self {
            led_state,
            button_tx,
            audio_tx,
            ws_event_tx,
            net_cmd_tx,
            ws_command_callback,
            audio_playback_callback,
        }
    }

    /// Get the current color of MGMT LED A as a CSS color string.
    #[wasm_bindgen(js_name = getMgmtLedA)]
    pub fn get_mgmt_led_a(&self) -> String {
        let color = u8_to_color(self.led_state.mgmt_a.load(Ordering::Relaxed));
        color_to_css(color).to_string()
    }

    /// Get the current color of MGMT LED B as a CSS color string.
    #[wasm_bindgen(js_name = getMgmtLedB)]
    pub fn get_mgmt_led_b(&self) -> String {
        let color = u8_to_color(self.led_state.mgmt_b.load(Ordering::Relaxed));
        color_to_css(color).to_string()
    }

    /// Get the current color of UI LED as a CSS color string.
    #[wasm_bindgen(js_name = getUiLed)]
    pub fn get_ui_led(&self) -> String {
        let color = u8_to_color(self.led_state.ui.load(Ordering::Relaxed));
        color_to_css(color).to_string()
    }

    /// Get the current color of NET LED as a CSS color string.
    #[wasm_bindgen(js_name = getNetLed)]
    pub fn get_net_led(&self) -> String {
        let color = u8_to_color(self.led_state.net.load(Ordering::Relaxed));
        color_to_css(color).to_string()
    }

    /// Press button A.
    #[wasm_bindgen(js_name = pressA)]
    pub fn press_a(&self) {
        let _ = self.button_tx.try_send(ButtonEvent::APressed);
    }

    /// Release button A.
    #[wasm_bindgen(js_name = releaseA)]
    pub fn release_a(&self) {
        let _ = self.button_tx.try_send(ButtonEvent::AReleased);
    }

    /// Press button B.
    #[wasm_bindgen(js_name = pressB)]
    pub fn press_b(&self) {
        let _ = self.button_tx.try_send(ButtonEvent::BPressed);
    }

    /// Release button B.
    #[wasm_bindgen(js_name = releaseB)]
    pub fn release_b(&self) {
        let _ = self.button_tx.try_send(ButtonEvent::BReleased);
    }

    /// Press Mic button.
    #[wasm_bindgen(js_name = pressMic)]
    pub fn press_mic(&self) {
        let _ = self.button_tx.try_send(ButtonEvent::MicPressed);
    }

    /// Release Mic button.
    #[wasm_bindgen(js_name = releaseMic)]
    pub fn release_mic(&self) {
        let _ = self.button_tx.try_send(ButtonEvent::MicReleased);
    }

    /// Set callback for WebSocket commands from NET chip.
    /// Callback signature: (command: string, arg: string) => void
    /// Commands: "connect" (arg=url), "disconnect" (arg=""), "send" (arg=hex data)
    #[wasm_bindgen(js_name = onWsCommand)]
    pub fn on_ws_command(&self, callback: Function) {
        *self.ws_command_callback.borrow_mut() = Some(callback);
    }

    /// Notify NET chip that WebSocket connected.
    #[wasm_bindgen(js_name = wsConnected)]
    pub fn ws_connected(&self) {
        let _ = self.ws_event_tx.try_send(WsEvent::Connected);
    }

    /// Notify NET chip that WebSocket disconnected.
    #[wasm_bindgen(js_name = wsDisconnected)]
    pub fn ws_disconnected(&self) {
        let _ = self.ws_event_tx.try_send(WsEvent::Disconnected);
    }

    /// Deliver received WebSocket data to NET chip (hex encoded).
    #[wasm_bindgen(js_name = wsReceived)]
    pub fn ws_received(&self, hex_data: &str) {
        if let Ok(data) = hex::decode(hex_data) {
            let _ = self.ws_event_tx.try_send(WsEvent::Received(data));
        }
    }

    /// Set the relay URL (triggers NET chip to connect).
    #[wasm_bindgen(js_name = setRelayUrl)]
    pub fn set_relay_url(&self, url: &str) {
        if url.is_empty() {
            log!("Clearing relay URL");
        } else {
            log!("Setting relay URL: {}", url);
        }
        let _ = self.net_cmd_tx.try_send(NetCommand::SetRelayUrl(url.to_string()));
    }

    /// Set callback for audio playback frames from UI chip.
    /// Callback signature: (samples: Int16Array) => void
    /// Called with 320 samples (40ms @ 8kHz) to play.
    #[wasm_bindgen(js_name = onAudioPlayback)]
    pub fn on_audio_playback(&self, callback: Function) {
        *self.audio_playback_callback.borrow_mut() = Some(callback);
    }

    /// Deliver microphone audio frame to UI chip.
    /// samples: Int16Array with 320 samples (40ms @ 8kHz)
    #[wasm_bindgen(js_name = audioCapture)]
    pub fn audio_capture(&self, samples: &js_sys::Int16Array) {
        let len = samples.length() as usize;
        let mut data = vec![0i16; len];
        samples.copy_to(&mut data);
        let _ = self.audio_tx.try_send(AudioEvent::MicFrame(AudioFrame(data)));
    }
}

impl Default for WebLink {
    fn default() -> Self {
        Self::new()
    }
}

/// Run the MGMT chip simulation.
async fn run_mgmt(
    led_state: Arc<LedState>,
    to_ui: Sender<Vec<u8>>,
    from_ui: Receiver<Vec<u8>>,
    to_net: Sender<Vec<u8>>,
    from_net: Receiver<Vec<u8>>,
) {
    log!("MGMT chip started");

    // Set initial LED colors
    led_state.mgmt_a.store(color_to_u8(Color::Blue), Ordering::Relaxed);
    led_state.mgmt_b.store(color_to_u8(Color::Green), Ordering::Relaxed);

    loop {
        // Wait for messages from UI or NET
        let ui_fut = from_ui.recv();
        let net_fut = from_net.recv();

        pin_mut!(ui_fut);
        pin_mut!(net_fut);

        match select(ui_fut, net_fut).await {
            futures::future::Either::Left((Ok(msg), _)) => {
                log!("MGMT received from UI: {} bytes", msg.len());
                // Route to NET if needed
                let _ = to_net.send(msg).await;
            }
            futures::future::Either::Right((Ok(msg), _)) => {
                log!("MGMT received from NET: {} bytes", msg.len());
                // Route to UI if needed
                let _ = to_ui.send(msg).await;
            }
            _ => {
                // Channel closed, exit
                break;
            }
        }
    }
}

/// Which button is currently active for audio capture.
#[derive(Clone, Copy, Debug, PartialEq)]
enum ActiveButton {
    None,
    A,
    B,
    Mic,
}

/// Run the UI chip simulation.
async fn run_ui(
    led_state: Arc<LedState>,
    button_rx: Receiver<ButtonEvent>,
    audio_rx: Receiver<AudioEvent>,
    playback_tx: Sender<AudioFrame>,
    _to_mgmt: Sender<Vec<u8>>,
    from_mgmt: Receiver<Vec<u8>>,
    to_net: Sender<Vec<u8>>,
    from_net: Receiver<Vec<u8>>,
) {
    log!("UI chip started");

    // Set initial LED color
    led_state.ui.store(color_to_u8(Color::Cyan), Ordering::Relaxed);

    // Track which button is currently held
    let mut active_button = ActiveButton::None;

    loop {
        let button_fut = button_rx.recv();
        let audio_fut = audio_rx.recv();
        let mgmt_fut = from_mgmt.recv();
        let net_fut = from_net.recv();

        pin_mut!(button_fut);
        pin_mut!(audio_fut);
        pin_mut!(mgmt_fut);
        pin_mut!(net_fut);

        // Select between all event sources
        let button_or_audio = select(button_fut, audio_fut);
        pin_mut!(button_or_audio);
        let mgmt_or_net = select(mgmt_fut, net_fut);
        pin_mut!(mgmt_or_net);

        match select(button_or_audio, mgmt_or_net).await {
            // Button event
            futures::future::Either::Left((futures::future::Either::Left((Ok(event), _)), _)) => {
                match event {
                    ButtonEvent::APressed => {
                        if active_button == ActiveButton::None {
                            active_button = ActiveButton::A;
                            led_state.ui.store(color_to_u8(Color::Red), Ordering::Relaxed);
                            log!("UI: Button A pressed - recording");
                        }
                    }
                    ButtonEvent::AReleased => {
                        if active_button == ActiveButton::A {
                            active_button = ActiveButton::None;
                            led_state.ui.store(color_to_u8(Color::Cyan), Ordering::Relaxed);
                            log!("UI: Button A released");
                        }
                    }
                    ButtonEvent::BPressed => {
                        if active_button == ActiveButton::None {
                            active_button = ActiveButton::B;
                            led_state.ui.store(color_to_u8(Color::Green), Ordering::Relaxed);
                            log!("UI: Button B pressed - recording");
                        }
                    }
                    ButtonEvent::BReleased => {
                        if active_button == ActiveButton::B {
                            active_button = ActiveButton::None;
                            led_state.ui.store(color_to_u8(Color::Cyan), Ordering::Relaxed);
                            log!("UI: Button B released");
                        }
                    }
                    ButtonEvent::MicPressed => {
                        if active_button == ActiveButton::None {
                            active_button = ActiveButton::Mic;
                            led_state.ui.store(color_to_u8(Color::Yellow), Ordering::Relaxed);
                            log!("UI: Mic button pressed - recording");
                        }
                    }
                    ButtonEvent::MicReleased => {
                        if active_button == ActiveButton::Mic {
                            active_button = ActiveButton::None;
                            led_state.ui.store(color_to_u8(Color::Cyan), Ordering::Relaxed);
                            log!("UI: Mic button released");
                        }
                    }
                }
            }
            // Audio capture event from microphone
            futures::future::Either::Left((futures::future::Either::Right((Ok(AudioEvent::MicFrame(frame)), _)), _)) => {
                // Only forward audio if a button is held
                match active_button {
                    ActiveButton::A | ActiveButton::Mic => {
                        // Convert i16 samples to bytes (little-endian) for transmission
                        let mut bytes = Vec::with_capacity(frame.0.len() * 2 + 1);
                        bytes.push(UiToNet::AudioFrameA as u8);
                        for sample in &frame.0 {
                            bytes.extend_from_slice(&sample.to_le_bytes());
                        }
                        let _ = to_net.send(bytes).await;
                    }
                    ActiveButton::B => {
                        let mut bytes = Vec::with_capacity(frame.0.len() * 2 + 1);
                        bytes.push(UiToNet::AudioFrameB as u8);
                        for sample in &frame.0 {
                            bytes.extend_from_slice(&sample.to_le_bytes());
                        }
                        let _ = to_net.send(bytes).await;
                    }
                    ActiveButton::None => {
                        // No button held, discard audio
                    }
                }
            }
            // Message from MGMT
            futures::future::Either::Right((futures::future::Either::Left((Ok(msg), _)), _)) => {
                log!("UI received from MGMT: {} bytes", msg.len());
            }
            // Audio from NET (playback)
            futures::future::Either::Right((futures::future::Either::Right((Ok(msg), _)), _)) => {
                if msg.is_empty() {
                    continue;
                }

                // Skip protocol prefix byte if present
                // Note: WebSocket echo returns UiToNet prefixes, not NetToUi
                let audio_data = if msg[0] == UiToNet::AudioFrameA as u8
                    || msg[0] == UiToNet::AudioFrameB as u8
                    || msg[0] == NetToUi::AudioFrame as u8
                {
                    &msg[1..]
                } else {
                    &msg[..]
                };

                // Convert bytes back to i16 samples
                if audio_data.len() >= 2 {
                    let mut samples = Vec::with_capacity(audio_data.len() / 2);
                    for chunk in audio_data.chunks_exact(2) {
                        samples.push(i16::from_le_bytes([chunk[0], chunk[1]]));
                    }
                    // Send to JS for playback
                    let _ = playback_tx.send(AudioFrame(samples)).await;
                    // Brief visual feedback
                    led_state.ui.store(color_to_u8(Color::White), Ordering::Relaxed);
                }
            }
            _ => {
                break;
            }
        }
    }
}

/// Run the NET chip simulation.
async fn run_net(
    led_state: Arc<LedState>,
    _to_mgmt: Sender<Vec<u8>>,
    from_mgmt: Receiver<Vec<u8>>,
    to_ui: Sender<Vec<u8>>,
    from_ui: Receiver<Vec<u8>>,
    ws_event_rx: Receiver<WsEvent>,
    ws_cmd_tx: Sender<WsCommand>,
    net_cmd_rx: Receiver<NetCommand>,
) {
    log!("NET chip started");

    // Set initial LED color (no relay configured = blue)
    led_state.net.store(color_to_u8(Color::Blue), Ordering::Relaxed);

    let mut relay_url: Option<String> = None;
    let mut ws_connected = false;

    loop {
        let mgmt_fut = from_mgmt.recv();
        let ui_fut = from_ui.recv();
        let ws_fut = ws_event_rx.recv();
        let cmd_fut = net_cmd_rx.recv();

        pin_mut!(mgmt_fut);
        pin_mut!(ui_fut);
        pin_mut!(ws_fut);
        pin_mut!(cmd_fut);

        let mgmt_or_ui = select(mgmt_fut, ui_fut);
        pin_mut!(mgmt_or_ui);
        let ws_or_cmd = select(ws_fut, cmd_fut);
        pin_mut!(ws_or_cmd);

        match select(mgmt_or_ui, ws_or_cmd).await {
            futures::future::Either::Left((futures::future::Either::Left((Ok(msg), _)), _)) => {
                log!("NET received from MGMT: {} bytes", msg.len());
            }
            futures::future::Either::Left((futures::future::Either::Right((Ok(msg), _)), _)) => {
                // Forward audio to WebSocket if connected
                if ws_connected && !msg.is_empty() {
                    let _ = ws_cmd_tx.send(WsCommand::Send(msg)).await;
                }
            }
            // WebSocket event
            futures::future::Either::Right((futures::future::Either::Left((Ok(event), _)), _)) => {
                match event {
                    WsEvent::Connected => {
                        log!("NET: WebSocket connected");
                        ws_connected = true;
                        led_state.net.store(color_to_u8(Color::Green), Ordering::Relaxed);
                    }
                    WsEvent::Disconnected => {
                        log!("NET: WebSocket disconnected");
                        ws_connected = false;
                        // Show red if we have a URL but disconnected, blue if no URL
                        if relay_url.is_some() {
                            led_state.net.store(color_to_u8(Color::Red), Ordering::Relaxed);
                        } else {
                            led_state.net.store(color_to_u8(Color::Blue), Ordering::Relaxed);
                        }
                    }
                    WsEvent::Received(data) => {
                        log!("NET: WebSocket received {} bytes", data.len());
                        // Forward to UI for playback
                        let _ = to_ui.send(data).await;
                    }
                }
            }
            // NET command (relay URL configuration)
            futures::future::Either::Right((futures::future::Either::Right((Ok(cmd), _)), _)) => {
                match cmd {
                    NetCommand::SetRelayUrl(url) => {
                        if url.is_empty() {
                            log!("NET: Clearing relay URL");
                            relay_url = None;
                            // Disconnect if connected
                            if ws_connected {
                                let _ = ws_cmd_tx.send(WsCommand::Disconnect).await;
                            }
                            led_state.net.store(color_to_u8(Color::Blue), Ordering::Relaxed);
                        } else {
                            log!("NET: Setting relay URL to {}", url);
                            relay_url = Some(url.clone());
                            // Connect to new URL
                            led_state.net.store(color_to_u8(Color::Yellow), Ordering::Relaxed);
                            let _ = ws_cmd_tx.send(WsCommand::Connect(url)).await;
                        }
                    }
                }
            }
            _ => {
                break;
            }
        }
    }
}

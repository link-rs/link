//! Integration tests for the multi-chip system.
//!
//! These tests require all features (ctl, mgmt, net, ui) to be enabled.
//! They test the full communication stack between CTL (using async CtlCore) and
//! firmware modules (async, embedded_io_async).

#![cfg(all(feature = "ctl", feature = "mgmt", feature = "net", feature = "ui"))]

use crate::ctl::CtlCore;
use crate::shared::NoOpStackMonitor;
use crate::shared::mocks::{
    GpioOp, MockAsyncDelay, MockAudioStream, MockButton, MockCtlPort, MockDelay, MockFlash,
    MockPin, TrackingPin, async_async_channel, ctl_async_channel, mock_i2c_with_eeprom,
    mock_led_pins,
};
use crate::{NetLoopbackMode, PinValue, UiLoopbackMode, mgmt, net, ui};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Channel;
use std::sync::{Arc, Mutex};

/// Run a test with the full device stack.
///
/// The test function receives a `CtlCore<MockCtlPort>` and runs async operations on it.
/// All tasks (CTL and firmware) run concurrently in async context.
async fn device_test<F, Fut>(test_fn: F)
where
    F: FnOnce(CtlCore<MockCtlPort>) -> Fut,
    Fut: std::future::Future<Output = ()>,
{
    // CTL <-> MGMT: both async
    let (ctl_port, (mgmt_from_ctl, mgmt_to_ctl)) = ctl_async_channel();

    // MGMT <-> UI: both async
    let ((mgmt_from_ui, mgmt_to_ui), (ui_from_mgmt, ui_to_mgmt)) = async_async_channel();

    // MGMT <-> NET: both async
    let ((mgmt_from_net, mgmt_to_net), (net_from_mgmt, net_to_mgmt)) = async_async_channel();

    // UI <-> NET: both async
    let ((ui_from_net, ui_to_net), (net_from_ui, net_to_ui)) = async_async_channel();

    let ctl_core = CtlCore::new(ctl_port);
    let ui_reset_pins = mgmt::UiResetPins::new(MockPin::new(), MockPin::new(), MockPin::new());
    let net_reset_pins = mgmt::NetResetPins::new(MockPin::new(), MockPin::new());

    let mgmt_task = mgmt::run(
        mgmt_to_ctl,
        mgmt_from_ctl,
        mgmt_to_ui,
        mgmt_from_ui,
        mgmt_to_net,
        mgmt_from_net,
        mock_led_pins(),
        mock_led_pins(),
        ui_reset_pins,
        net_reset_pins,
        MockAsyncDelay,
        NoOpStackMonitor,
    );

    // Create WS channels for NET app
    let ws_cmd_channel: Channel<CriticalSectionRawMutex, net::WsCommand, 4> = Channel::new();
    let ws_event_channel: Channel<CriticalSectionRawMutex, net::WsEvent, 4> = Channel::new();

    // Run the CTL test as an async task
    let ctl_task = test_fn(ctl_core);

    tokio::select! {
        _ = ctl_task => {
            // Test completed
        }
        _ = mgmt_task => {}
        _ = ui::run(
            ui_to_mgmt,
            ui_from_mgmt,
            ui_to_net,
            ui_from_net,
            mock_led_pins(),
            MockButton,
            MockButton,
            MockButton,
            mock_i2c_with_eeprom(),
            MockDelay,
            MockAudioStream::new(),
            NoOpStackMonitor,
        ) => {}
        _ = net::run(
            net_to_mgmt,
            net_from_mgmt,
            net_to_ui,
            net_from_ui,
            mock_led_pins(),
            MockFlash::new(),
            0,
            ws_cmd_channel.sender(),
            ws_event_channel.receiver(),
        ) => {}
    }
}

/// Test harness that provides access to tracked GPIO operations.
async fn device_test_with_gpio_tracking<F, Fut>(test_fn: F)
where
    F: FnOnce(CtlCore<MockCtlPort>, Arc<Mutex<Vec<(&'static str, GpioOp)>>>) -> Fut,
    Fut: std::future::Future<Output = ()>,
{
    // CTL <-> MGMT: both async
    let (ctl_port, (mgmt_from_ctl, mgmt_to_ctl)) = ctl_async_channel();

    // MGMT <-> UI: both async
    let ((mgmt_from_ui, mgmt_to_ui), (ui_from_mgmt, ui_to_mgmt)) = async_async_channel();

    // MGMT <-> NET: both async
    let ((mgmt_from_net, mgmt_to_net), (net_from_mgmt, net_to_mgmt)) = async_async_channel();

    // UI <-> NET: both async
    let ((ui_from_net, ui_to_net), (net_from_ui, net_to_ui)) = async_async_channel();

    let ctl_core = CtlCore::new(ctl_port);

    // Create tracking pins for UI and NET reset
    let gpio_ops: Arc<Mutex<Vec<(&'static str, GpioOp)>>> = Arc::new(Mutex::new(Vec::new()));
    let ui_boot0_pin = TrackingPin::new("UI_BOOT0", gpio_ops.clone());
    let ui_boot1_pin = TrackingPin::new("UI_BOOT1", gpio_ops.clone());
    let ui_rst_pin = TrackingPin::new("UI_RST", gpio_ops.clone());
    let ui_reset_pins = mgmt::UiResetPins::new(ui_boot0_pin, ui_boot1_pin, ui_rst_pin);

    let net_boot_pin = TrackingPin::new("NET_BOOT", gpio_ops.clone());
    let net_rst_pin = TrackingPin::new("NET_RST", gpio_ops.clone());
    let net_reset_pins = mgmt::NetResetPins::new(net_boot_pin, net_rst_pin);

    let mgmt_task = mgmt::run(
        mgmt_to_ctl,
        mgmt_from_ctl,
        mgmt_to_ui,
        mgmt_from_ui,
        mgmt_to_net,
        mgmt_from_net,
        mock_led_pins(),
        mock_led_pins(),
        ui_reset_pins,
        net_reset_pins,
        MockAsyncDelay,
        NoOpStackMonitor,
    );

    // Create WS channels for NET app
    let ws_cmd_channel: Channel<CriticalSectionRawMutex, net::WsCommand, 4> = Channel::new();
    let ws_event_channel: Channel<CriticalSectionRawMutex, net::WsEvent, 4> = Channel::new();

    // Run the CTL test as an async task
    let ctl_task = test_fn(ctl_core, gpio_ops);

    tokio::select! {
        _ = ctl_task => {
            // Test completed
        }
        _ = mgmt_task => {}
        _ = ui::run(
            ui_to_mgmt,
            ui_from_mgmt,
            ui_to_net,
            ui_from_net,
            mock_led_pins(),
            MockButton,
            MockButton,
            MockButton,
            mock_i2c_with_eeprom(),
            MockDelay,
            MockAudioStream::new(),
            NoOpStackMonitor,
        ) => {}
        _ = net::run(
            net_to_mgmt,
            net_from_mgmt,
            net_to_ui,
            net_from_ui,
            mock_led_pins(),
            MockFlash::new(),
            0,
            ws_cmd_channel.sender(),
            ws_event_channel.receiver(),
        ) => {}
    }
}

#[tokio::test]
async fn ctl_mgmt_ping() {
    device_test(|mut ctl| async move {
        ctl.mgmt_ping(b"hello mgmt").await.unwrap();
    })
    .await;
}

#[tokio::test]
async fn ctl_mgmt_board_version() {
    device_test(|mut ctl| async move {
        assert_eq!(ctl.mgmt_get_board_version().await.unwrap(), 0xFF);
    })
    .await;
}

#[tokio::test]
async fn ctl_ui_ping() {
    device_test(|mut ctl| async move {
        ctl.ui_ping(b"hello ui").await.unwrap();
    })
    .await;
}

#[tokio::test]
async fn ctl_net_ping() {
    device_test(|mut ctl| async move {
        ctl.net_ping(b"hello net").await.unwrap();
    })
    .await;
}

#[tokio::test]
async fn ui_first_circular_ping() {
    device_test(|mut ctl| async move {
        ctl.ui_first_circular_ping(b"hello ui circular")
            .await
            .unwrap();
    })
    .await;
}

#[tokio::test]
async fn net_first_circular_ping() {
    device_test(|mut ctl| async move {
        ctl.net_first_circular_ping(b"hello net circular")
            .await
            .unwrap();
    })
    .await;
}

#[tokio::test]
async fn get_version_default() {
    device_test(|mut ctl| async move {
        let version = ctl.get_version().await.unwrap();
        assert_eq!(version, 0xffffffff);
    })
    .await;
}

#[tokio::test]
async fn set_and_get_version() {
    device_test(|mut ctl| async move {
        ctl.set_version(0x12345678).await.unwrap();
        let version = ctl.get_version().await.unwrap();
        assert_eq!(version, 0x12345678);
    })
    .await;
}

#[tokio::test]
async fn get_sframe_key_default() {
    device_test(|mut ctl| async move {
        let key = ctl.get_sframe_key().await.unwrap();
        assert_eq!(key, [0xff; 16]);
    })
    .await;
}

#[tokio::test]
async fn set_and_get_sframe_key() {
    device_test(|mut ctl| async move {
        let key = [
            0x5b, 0x9f, 0x37, 0xb1, 0x54, 0x6b, 0x61, 0xf9, 0x14, 0xda, 0x9f, 0x55, 0x7a, 0x8f,
            0xe2, 0x15,
        ];
        ctl.set_sframe_key(&key).await.unwrap();
        let result = ctl.get_sframe_key().await.unwrap();
        assert_eq!(result, key);
    })
    .await;
}

#[tokio::test]
async fn get_wifi_ssids_default() {
    device_test(|mut ctl| async move {
        let ssids = ctl.get_wifi_ssids().await.unwrap();
        assert!(ssids.is_empty());
    })
    .await;
}

#[tokio::test]
async fn add_and_get_wifi_ssid() {
    device_test(|mut ctl| async move {
        ctl.add_wifi_ssid("MyNetwork", "MyPassword").await.unwrap();
        let ssids = ctl.get_wifi_ssids().await.unwrap();
        assert_eq!(ssids.len(), 1);
        assert_eq!(ssids[0].ssid.as_str(), "MyNetwork");
        assert_eq!(ssids[0].password.as_str(), "MyPassword");
    })
    .await;
}

#[tokio::test]
async fn clear_wifi_ssids() {
    device_test(|mut ctl| async move {
        ctl.add_wifi_ssid("Network1", "Pass1").await.unwrap();
        ctl.add_wifi_ssid("Network2", "Pass2").await.unwrap();
        ctl.clear_wifi_ssids().await.unwrap();
        let ssids = ctl.get_wifi_ssids().await.unwrap();
        assert!(ssids.is_empty());
    })
    .await;
}

#[tokio::test]
async fn get_relay_url_default() {
    device_test(|mut ctl| async move {
        let url = ctl.get_relay_url().await.unwrap();
        assert_eq!(url.as_str(), "");
    })
    .await;
}

#[tokio::test]
async fn set_and_get_relay_url() {
    device_test(|mut ctl| async move {
        ctl.set_relay_url("wss://relay.example.com/stream")
            .await
            .unwrap();
        let url = ctl.get_relay_url().await.unwrap();
        assert_eq!(url.as_str(), "wss://relay.example.com/stream");
    })
    .await;
}

#[tokio::test]
async fn reset_ui_to_bootloader_gpio_sequence() {
    device_test_with_gpio_tracking(|mut ctl, gpio_ops| async move {
        ctl.reset_ui_to_bootloader(|_| async {}).await.unwrap();

        let ops = gpio_ops.lock().unwrap();
        // First 2 ops are MGMT startup releasing both chips from reset
        // Then UI bootloader sequence: BOOT0=1, BOOT1=0, then RST low -> (delay) -> RST high
        assert_eq!(ops.len(), 6);
        assert_eq!(ops[0], ("UI_RST", GpioOp::SetHigh)); // MGMT startup
        assert_eq!(ops[1], ("NET_RST", GpioOp::SetHigh)); // MGMT startup
        assert_eq!(ops[2], ("UI_BOOT0", GpioOp::SetHigh));
        assert_eq!(ops[3], ("UI_BOOT1", GpioOp::SetLow));
        assert_eq!(ops[4], ("UI_RST", GpioOp::SetLow));
        assert_eq!(ops[5], ("UI_RST", GpioOp::SetHigh));
    })
    .await;
}

#[tokio::test]
async fn reset_ui_to_user_gpio_sequence() {
    device_test_with_gpio_tracking(|mut ctl, gpio_ops| async move {
        ctl.reset_ui_to_user(|_| async {}).await.unwrap();

        let ops = gpio_ops.lock().unwrap();
        // First 2 ops are MGMT startup releasing both chips from reset
        // Then UI user mode sequence: BOOT0=0, BOOT1=1, then RST low -> (delay) -> RST high
        assert_eq!(ops.len(), 6);
        assert_eq!(ops[0], ("UI_RST", GpioOp::SetHigh)); // MGMT startup
        assert_eq!(ops[1], ("NET_RST", GpioOp::SetHigh)); // MGMT startup
        assert_eq!(ops[2], ("UI_BOOT0", GpioOp::SetLow));
        assert_eq!(ops[3], ("UI_BOOT1", GpioOp::SetHigh));
        assert_eq!(ops[4], ("UI_RST", GpioOp::SetLow));
        assert_eq!(ops[5], ("UI_RST", GpioOp::SetHigh));
    })
    .await;
}

#[tokio::test]
async fn reset_net_to_bootloader_gpio_sequence() {
    device_test_with_gpio_tracking(|mut ctl, gpio_ops| async move {
        ctl.reset_net_to_bootloader(|_ms| async {}).await.unwrap();

        let ops = gpio_ops.lock().unwrap();
        // First 2 ops are MGMT startup releasing both chips from reset
        // Then NET bootloader sequence (matches C code):
        // 1. First power cycle (clean slate)
        // 2. BOOT low
        // 3. Second power cycle (ESP32 samples BOOT when RST goes high)
        assert_eq!(ops.len(), 7);
        assert_eq!(ops[0], ("UI_RST", GpioOp::SetHigh)); // MGMT startup
        assert_eq!(ops[1], ("NET_RST", GpioOp::SetHigh)); // MGMT startup
        assert_eq!(ops[2], ("NET_RST", GpioOp::SetLow));
        assert_eq!(ops[3], ("NET_RST", GpioOp::SetHigh));
        assert_eq!(ops[4], ("NET_BOOT", GpioOp::SetLow));
        assert_eq!(ops[5], ("NET_RST", GpioOp::SetLow));
        assert_eq!(ops[6], ("NET_RST", GpioOp::SetHigh));
        // BOOT stays low (not set back to high)
    })
    .await;
}

#[tokio::test]
async fn reset_net_to_user_gpio_sequence() {
    device_test_with_gpio_tracking(|mut ctl, gpio_ops| async move {
        ctl.reset_net_to_user(|_ms| async {}).await.unwrap();

        let ops = gpio_ops.lock().unwrap();
        // First 2 ops are MGMT startup releasing both chips from reset
        // Then NET user mode sequence: BOOT high -> RST low -> (delay) -> RST high
        assert_eq!(ops.len(), 5);
        assert_eq!(ops[0], ("UI_RST", GpioOp::SetHigh)); // MGMT startup
        assert_eq!(ops[1], ("NET_RST", GpioOp::SetHigh)); // MGMT startup
        assert_eq!(ops[2], ("NET_BOOT", GpioOp::SetHigh));
        assert_eq!(ops[3], ("NET_RST", GpioOp::SetLow));
        assert_eq!(ops[4], ("NET_RST", GpioOp::SetHigh));
    })
    .await;
}

// ── Hello handshake ──

#[tokio::test]
async fn hello_handshake() {
    device_test(|mut ctl| async move {
        let challenge = [0x01, 0x02, 0x03, 0x04];
        assert!(ctl.hello(&challenge).await);
    })
    .await;
}

// ── MGMT stack operations ──

#[tokio::test]
async fn mgmt_stack_info() {
    device_test(|mut ctl| async move {
        let info = ctl.mgmt_get_stack_info().await.unwrap();
        // NoOpStackMonitor returns zeros for everything
        assert_eq!(info.stack_size, 0);
        assert_eq!(info.stack_used, 0);
    })
    .await;
}

#[tokio::test]
async fn mgmt_stack_repaint() {
    device_test(|mut ctl| async move {
        ctl.mgmt_repaint_stack().await.unwrap();
    })
    .await;
}

// ── UI loopback ──

#[tokio::test]
async fn ui_loopback_default_off() {
    device_test(|mut ctl| async move {
        let mode = ctl.ui_get_loopback().await.unwrap();
        assert_eq!(mode, UiLoopbackMode::Off);
    })
    .await;
}

#[tokio::test]
async fn ui_loopback_set_raw() {
    device_test(|mut ctl| async move {
        ctl.ui_set_loopback(UiLoopbackMode::Raw).await.unwrap();
        let mode = ctl.ui_get_loopback().await.unwrap();
        assert_eq!(mode, UiLoopbackMode::Raw);
    })
    .await;
}

#[tokio::test]
async fn ui_loopback_set_alaw() {
    device_test(|mut ctl| async move {
        ctl.ui_set_loopback(UiLoopbackMode::Alaw).await.unwrap();
        let mode = ctl.ui_get_loopback().await.unwrap();
        assert_eq!(mode, UiLoopbackMode::Alaw);
    })
    .await;
}

#[tokio::test]
async fn ui_loopback_set_sframe() {
    device_test(|mut ctl| async move {
        ctl.ui_set_loopback(UiLoopbackMode::Sframe).await.unwrap();
        let mode = ctl.ui_get_loopback().await.unwrap();
        assert_eq!(mode, UiLoopbackMode::Sframe);
    })
    .await;
}

// ── UI pin control (with GPIO tracking) ──

#[tokio::test]
async fn set_ui_boot0_gpio() {
    device_test_with_gpio_tracking(|mut ctl, gpio_ops| async move {
        ctl.set_ui_boot0(PinValue::High).await.unwrap();
        ctl.set_ui_boot0(PinValue::Low).await.unwrap();

        let ops = gpio_ops.lock().unwrap();
        // First 2 ops are MGMT startup, then our explicit pin sets
        assert_eq!(ops[2], ("UI_BOOT0", GpioOp::SetHigh));
        assert_eq!(ops[3], ("UI_BOOT0", GpioOp::SetLow));
    })
    .await;
}

#[tokio::test]
async fn set_ui_boot1_gpio() {
    device_test_with_gpio_tracking(|mut ctl, gpio_ops| async move {
        ctl.set_ui_boot1(PinValue::High).await.unwrap();
        ctl.set_ui_boot1(PinValue::Low).await.unwrap();

        let ops = gpio_ops.lock().unwrap();
        assert_eq!(ops[2], ("UI_BOOT1", GpioOp::SetHigh));
        assert_eq!(ops[3], ("UI_BOOT1", GpioOp::SetLow));
    })
    .await;
}

#[tokio::test]
async fn set_ui_rst_gpio() {
    device_test_with_gpio_tracking(|mut ctl, gpio_ops| async move {
        ctl.set_ui_rst(PinValue::Low).await.unwrap();
        ctl.set_ui_rst(PinValue::High).await.unwrap();

        let ops = gpio_ops.lock().unwrap();
        assert_eq!(ops[2], ("UI_RST", GpioOp::SetLow));
        assert_eq!(ops[3], ("UI_RST", GpioOp::SetHigh));
    })
    .await;
}

// ── UI reset hold/release (with GPIO tracking) ──

#[tokio::test]
async fn hold_ui_reset_gpio() {
    device_test_with_gpio_tracking(|mut ctl, gpio_ops| async move {
        ctl.hold_ui_reset().await.unwrap();

        let ops = gpio_ops.lock().unwrap();
        // MGMT startup: UI_RST high, NET_RST high; then hold sets UI_RST low
        assert_eq!(ops[2], ("UI_RST", GpioOp::SetLow));
    })
    .await;
}

#[tokio::test]
async fn release_ui_reset_gpio() {
    device_test_with_gpio_tracking(|mut ctl, gpio_ops| async move {
        ctl.hold_ui_reset().await.unwrap();
        ctl.set_ui_rst(PinValue::High).await.unwrap();

        let ops = gpio_ops.lock().unwrap();
        assert_eq!(ops[2], ("UI_RST", GpioOp::SetLow)); // hold
        assert_eq!(ops[3], ("UI_RST", GpioOp::SetHigh)); // release
    })
    .await;
}

// ── UI stack operations ──

#[tokio::test]
async fn ui_stack_info() {
    device_test(|mut ctl| async move {
        let info = ctl.ui_get_stack_info().await.unwrap();
        assert_eq!(info.stack_size, 0);
        assert_eq!(info.stack_used, 0);
    })
    .await;
}

#[tokio::test]
async fn ui_stack_repaint() {
    device_test(|mut ctl| async move {
        ctl.ui_repaint_stack().await.unwrap();
    })
    .await;
}

// ── NET loopback ──

#[tokio::test]
async fn net_loopback_default_off() {
    device_test(|mut ctl| async move {
        let mode = ctl.net_get_loopback().await.unwrap();
        assert_eq!(mode, NetLoopbackMode::Off);
    })
    .await;
}

#[tokio::test]
async fn net_loopback_set_raw() {
    device_test(|mut ctl| async move {
        ctl.net_set_loopback(NetLoopbackMode::Raw).await.unwrap();
        let mode = ctl.net_get_loopback().await.unwrap();
        assert_eq!(mode, NetLoopbackMode::Raw);
    })
    .await;
}

#[tokio::test]
async fn net_loopback_set_moq() {
    device_test(|mut ctl| async move {
        ctl.net_set_loopback(NetLoopbackMode::Moq).await.unwrap();
        let mode = ctl.net_get_loopback().await.unwrap();
        assert_eq!(mode, NetLoopbackMode::Moq);
    })
    .await;
}

// ── NET pin control (with GPIO tracking) ──

#[tokio::test]
async fn set_net_boot_gpio() {
    device_test_with_gpio_tracking(|mut ctl, gpio_ops| async move {
        ctl.set_net_boot(PinValue::Low).await.unwrap();
        ctl.set_net_boot(PinValue::High).await.unwrap();

        let ops = gpio_ops.lock().unwrap();
        assert_eq!(ops[2], ("NET_BOOT", GpioOp::SetLow));
        assert_eq!(ops[3], ("NET_BOOT", GpioOp::SetHigh));
    })
    .await;
}

#[tokio::test]
async fn set_net_rst_gpio() {
    device_test_with_gpio_tracking(|mut ctl, gpio_ops| async move {
        ctl.set_net_rst(PinValue::Low).await.unwrap();
        ctl.set_net_rst(PinValue::High).await.unwrap();

        let ops = gpio_ops.lock().unwrap();
        assert_eq!(ops[2], ("NET_RST", GpioOp::SetLow));
        assert_eq!(ops[3], ("NET_RST", GpioOp::SetHigh));
    })
    .await;
}

// ── NET reset hold/release (with GPIO tracking) ──

#[tokio::test]
async fn hold_net_reset_gpio() {
    device_test_with_gpio_tracking(|mut ctl, gpio_ops| async move {
        ctl.hold_net_reset().await.unwrap();

        let ops = gpio_ops.lock().unwrap();
        assert_eq!(ops[2], ("NET_RST", GpioOp::SetLow));
    })
    .await;
}

#[tokio::test]
async fn release_net_reset_gpio() {
    device_test_with_gpio_tracking(|mut ctl, gpio_ops| async move {
        ctl.hold_net_reset().await.unwrap();
        ctl.set_net_rst(PinValue::High).await.unwrap();

        let ops = gpio_ops.lock().unwrap();
        assert_eq!(ops[2], ("NET_RST", GpioOp::SetLow)); // hold
        assert_eq!(ops[3], ("NET_RST", GpioOp::SetHigh)); // release
    })
    .await;
}

// NOTE: Audio/WebSocket integration tests are not included because the test harness
// uses tokio::select! with futures::join! which doesn't work well with tokio::time::sleep.
// The audio functionality is tested in the ui::audio_streaming_tests module instead,
// and the NET unit tests verify the WebSocket forwarding logic.

//! Integration tests for the multi-chip system.
//!
//! These tests require all features (ctl, mgmt, net, ui) to be enabled.
//! They test the full communication stack between CTL (sync, std::io) and
//! firmware modules (async, embedded_io_async).

#![cfg(all(feature = "ctl", feature = "mgmt", feature = "net", feature = "ui"))]

use crate::shared::mocks::{
    GpioOp, MockAsyncDelay, MockAudioStream, MockButton, MockDelay, MockFlash, MockPin, SyncReader,
    SyncWriter, TrackingPin, async_async_channel, mock_i2c_with_eeprom, mock_led_pins,
    sync_async_channel,
};
use crate::{ctl, mgmt, net, ui};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Channel;
use std::sync::{Arc, Mutex};

/// Run a test with the full device stack.
///
/// The test function receives a `ctl::App` and runs sync operations on it.
/// The firmware tasks run concurrently in async context.
async fn device_test<F>(test_fn: F)
where
    F: FnOnce(ctl::App<SyncReader, SyncWriter>) + Send + 'static,
{
    // CTL <-> MGMT: sync on CTL side, async on MGMT side
    let ((ctl_reader, ctl_writer), (mgmt_from_ctl, mgmt_to_ctl)) = sync_async_channel();

    // MGMT <-> UI: both async
    let ((mgmt_from_ui, mgmt_to_ui), (ui_from_mgmt, ui_to_mgmt)) = async_async_channel();

    // MGMT <-> NET: both async
    let ((mgmt_from_net, mgmt_to_net), (net_from_mgmt, net_to_mgmt)) = async_async_channel();

    // UI <-> NET: both async
    let ((ui_from_net, ui_to_net), (net_from_ui, net_to_ui)) = async_async_channel();

    let ctl_app = ctl::App::new(ctl_reader, ctl_writer);
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
    );

    // Create WS channels for NET app
    let ws_cmd_channel: Channel<CriticalSectionRawMutex, net::WsCommand, 4> = Channel::new();
    let ws_event_channel: Channel<CriticalSectionRawMutex, net::WsEvent, 4> = Channel::new();

    // Run the CTL test in a blocking task to avoid blocking the async runtime
    let ctl_task = tokio::task::spawn_blocking(move || {
        test_fn(ctl_app);
    });

    tokio::select! {
        result = ctl_task => {
            // Test completed - unwrap to propagate any panics
            result.unwrap();
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
async fn device_test_with_gpio_tracking<F>(test_fn: F)
where
    F: FnOnce(ctl::App<SyncReader, SyncWriter>, Arc<Mutex<Vec<(&'static str, GpioOp)>>>) + Send + 'static,
{
    // CTL <-> MGMT: sync on CTL side, async on MGMT side
    let ((ctl_reader, ctl_writer), (mgmt_from_ctl, mgmt_to_ctl)) = sync_async_channel();

    // MGMT <-> UI: both async
    let ((mgmt_from_ui, mgmt_to_ui), (ui_from_mgmt, ui_to_mgmt)) = async_async_channel();

    // MGMT <-> NET: both async
    let ((mgmt_from_net, mgmt_to_net), (net_from_mgmt, net_to_mgmt)) = async_async_channel();

    // UI <-> NET: both async
    let ((ui_from_net, ui_to_net), (net_from_ui, net_to_ui)) = async_async_channel();

    let ctl_app = ctl::App::new(ctl_reader, ctl_writer);

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
    );

    // Create WS channels for NET app
    let ws_cmd_channel: Channel<CriticalSectionRawMutex, net::WsCommand, 4> = Channel::new();
    let ws_event_channel: Channel<CriticalSectionRawMutex, net::WsEvent, 4> = Channel::new();

    // Run the CTL test in a blocking task to avoid blocking the async runtime
    let gpio_ops_clone = gpio_ops.clone();
    let ctl_task = tokio::task::spawn_blocking(move || {
        test_fn(ctl_app, gpio_ops_clone);
    });

    tokio::select! {
        result = ctl_task => {
            // Test completed - unwrap to propagate any panics
            result.unwrap();
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
    device_test(|mut ctl| {
        ctl.mgmt_ping(b"hello mgmt");
    })
    .await;
}

#[tokio::test]
async fn ctl_ui_ping() {
    device_test(|mut ctl| {
        ctl.ui_ping(b"hello ui");
    })
    .await;
}

#[tokio::test]
async fn ctl_net_ping() {
    device_test(|mut ctl| {
        ctl.net_ping(b"hello net");
    })
    .await;
}

#[tokio::test]
async fn ui_first_circular_ping() {
    device_test(|mut ctl| {
        ctl.ui_first_circular_ping(b"hello ui circular");
    })
    .await;
}

#[tokio::test]
async fn net_first_circular_ping() {
    device_test(|mut ctl| {
        ctl.net_first_circular_ping(b"hello net circular");
    })
    .await;
}

#[tokio::test]
async fn get_version_default() {
    device_test(|mut ctl| {
        let version = ctl.get_version();
        assert_eq!(version, 0xffffffff);
    })
    .await;
}

#[tokio::test]
async fn set_and_get_version() {
    device_test(|mut ctl| {
        ctl.set_version(0x12345678);
        let version = ctl.get_version();
        assert_eq!(version, 0x12345678);
    })
    .await;
}

#[tokio::test]
async fn get_sframe_key_default() {
    device_test(|mut ctl| {
        let key = ctl.get_sframe_key();
        assert_eq!(key, [0xff; 16]);
    })
    .await;
}

#[tokio::test]
async fn set_and_get_sframe_key() {
    device_test(|mut ctl| {
        let key = [
            0x5b, 0x9f, 0x37, 0xb1, 0x54, 0x6b, 0x61, 0xf9, 0x14, 0xda, 0x9f, 0x55, 0x7a, 0x8f,
            0xe2, 0x15,
        ];
        ctl.set_sframe_key(&key);
        let result = ctl.get_sframe_key();
        assert_eq!(result, key);
    })
    .await;
}

#[tokio::test]
async fn get_wifi_ssids_default() {
    device_test(|mut ctl| {
        let ssids = ctl.get_wifi_ssids();
        assert!(ssids.is_empty());
    })
    .await;
}

#[tokio::test]
async fn add_and_get_wifi_ssid() {
    device_test(|mut ctl| {
        ctl.add_wifi_ssid("MyNetwork", "MyPassword");
        let ssids = ctl.get_wifi_ssids();
        assert_eq!(ssids.len(), 1);
        assert_eq!(ssids[0].ssid.as_str(), "MyNetwork");
        assert_eq!(ssids[0].password.as_str(), "MyPassword");
    })
    .await;
}

#[tokio::test]
async fn clear_wifi_ssids() {
    device_test(|mut ctl| {
        ctl.add_wifi_ssid("Network1", "Pass1");
        ctl.add_wifi_ssid("Network2", "Pass2");
        ctl.clear_wifi_ssids();
        let ssids = ctl.get_wifi_ssids();
        assert!(ssids.is_empty());
    })
    .await;
}

#[tokio::test]
async fn get_relay_url_default() {
    device_test(|mut ctl| {
        let url = ctl.get_relay_url();
        assert_eq!(url.as_str(), "");
    })
    .await;
}

#[tokio::test]
async fn set_and_get_relay_url() {
    device_test(|mut ctl| {
        ctl.set_relay_url("wss://relay.example.com/stream");
        let url = ctl.get_relay_url();
        assert_eq!(url.as_str(), "wss://relay.example.com/stream");
    })
    .await;
}

#[tokio::test]
async fn reset_ui_to_bootloader_gpio_sequence() {
    device_test_with_gpio_tracking(|mut ctl, gpio_ops| {
        ctl.reset_ui_to_bootloader();

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
    device_test_with_gpio_tracking(|mut ctl, gpio_ops| {
        ctl.reset_ui_to_user();

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
    device_test_with_gpio_tracking(|mut ctl, gpio_ops| {
        ctl.reset_net_to_bootloader();

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
    device_test_with_gpio_tracking(|mut ctl, gpio_ops| {
        ctl.reset_net_to_user();

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

// NOTE: Audio/WebSocket integration tests are not included because the test harness
// uses tokio::select! with futures::join! which doesn't work well with tokio::time::sleep.
// The audio functionality is tested in the ui::audio_streaming_tests module instead,
// and the NET unit tests verify the WebSocket forwarding logic.

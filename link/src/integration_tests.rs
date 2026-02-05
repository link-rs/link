//! Integration tests for the multi-chip system.
//!
//! These tests require all features (ctl, mgmt, net, ui) to be enabled.
//! They test the full communication stack between CTL (using async CtlCore) and
//! firmware modules (async, embedded_io_async).

#![cfg(all(feature = "ctl", feature = "mgmt", feature = "net", feature = "ui"))]

use crate::ctl::CtlCore;
use crate::shared::mocks::{
    GpioOp, MockAsyncDelay, MockAudioStream, MockButton, MockCtlPort, MockDelay, MockFlash,
    MockPin, TrackingPin, async_async_channel, ctl_async_channel,
    ctl_async_channel_with_baud_tracking, mock_i2c_with_eeprom, mock_led_pins,
};
use crate::{mgmt, net, ui};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Channel;
use std::sync::atomic::{AtomicU32, Ordering};
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
        ctl.reset_ui_to_bootloader().await.unwrap();

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
        ctl.reset_ui_to_user().await.unwrap();

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
        ctl.reset_net_to_bootloader().await.unwrap();

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
        ctl.reset_net_to_user().await.unwrap();

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

/// Test harness that provides access to tracked baud rate changes.
///
/// Uses AsyncWriter/AsyncReader with baud rate tracking built in.
/// The test function receives the CTL baud rate and NET baud rate atomics to verify changes.
async fn device_test_with_baud_tracking<F, Fut>(test_fn: F)
where
    F: FnOnce(CtlCore<MockCtlPort>, Arc<AtomicU32>, Arc<AtomicU32>) -> Fut,
    Fut: std::future::Future<Output = ()>,
{
    use crate::shared::mocks::AsyncReader;
    use crate::shared::mocks::AsyncWriter;

    // Create baud rate tracking atomics
    let ctl_baud = Arc::new(AtomicU32::new(115200));
    let net_baud = Arc::new(AtomicU32::new(115200));

    // CTL <-> MGMT: both async with baud rate tracking
    // ctl_writer -> mgmt_reader (CTL sends to MGMT)
    let (tx1, rx1) = tokio::sync::mpsc::unbounded_channel();
    // mgmt_writer -> ctl_reader (MGMT sends to CTL)
    let (tx2, rx2) = tokio::sync::mpsc::unbounded_channel();
    let ctl_port = MockCtlPort::new(
        AsyncReader::with_baud_tracking(rx2, ctl_baud.clone()),
        AsyncWriter::with_baud_tracking(tx1, ctl_baud.clone()),
    );
    let (mgmt_from_ctl, mgmt_to_ctl) = (
        AsyncReader::with_baud_tracking(rx1, ctl_baud.clone()),
        AsyncWriter::with_baud_tracking(tx2, ctl_baud.clone()),
    );

    // MGMT <-> UI: both async (no baud rate tracking needed)
    let ((mgmt_from_ui, mgmt_to_ui), (ui_from_mgmt, ui_to_mgmt)) = async_async_channel();

    // MGMT <-> NET: both async with baud rate tracking
    let (tx3, rx3) = tokio::sync::mpsc::unbounded_channel();
    let (tx4, rx4) = tokio::sync::mpsc::unbounded_channel();
    let (mgmt_from_net, mgmt_to_net) = (
        AsyncReader::with_baud_tracking(rx4, net_baud.clone()),
        AsyncWriter::with_baud_tracking(tx3, net_baud.clone()),
    );
    let (net_from_mgmt, net_to_mgmt) = (AsyncReader::new(rx3), AsyncWriter::new(tx4));

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
    );

    // Create WS channels for NET app
    let ws_cmd_channel: Channel<CriticalSectionRawMutex, net::WsCommand, 4> = Channel::new();
    let ws_event_channel: Channel<CriticalSectionRawMutex, net::WsEvent, 4> = Channel::new();

    // Run the CTL test as an async task
    let ctl_task = test_fn(ctl_core, ctl_baud, net_baud);

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
async fn set_net_baud_rate() {
    device_test_with_baud_tracking(|mut ctl, ctl_baud, net_baud| async move {
        // Verify initial baud rates
        assert_eq!(ctl_baud.load(Ordering::SeqCst), 115200);
        assert_eq!(net_baud.load(Ordering::SeqCst), 115200);

        // Set NET baud rate
        ctl.set_net_baud_rate(460800).await.unwrap();

        // Verify NET changed, CTL unchanged
        assert_eq!(net_baud.load(Ordering::SeqCst), 460800);
        assert_eq!(ctl_baud.load(Ordering::SeqCst), 115200);
    })
    .await;
}

#[tokio::test]
async fn set_ctl_baud_rate() {
    device_test_with_baud_tracking(|mut ctl, ctl_baud, net_baud| async move {
        // Verify initial baud rates
        assert_eq!(ctl_baud.load(Ordering::SeqCst), 115200);
        assert_eq!(net_baud.load(Ordering::SeqCst), 115200);

        // Set CTL baud rate
        ctl.set_ctl_baud_rate(230400).await.unwrap();

        // Verify CTL changed, NET unchanged
        assert_eq!(ctl_baud.load(Ordering::SeqCst), 230400);
        assert_eq!(net_baud.load(Ordering::SeqCst), 115200);
    })
    .await;
}

#[tokio::test]
async fn set_both_baud_rates() {
    device_test_with_baud_tracking(|mut ctl, ctl_baud, net_baud| async move {
        // Verify initial baud rates
        assert_eq!(ctl_baud.load(Ordering::SeqCst), 115200);
        assert_eq!(net_baud.load(Ordering::SeqCst), 115200);

        // Set NET baud rate first
        ctl.set_net_baud_rate(921600).await.unwrap();
        assert_eq!(net_baud.load(Ordering::SeqCst), 921600);
        assert_eq!(ctl_baud.load(Ordering::SeqCst), 115200);

        // Then set CTL baud rate
        ctl.set_ctl_baud_rate(460800).await.unwrap();
        assert_eq!(net_baud.load(Ordering::SeqCst), 921600);
        assert_eq!(ctl_baud.load(Ordering::SeqCst), 460800);
    })
    .await;
}

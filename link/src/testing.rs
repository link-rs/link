//! Integration tests for the multi-chip system.

use crate::mocks::{
    mock_i2c_with_eeprom, mock_led_pins, GpioOp, MockAsyncDelay, MockAudioStream, MockButton,
    MockDelay, MockFlash, MockPin, TrackingPin,
};
use crate::{ctl, mgmt, net, ui};
use core::future::Future;
use embedded_io_adapters::futures_03::FromFutures;
use std::sync::{Arc, Mutex};

type Reader = FromFutures<async_ringbuffer::Reader>;
type Writer = FromFutures<async_ringbuffer::Writer>;

fn channel() -> (Writer, Reader) {
    const BUFFER_CAPACITY: usize = 1024;
    let (w, r) = async_ringbuffer::ring_buffer(BUFFER_CAPACITY);
    (FromFutures::new(w), FromFutures::new(r))
}

async fn device_test<F, Fut>(test_fn: F)
where
    F: FnOnce(ctl::App<Reader, Writer>) -> Fut,
    Fut: Future<Output = ()>,
{
    let (ctl_to_mgmt, mgmt_from_ctl) = channel();
    let (mgmt_to_ctl, ctl_from_mgmt) = channel();

    let (ui_to_mgmt, mgmt_from_ui) = channel();
    let (mgmt_to_ui, ui_from_mgmt) = channel();

    let (net_to_mgmt, mgmt_from_net) = channel();
    let (mgmt_to_net, net_from_mgmt) = channel();

    let (net_to_ui, ui_from_net) = channel();
    let (ui_to_net, net_from_ui) = channel();

    let ctl_app = ctl::App::new(ctl_to_mgmt, ctl_from_mgmt);
    let ui_reset_pins = mgmt::UiResetPins::new(MockPin::new(), MockPin::new());
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
    let ui_app = ui::App::new(
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
    );
    let net_app = net::App::new(
        net_to_mgmt,
        net_from_mgmt,
        net_to_ui,
        net_from_ui,
        mock_led_pins(),
        MockFlash::new(),
        0,
    );

    tokio::select! {
        _ = test_fn(ctl_app) => {},
        _ = mgmt_task => {},
        _ = ui_app.run() => {},
        _ = net_app.run() => {},
    }
}

/// Test harness that provides access to tracked GPIO operations.
async fn device_test_with_gpio_tracking<F, Fut>(test_fn: F)
where
    F: FnOnce(ctl::App<Reader, Writer>, Arc<Mutex<Vec<(&'static str, GpioOp)>>>) -> Fut,
    Fut: Future<Output = ()>,
{
    let (ctl_to_mgmt, mgmt_from_ctl) = channel();
    let (mgmt_to_ctl, ctl_from_mgmt) = channel();

    let (ui_to_mgmt, mgmt_from_ui) = channel();
    let (mgmt_to_ui, ui_from_mgmt) = channel();

    let (net_to_mgmt, mgmt_from_net) = channel();
    let (mgmt_to_net, net_from_mgmt) = channel();

    let (net_to_ui, ui_from_net) = channel();
    let (ui_to_net, net_from_ui) = channel();

    let ctl_app = ctl::App::new(ctl_to_mgmt, ctl_from_mgmt);

    // Create tracking pins for UI and NET reset
    let gpio_ops: Arc<Mutex<Vec<(&'static str, GpioOp)>>> = Arc::new(Mutex::new(Vec::new()));
    let ui_boot_pin = TrackingPin::new("UI_BOOT", gpio_ops.clone());
    let ui_rst_pin = TrackingPin::new("UI_RST", gpio_ops.clone());
    let ui_reset_pins = mgmt::UiResetPins::new(ui_boot_pin, ui_rst_pin);

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
    let ui_app = ui::App::new(
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
    );
    let net_app = net::App::new(
        net_to_mgmt,
        net_from_mgmt,
        net_to_ui,
        net_from_ui,
        mock_led_pins(),
        MockFlash::new(),
        0,
    );

    tokio::select! {
        _ = test_fn(ctl_app, gpio_ops) => {},
        _ = mgmt_task => {},
        _ = ui_app.run() => {},
        _ = net_app.run() => {},
    }
}

#[tokio::test]
async fn ctl_mgmt_ping() {
    device_test(|mut ctl| async move {
        ctl.mgmt_ping(b"hello mgmt").await;
    })
    .await;
}

#[tokio::test]
async fn ctl_ui_ping() {
    device_test(|mut ctl| async move {
        ctl.ui_ping(b"hello ui").await;
    })
    .await;
}

#[tokio::test]
async fn ctl_net_ping() {
    device_test(|mut ctl| async move {
        ctl.net_ping(b"hello net").await;
    })
    .await;
}

#[tokio::test]
async fn ui_first_circular_ping() {
    device_test(|mut ctl| async move {
        ctl.ui_first_circular_ping(b"hello ui circular").await;
    })
    .await;
}

#[tokio::test]
async fn net_first_circular_ping() {
    device_test(|mut ctl| async move {
        ctl.net_first_circular_ping(b"hello net circular").await;
    })
    .await;
}

#[tokio::test]
async fn get_version_default() {
    device_test(|mut ctl| async move {
        let version = ctl.get_version().await;
        assert_eq!(version, 0xffffffff);
    })
    .await;
}

#[tokio::test]
async fn set_and_get_version() {
    device_test(|mut ctl| async move {
        ctl.set_version(0x12345678).await;
        let version = ctl.get_version().await;
        assert_eq!(version, 0x12345678);
    })
    .await;
}

#[tokio::test]
async fn get_sframe_key_default() {
    device_test(|mut ctl| async move {
        let key = ctl.get_sframe_key().await;
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
        ctl.set_sframe_key(&key).await;
        let result = ctl.get_sframe_key().await;
        assert_eq!(result, key);
    })
    .await;
}

#[tokio::test]
async fn get_wifi_ssids_default() {
    device_test(|mut ctl| async move {
        let ssids = ctl.get_wifi_ssids().await;
        assert!(ssids.is_empty());
    })
    .await;
}

#[tokio::test]
async fn add_and_get_wifi_ssid() {
    device_test(|mut ctl| async move {
        ctl.add_wifi_ssid("MyNetwork", "MyPassword").await;
        let ssids = ctl.get_wifi_ssids().await;
        assert_eq!(ssids.len(), 1);
        assert_eq!(ssids[0].ssid.as_str(), "MyNetwork");
        assert_eq!(ssids[0].password.as_str(), "MyPassword");
    })
    .await;
}

#[tokio::test]
async fn clear_wifi_ssids() {
    device_test(|mut ctl| async move {
        ctl.add_wifi_ssid("Network1", "Pass1").await;
        ctl.add_wifi_ssid("Network2", "Pass2").await;
        ctl.clear_wifi_ssids().await;
        let ssids = ctl.get_wifi_ssids().await;
        assert!(ssids.is_empty());
    })
    .await;
}

#[tokio::test]
async fn get_moq_url_default() {
    device_test(|mut ctl| async move {
        let url = ctl.get_moq_url().await;
        assert_eq!(url.as_str(), "");
    })
    .await;
}

#[tokio::test]
async fn set_and_get_moq_url() {
    device_test(|mut ctl| async move {
        ctl.set_moq_url("https://moq.example.com/stream").await;
        let url = ctl.get_moq_url().await;
        assert_eq!(url.as_str(), "https://moq.example.com/stream");
    })
    .await;
}

#[tokio::test]
async fn reset_ui_to_bootloader_gpio_sequence() {
    device_test_with_gpio_tracking(|mut ctl, gpio_ops| async move {
        ctl.reset_ui_to_bootloader().await;

        let ops = gpio_ops.lock().unwrap();
        // UI bootloader sequence: BOOT high -> RST low -> (delay) -> RST high -> BOOT low
        assert_eq!(ops.len(), 4);
        assert_eq!(ops[0], ("UI_BOOT", GpioOp::SetHigh));
        assert_eq!(ops[1], ("UI_RST", GpioOp::SetLow));
        assert_eq!(ops[2], ("UI_RST", GpioOp::SetHigh));
        assert_eq!(ops[3], ("UI_BOOT", GpioOp::SetLow));
    })
    .await;
}

#[tokio::test]
async fn reset_ui_to_user_gpio_sequence() {
    device_test_with_gpio_tracking(|mut ctl, gpio_ops| async move {
        ctl.reset_ui_to_user().await;

        let ops = gpio_ops.lock().unwrap();
        // UI user mode sequence: BOOT low -> RST low -> (delay) -> RST high
        assert_eq!(ops.len(), 3);
        assert_eq!(ops[0], ("UI_BOOT", GpioOp::SetLow));
        assert_eq!(ops[1], ("UI_RST", GpioOp::SetLow));
        assert_eq!(ops[2], ("UI_RST", GpioOp::SetHigh));
    })
    .await;
}

#[tokio::test]
async fn reset_net_to_bootloader_gpio_sequence() {
    device_test_with_gpio_tracking(|mut ctl, gpio_ops| async move {
        ctl.reset_net_to_bootloader().await;

        let ops = gpio_ops.lock().unwrap();
        // NET bootloader sequence: BOOT low -> RST low -> (delay) -> RST high -> BOOT high
        assert_eq!(ops.len(), 4);
        assert_eq!(ops[0], ("NET_BOOT", GpioOp::SetLow));
        assert_eq!(ops[1], ("NET_RST", GpioOp::SetLow));
        assert_eq!(ops[2], ("NET_RST", GpioOp::SetHigh));
        assert_eq!(ops[3], ("NET_BOOT", GpioOp::SetHigh));
    })
    .await;
}

#[tokio::test]
async fn reset_net_to_user_gpio_sequence() {
    device_test_with_gpio_tracking(|mut ctl, gpio_ops| async move {
        ctl.reset_net_to_user().await;

        let ops = gpio_ops.lock().unwrap();
        // NET user mode sequence: BOOT high -> RST low -> (delay) -> RST high
        assert_eq!(ops.len(), 3);
        assert_eq!(ops[0], ("NET_BOOT", GpioOp::SetHigh));
        assert_eq!(ops[1], ("NET_RST", GpioOp::SetLow));
        assert_eq!(ops[2], ("NET_RST", GpioOp::SetHigh));
    })
    .await;
}

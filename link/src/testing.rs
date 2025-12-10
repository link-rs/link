//! Integration tests for the multi-chip system.

use crate::mocks::{
    mock_i2c_with_eeprom, mock_led_pins, MockAudioCodec, MockAudioStream, MockButton, MockDelay,
    MockFlash,
};
use crate::{ctl, mgmt, net, ui};
use core::future::Future;
use embedded_io_adapters::futures_03::FromFutures;

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
    let mgmt_app = mgmt::App::new(
        mgmt_to_ctl,
        mgmt_from_ctl,
        mgmt_to_ui,
        mgmt_from_ui,
        mgmt_to_net,
        mgmt_from_net,
        mock_led_pins(),
        mock_led_pins(),
    );
    let ui_app = ui::App::new(
        ui_to_mgmt,
        ui_from_mgmt,
        ui_to_net,
        ui_from_net,
        mock_led_pins(),
        MockButton,
        MockButton,
        mock_i2c_with_eeprom(),
        MockDelay,
        MockAudioCodec,
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
        _ = mgmt_app.run() => {},
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

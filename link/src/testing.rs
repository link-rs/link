//! Test utilities and integration tests.

use crate::{ctl, mgmt, net, ui};
use core::convert::Infallible;
use core::future::Future;
use embedded_hal::digital::{ErrorType, OutputPin, StatefulOutputPin};
use embedded_hal_async::digital::Wait;
use embedded_io_adapters::futures_03::FromFutures;

type Reader = FromFutures<async_ringbuffer::Reader>;
type Writer = FromFutures<async_ringbuffer::Writer>;

fn channel() -> (Writer, Reader) {
    const BUFFER_CAPACITY: usize = 1024;
    let (w, r) = async_ringbuffer::ring_buffer(BUFFER_CAPACITY);
    (FromFutures::new(w), FromFutures::new(r))
}

/// Mock pin for testing LED functionality
pub struct MockPin {
    state: bool,
}

impl MockPin {
    pub fn new() -> Self {
        Self { state: false }
    }
}

impl Default for MockPin {
    fn default() -> Self {
        Self::new()
    }
}

impl ErrorType for MockPin {
    type Error = Infallible;
}

impl OutputPin for MockPin {
    fn set_low(&mut self) -> Result<(), Self::Error> {
        self.state = false;
        Ok(())
    }

    fn set_high(&mut self) -> Result<(), Self::Error> {
        self.state = true;
        Ok(())
    }
}

impl StatefulOutputPin for MockPin {
    fn is_set_high(&mut self) -> Result<bool, Self::Error> {
        Ok(self.state)
    }

    fn is_set_low(&mut self) -> Result<bool, Self::Error> {
        Ok(!self.state)
    }
}

/// Mock button for testing button functionality (never triggers)
pub struct MockButton;

impl ErrorType for MockButton {
    type Error = Infallible;
}

impl Wait for MockButton {
    async fn wait_for_high(&mut self) -> Result<(), Self::Error> {
        core::future::pending().await
    }

    async fn wait_for_low(&mut self) -> Result<(), Self::Error> {
        core::future::pending().await
    }

    async fn wait_for_rising_edge(&mut self) -> Result<(), Self::Error> {
        core::future::pending().await
    }

    async fn wait_for_falling_edge(&mut self) -> Result<(), Self::Error> {
        core::future::pending().await
    }

    async fn wait_for_any_edge(&mut self) -> Result<(), Self::Error> {
        core::future::pending().await
    }
}

/// Create a tuple of mock LED pins
pub fn mock_led_pins() -> (MockPin, MockPin, MockPin) {
    (MockPin::new(), MockPin::new(), MockPin::new())
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
    );
    let net_app = net::App::new(
        net_to_mgmt,
        net_from_mgmt,
        net_to_ui,
        net_from_ui,
        mock_led_pins(),
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

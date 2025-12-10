//! Mock implementations for testing.
//!
//! This module provides reusable mock implementations of hardware traits
//! for use in tests across the crate.

use core::convert::Infallible;
use embedded_hal::delay::DelayNs;
use embedded_hal::digital::{ErrorType as DigitalErrorType, OutputPin, StatefulOutputPin};
use embedded_hal::i2c::{ErrorType as I2cErrorType, Operation};
use embedded_hal_async::digital::Wait;

/// Trait for I2C device handlers that can be attached to MockI2c.
pub trait I2cDevice {
    /// Handle an I2C transaction for this device.
    fn transaction(&mut self, operations: &mut [Operation<'_>]) -> Result<(), Infallible>;
}

impl<T: I2cDevice> I2cDevice for std::rc::Rc<std::cell::RefCell<T>> {
    fn transaction(&mut self, operations: &mut [Operation<'_>]) -> Result<(), Infallible> {
        self.borrow_mut().transaction(operations)
    }
}

/// A mock I2C bus that routes transactions to registered device handlers.
pub struct MockI2c {
    devices: std::collections::HashMap<u8, Box<dyn I2cDevice>>,
}

impl MockI2c {
    pub fn new() -> Self {
        Self {
            devices: std::collections::HashMap::new(),
        }
    }

    /// Attach a device handler at the given I2C address.
    pub fn attach<D: I2cDevice + 'static>(&mut self, address: u8, device: D) {
        self.devices.insert(address, Box::new(device));
    }

    /// Attach a shared device handler at the given I2C address.
    /// This allows tests to retain a reference to the mock device to inspect its state.
    pub fn attach_shared<D: I2cDevice + 'static>(
        &mut self,
        address: u8,
        device: std::rc::Rc<std::cell::RefCell<D>>,
    ) {
        self.devices.insert(address, Box::new(device));
    }
}

impl Default for MockI2c {
    fn default() -> Self {
        Self::new()
    }
}

impl I2cErrorType for MockI2c {
    type Error = Infallible;
}

impl embedded_hal::i2c::I2c for MockI2c {
    fn transaction(
        &mut self,
        address: u8,
        operations: &mut [Operation<'_>],
    ) -> Result<(), Self::Error> {
        if let Some(device) = self.devices.get_mut(&address) {
            device.transaction(operations)
        } else {
            // No device at this address - silently succeed (like a real bus with no ACK checking)
            Ok(())
        }
    }
}

/// Mock EEPROM device (256 bytes) that can be attached to MockI2c.
pub struct MockEeprom {
    data: [u8; 256],
}

impl MockEeprom {
    pub const I2C_ADDR: u8 = 0x50;

    pub fn new() -> Self {
        Self { data: [0xff; 256] }
    }
}

impl Default for MockEeprom {
    fn default() -> Self {
        Self::new()
    }
}

impl I2cDevice for MockEeprom {
    fn transaction(&mut self, operations: &mut [Operation<'_>]) -> Result<(), Infallible> {
        let mut addr: usize = 0;
        for op in operations {
            match op {
                Operation::Write(data) => {
                    if !data.is_empty() {
                        addr = data[0] as usize;
                        // If there's more data, it's a write operation
                        for (i, byte) in data.iter().skip(1).enumerate() {
                            if addr + i < 256 {
                                self.data[addr + i] = *byte;
                            }
                        }
                    }
                }
                Operation::Read(data) => {
                    for (i, byte) in data.iter_mut().enumerate() {
                        if addr + i < 256 {
                            *byte = self.data[addr + i];
                        }
                    }
                }
            }
        }
        Ok(())
    }
}

/// Mock delay (no-op for testing).
pub struct MockDelay;

impl DelayNs for MockDelay {
    fn delay_ns(&mut self, _ns: u32) {
        // No-op for testing
    }
}

/// Mock pin for testing LED functionality.
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

impl DigitalErrorType for MockPin {
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

/// Mock button for testing (never triggers).
pub struct MockButton;

impl DigitalErrorType for MockButton {
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

/// Controllable mock button for testing button press/release behavior.
/// Uses tokio channels to trigger edge events.
pub struct ControllableButton {
    rising_rx: tokio::sync::mpsc::Receiver<()>,
    falling_rx: tokio::sync::mpsc::Receiver<()>,
}

/// Handle to control a ControllableButton.
pub struct ButtonController {
    rising_tx: tokio::sync::mpsc::Sender<()>,
    falling_tx: tokio::sync::mpsc::Sender<()>,
}

impl ControllableButton {
    pub fn new() -> (Self, ButtonController) {
        let (rising_tx, rising_rx) = tokio::sync::mpsc::channel(1);
        let (falling_tx, falling_rx) = tokio::sync::mpsc::channel(1);
        (
            Self {
                rising_rx,
                falling_rx,
            },
            ButtonController {
                rising_tx,
                falling_tx,
            },
        )
    }
}

impl ButtonController {
    /// Simulate pressing the button (rising edge).
    pub async fn press(&self) {
        self.rising_tx.send(()).await.ok();
    }

    /// Simulate releasing the button (falling edge).
    pub async fn release(&self) {
        self.falling_tx.send(()).await.ok();
    }
}

impl DigitalErrorType for ControllableButton {
    type Error = Infallible;
}

impl Wait for ControllableButton {
    async fn wait_for_high(&mut self) -> Result<(), Self::Error> {
        core::future::pending().await
    }

    async fn wait_for_low(&mut self) -> Result<(), Self::Error> {
        core::future::pending().await
    }

    async fn wait_for_rising_edge(&mut self) -> Result<(), Self::Error> {
        self.rising_rx.recv().await;
        Ok(())
    }

    async fn wait_for_falling_edge(&mut self) -> Result<(), Self::Error> {
        self.falling_rx.recv().await;
        Ok(())
    }

    async fn wait_for_any_edge(&mut self) -> Result<(), Self::Error> {
        core::future::pending().await
    }
}

/// Create a tuple of mock LED pins.
pub fn mock_led_pins() -> (MockPin, MockPin, MockPin) {
    (MockPin::new(), MockPin::new(), MockPin::new())
}

/// Create a MockI2c with a MockEeprom attached at the standard EEPROM address.
pub fn mock_i2c_with_eeprom() -> MockI2c {
    let mut i2c = MockI2c::new();
    i2c.attach(MockEeprom::I2C_ADDR, MockEeprom::new());
    i2c
}

/// Mock flash storage for testing (4KB).
pub struct MockFlash {
    pub data: [u8; 4096],
}

impl MockFlash {
    pub fn new() -> Self {
        Self { data: [0xff; 4096] }
    }
}

impl Default for MockFlash {
    fn default() -> Self {
        Self::new()
    }
}

impl embedded_storage::ReadStorage for MockFlash {
    type Error = Infallible;

    fn read(&mut self, offset: u32, bytes: &mut [u8]) -> Result<(), Self::Error> {
        let start = offset as usize;
        let end = start + bytes.len();
        bytes.copy_from_slice(&self.data[start..end]);
        Ok(())
    }

    fn capacity(&self) -> usize {
        self.data.len()
    }
}

impl embedded_storage::Storage for MockFlash {
    fn write(&mut self, offset: u32, bytes: &[u8]) -> Result<(), Self::Error> {
        let start = offset as usize;
        self.data[start..start + bytes.len()].copy_from_slice(bytes);
        Ok(())
    }
}

/// Mock audio codec for testing.
pub struct MockAudioCodec;

impl crate::ui::AudioCodec for MockAudioCodec {
    fn start(&mut self) {}
    fn enable_input(&mut self, _enabled: bool) {}
    fn enable_output(&mut self, _enabled: bool) {}
}

/// Mock audio stream for testing that emits frames every 20ms.
///
/// Each frame contains a counter value in the first sample to identify it.
pub struct MockAudioStream {
    frame_counter: u16,
}

impl MockAudioStream {
    pub fn new() -> Self {
        Self { frame_counter: 0 }
    }
}

impl Default for MockAudioStream {
    fn default() -> Self {
        Self::new()
    }
}

impl crate::ui::AudioStream for MockAudioStream {
    async fn start(&mut self) {}
    async fn stop(&mut self) {}
    async fn read(&mut self) -> crate::ui::Frame {
        // Wait 20ms between frames (8kHz sample rate, 320 samples = 40ms per frame,
        // but we use 20ms for faster test execution)
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;

        // Create a frame with a unique identifier in the first sample
        let mut frame = crate::ui::Frame::default();
        frame.0[0] = self.frame_counter;
        self.frame_counter = self.frame_counter.wrapping_add(1);
        frame
    }
    async fn write(&mut self, _frame: &crate::ui::Frame) {}
    async fn read_write(
        &mut self,
        _tx: &crate::ui::Frame,
        rx: &mut crate::ui::Frame,
    ) -> Result<(), crate::ui::AudioError> {
        *rx = self.read().await;
        Ok(())
    }
}

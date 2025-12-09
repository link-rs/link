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

//! Mock implementations for testing.
//!
//! This module provides reusable mock implementations of hardware traits
//! for use in tests across the crate.

use core::convert::Infallible;
use embedded_hal::delay::DelayNs;
use embedded_hal::digital::{ErrorType as DigitalErrorType, OutputPin, StatefulOutputPin};
use embedded_hal::i2c::{ErrorType as I2cErrorType, Operation};
use embedded_hal_async::digital::Wait;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::time::sleep;

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
    rising_rx: mpsc::Receiver<()>,
    falling_rx: mpsc::Receiver<()>,
}

/// Handle to control a ControllableButton.
pub struct ButtonController {
    rising_tx: mpsc::Sender<()>,
    falling_tx: mpsc::Sender<()>,
}

impl ControllableButton {
    pub fn new() -> (Self, ButtonController) {
        let (rising_tx, rising_rx) = mpsc::channel(1);
        let (falling_tx, falling_rx) = mpsc::channel(1);
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
    /// Simulate pressing the button (rising edge for active-high buttons).
    pub async fn press(&self) {
        self.rising_tx.send(()).await.ok();
    }

    /// Simulate releasing the button (falling edge for active-high buttons).
    pub async fn release(&self) {
        self.falling_tx.send(()).await.ok();
    }

    /// Simulate pressing the button (falling edge for active-low buttons).
    pub async fn press_active_low(&self) {
        self.falling_tx.send(()).await.ok();
    }

    /// Simulate releasing the button (rising edge for active-low buttons).
    pub async fn release_active_low(&self) {
        self.rising_tx.send(()).await.ok();
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

/// Mock async delay that does nothing (instant).
pub struct MockAsyncDelay;

impl embedded_hal_async::delay::DelayNs for MockAsyncDelay {
    async fn delay_ns(&mut self, _ns: u32) {
        // No-op for testing
    }
}

/// GPIO operation for tracking pin state changes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GpioOp {
    SetHigh,
    SetLow,
}

/// A mock pin that tracks all operations for verification in tests.
pub struct TrackingPin {
    name: &'static str,
    state: bool,
    ops: std::sync::Arc<std::sync::Mutex<Vec<(&'static str, GpioOp)>>>,
}

impl TrackingPin {
    pub fn new(
        name: &'static str,
        ops: std::sync::Arc<std::sync::Mutex<Vec<(&'static str, GpioOp)>>>,
    ) -> Self {
        Self {
            name,
            state: false,
            ops,
        }
    }
}

impl DigitalErrorType for TrackingPin {
    type Error = Infallible;
}

impl OutputPin for TrackingPin {
    fn set_low(&mut self) -> Result<(), Self::Error> {
        self.state = false;
        self.ops.lock().unwrap().push((self.name, GpioOp::SetLow));
        Ok(())
    }

    fn set_high(&mut self) -> Result<(), Self::Error> {
        self.state = true;
        self.ops.lock().unwrap().push((self.name, GpioOp::SetHigh));
        Ok(())
    }
}

impl StatefulOutputPin for TrackingPin {
    fn is_set_high(&mut self) -> Result<bool, Self::Error> {
        Ok(self.state)
    }

    fn is_set_low(&mut self) -> Result<bool, Self::Error> {
        Ok(!self.state)
    }
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

impl crate::ui::AudioSystem for MockAudioStream {
    fn set_input_enabled<I: embedded_hal::i2c::I2c>(&mut self, _i2c: &mut I, _enable: bool) {
        // No-op for mock
    }

    fn set_output_enabled<I: embedded_hal::i2c::I2c>(&mut self, _i2c: &mut I, _enable: bool) {
        // No-op for mock
    }

    async fn start(&mut self) {}
    async fn stop(&mut self) {}

    async fn read_write(
        &mut self,
        _tx: &crate::ui::StereoFrame,
        rx: &mut crate::ui::StereoFrame,
    ) -> Result<(), crate::ui::AudioError> {
        // Simulate real audio timing (80ms per frame at 8kHz stereo with A-law)
        // Use shorter delay in tests to speed them up while still allowing scheduler to run
        sleep(Duration::from_millis(10)).await;

        // Create a stereo frame with a unique identifier in the first stereo pair
        // Use frame counter as amplitude for both L and R channels
        rx.0[0] = self.frame_counter;
        rx.0[1] = self.frame_counter;
        self.frame_counter = self.frame_counter.wrapping_add(1);
        Ok(())
    }
}

/// Mock audio stream that captures written frames for verification.
/// Captures stereo frames that are sent to the audio hardware.
pub struct CapturingAudioStream {
    frame_counter: u16,
    written_frames: std::sync::Arc<std::sync::Mutex<Vec<crate::ui::StereoFrame>>>,
}

impl CapturingAudioStream {
    pub fn new() -> (
        Self,
        std::sync::Arc<std::sync::Mutex<Vec<crate::ui::StereoFrame>>>,
    ) {
        let written = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        (
            Self {
                frame_counter: 0,
                written_frames: written.clone(),
            },
            written,
        )
    }
}

impl crate::ui::AudioSystem for CapturingAudioStream {
    fn set_input_enabled<I: embedded_hal::i2c::I2c>(&mut self, _i2c: &mut I, _enable: bool) {
        // No-op for mock
    }

    fn set_output_enabled<I: embedded_hal::i2c::I2c>(&mut self, _i2c: &mut I, _enable: bool) {
        // No-op for mock
    }

    async fn start(&mut self) {}
    async fn stop(&mut self) {}

    async fn read_write(
        &mut self,
        tx: &crate::ui::StereoFrame,
        rx: &mut crate::ui::StereoFrame,
    ) -> Result<(), crate::ui::AudioError> {
        // Capture non-silent stereo frames
        if tx.0.iter().any(|&s| s != 0) {
            self.written_frames.lock().unwrap().push(tx.clone());
        }
        // Simulate real audio timing (5ms per frame for faster tests)
        sleep(Duration::from_millis(5)).await;
        // Put frame counter in the first stereo pair
        rx.0[0] = self.frame_counter;
        rx.0[1] = self.frame_counter;
        self.frame_counter = self.frame_counter.wrapping_add(1);
        Ok(())
    }
}

/// Mock audio stream that injects specific mic samples and captures speaker output.
/// Use this to verify that specific audio samples flow through the system correctly.
pub struct InjectableAudioStream {
    /// Queue of stereo frames to inject as mic input (FIFO)
    inject_frames:
        std::sync::Arc<std::sync::Mutex<std::collections::VecDeque<crate::ui::StereoFrame>>>,
    /// Captured stereo frames sent to speaker
    captured_frames: std::sync::Arc<std::sync::Mutex<Vec<crate::ui::StereoFrame>>>,
}

impl InjectableAudioStream {
    pub fn new() -> (
        Self,
        std::sync::Arc<std::sync::Mutex<std::collections::VecDeque<crate::ui::StereoFrame>>>,
        std::sync::Arc<std::sync::Mutex<Vec<crate::ui::StereoFrame>>>,
    ) {
        let inject = std::sync::Arc::new(std::sync::Mutex::new(std::collections::VecDeque::new()));
        let captured = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        (
            Self {
                inject_frames: inject.clone(),
                captured_frames: captured.clone(),
            },
            inject,
            captured,
        )
    }
}

impl crate::ui::AudioSystem for InjectableAudioStream {
    fn set_input_enabled<I: embedded_hal::i2c::I2c>(&mut self, _i2c: &mut I, _enable: bool) {}
    fn set_output_enabled<I: embedded_hal::i2c::I2c>(&mut self, _i2c: &mut I, _enable: bool) {}

    async fn start(&mut self) {}
    async fn stop(&mut self) {}

    async fn read_write(
        &mut self,
        tx: &crate::ui::StereoFrame,
        rx: &mut crate::ui::StereoFrame,
    ) -> Result<(), crate::ui::AudioError> {
        // Capture non-silent frames sent to speaker
        if tx.0.iter().any(|&s| s != 0) {
            self.captured_frames.lock().unwrap().push(tx.clone());
        }

        // Simulate audio timing
        sleep(Duration::from_millis(5)).await;

        // Return injected frame if available, otherwise silence
        if let Some(frame) = self.inject_frames.lock().unwrap().pop_front() {
            *rx = frame;
        } else {
            *rx = crate::ui::StereoFrame::default();
        }
        Ok(())
    }
}

// =============================================================================
// Sync/Async Channel Adapters for Integration Tests
// =============================================================================
//
// These types bridge sync (std::io) and async (embedded_io_async) I/O for testing.
// They use tokio mpsc channels internally, with the sync side using blocking operations.

use std::collections::VecDeque;

/// Writer that implements `std::io::Write` for sync code (like ctl::App).
/// Sends byte chunks through a tokio channel.
pub struct SyncWriter {
    tx: mpsc::UnboundedSender<Vec<u8>>,
}

impl SyncWriter {
    pub fn new(tx: mpsc::UnboundedSender<Vec<u8>>) -> Self {
        Self { tx }
    }
}

impl std::io::Write for SyncWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.tx
            .send(buf.to_vec())
            .map_err(|_| std::io::Error::other("channel closed"))?;
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Reader that implements `std::io::Read` for sync code (like ctl::App).
/// Uses blocking receive from a tokio channel.
pub struct SyncReader {
    rx: mpsc::UnboundedReceiver<Vec<u8>>,
    buffer: VecDeque<u8>,
}

impl SyncReader {
    pub fn new(rx: mpsc::UnboundedReceiver<Vec<u8>>) -> Self {
        Self {
            rx,
            buffer: VecDeque::new(),
        }
    }
}

impl std::io::Read for SyncReader {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        // Fill buffer if empty
        while self.buffer.is_empty() {
            match self.rx.blocking_recv() {
                Some(data) => self.buffer.extend(data),
                None => return Ok(0), // EOF
            }
        }

        // Copy from buffer to output
        let n = buf.len().min(self.buffer.len());
        for b in buf.iter_mut().take(n) {
            *b = self.buffer.pop_front().unwrap();
        }
        Ok(n)
    }
}

/// Writer that implements `embedded_io_async::Write` for async firmware code.
pub struct AsyncWriter {
    tx: mpsc::UnboundedSender<Vec<u8>>,
}

impl AsyncWriter {
    pub fn new(tx: mpsc::UnboundedSender<Vec<u8>>) -> Self {
        Self { tx }
    }
}

impl embedded_io_async::ErrorType for AsyncWriter {
    type Error = Infallible;
}

impl embedded_io_async::Write for AsyncWriter {
    async fn write(&mut self, buf: &[u8]) -> Result<usize, Self::Error> {
        // Send never blocks for unbounded channel, just fails if closed
        let _ = self.tx.send(buf.to_vec());
        Ok(buf.len())
    }

    async fn flush(&mut self) -> Result<(), Self::Error> {
        Ok(())
    }
}

#[cfg(feature = "mgmt")]
impl crate::shared::uart_config::SetBaudRate for AsyncWriter {
    async fn set_baud_rate(&mut self, _baud_rate: u32) {}
}

/// Reader that implements `embedded_io_async::Read` for async firmware code.
pub struct AsyncReader {
    rx: mpsc::UnboundedReceiver<Vec<u8>>,
    buffer: VecDeque<u8>,
}

impl AsyncReader {
    pub fn new(rx: mpsc::UnboundedReceiver<Vec<u8>>) -> Self {
        Self {
            rx,
            buffer: VecDeque::new(),
        }
    }
}

impl embedded_io_async::ErrorType for AsyncReader {
    type Error = Infallible;
}

impl embedded_io_async::Read for AsyncReader {
    async fn read(&mut self, buf: &mut [u8]) -> Result<usize, Self::Error> {
        // Fill buffer if empty
        while self.buffer.is_empty() {
            match self.rx.recv().await {
                Some(data) => self.buffer.extend(data),
                None => return Ok(0), // EOF
            }
        }

        // Copy from buffer to output
        let n = buf.len().min(self.buffer.len());
        for b in buf.iter_mut().take(n) {
            *b = self.buffer.pop_front().unwrap();
        }
        Ok(n)
    }
}

#[cfg(feature = "mgmt")]
impl crate::shared::uart_config::SetBaudRate for AsyncReader {
    async fn set_baud_rate(&mut self, _baud_rate: u32) {}
}

/// Create a bidirectional channel pair for connecting sync and async code.
///
/// Returns `((sync_reader, sync_writer), (async_reader, async_writer))`.
///
/// - The sync side (ctl::App) uses `sync_reader` and `sync_writer`
/// - The async side (firmware) uses `async_reader` and `async_writer`
/// - Data flows: sync_writer -> async_reader, async_writer -> sync_reader
pub fn sync_async_channel() -> ((SyncReader, SyncWriter), (AsyncReader, AsyncWriter)) {
    // sync_writer -> async_reader (CTL sends to firmware)
    let (tx1, rx1) = mpsc::unbounded_channel();
    // async_writer -> sync_reader (firmware sends to CTL)
    let (tx2, rx2) = mpsc::unbounded_channel();

    let sync_end = (SyncReader::new(rx2), SyncWriter::new(tx1));
    let async_end = (AsyncReader::new(rx1), AsyncWriter::new(tx2));

    (sync_end, async_end)
}

/// Create a bidirectional channel pair for connecting two async endpoints.
///
/// Returns two `(AsyncReader, AsyncWriter)` pairs, one for each end.
pub fn async_async_channel() -> ((AsyncReader, AsyncWriter), (AsyncReader, AsyncWriter)) {
    let (tx1, rx1) = mpsc::unbounded_channel();
    let (tx2, rx2) = mpsc::unbounded_channel();

    let end1 = (AsyncReader::new(rx2), AsyncWriter::new(tx1));
    let end2 = (AsyncReader::new(rx1), AsyncWriter::new(tx2));

    (end1, end2)
}

// ============================================================================
// MockCtlPort - for testing CtlCore with async channels
// ============================================================================

/// A mock port that implements `CtlPort` for testing `CtlCore`.
///
/// Combines an `AsyncReader` and `AsyncWriter` into a single type.
#[cfg(feature = "ctl")]
pub struct MockCtlPort {
    reader: AsyncReader,
    writer: AsyncWriter,
}

#[cfg(feature = "ctl")]
impl MockCtlPort {
    pub fn new(reader: AsyncReader, writer: AsyncWriter) -> Self {
        Self { reader, writer }
    }
}

#[cfg(feature = "ctl")]
impl crate::ctl::CtlPort for MockCtlPort {
    type Error = std::convert::Infallible;

    async fn read(&mut self, buf: &mut [u8]) -> Result<usize, Self::Error> {
        use embedded_io_async::Read;
        self.reader.read(buf).await
    }

    async fn write_all(&mut self, buf: &[u8]) -> Result<(), Self::Error> {
        use embedded_io_async::Write;
        let mut written = 0;
        while written < buf.len() {
            let n = self.writer.write(&buf[written..]).await?;
            written += n;
        }
        Ok(())
    }

    async fn flush(&mut self) -> Result<(), Self::Error> {
        use embedded_io_async::Write;
        self.writer.flush().await
    }
}

/// Create a bidirectional channel pair for CTL (using CtlCore) and MGMT firmware.
///
/// Returns `(MockCtlPort, (AsyncReader, AsyncWriter))`.
///
/// - The CTL side uses `MockCtlPort` with `CtlCore`
/// - The MGMT side uses `(async_reader, async_writer)` tuples
/// - Data flows: ctl_port.write -> mgmt_reader, mgmt_writer -> ctl_port.read
#[cfg(feature = "ctl")]
pub fn ctl_async_channel() -> (MockCtlPort, (AsyncReader, AsyncWriter)) {
    // ctl_writer -> mgmt_reader (CTL sends to MGMT)
    let (tx1, rx1) = mpsc::unbounded_channel();
    // mgmt_writer -> ctl_reader (MGMT sends to CTL)
    let (tx2, rx2) = mpsc::unbounded_channel();

    let ctl_port = MockCtlPort::new(AsyncReader::new(rx2), AsyncWriter::new(tx1));
    let mgmt_end = (AsyncReader::new(rx1), AsyncWriter::new(tx2));

    (ctl_port, mgmt_end)
}

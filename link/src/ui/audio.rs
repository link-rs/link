//! Audio types and traits for the UI chip.
//!
//! This module defines the interface between the library and board-level
//! audio implementations. The board is responsible for both codec control
//! (e.g., WM8960 via I2C) and audio streaming (e.g., I2S).

use embedded_hal::delay::DelayNs;
use embedded_hal::i2c::I2c;

/// Size of an audio frame in 16-bit samples.
pub const FRAME_SIZE: usize = 320;

/// An audio frame containing PCM samples.
#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub struct Frame(pub [u16; FRAME_SIZE]);

impl Default for Frame {
    fn default() -> Self {
        Self([0; FRAME_SIZE])
    }
}

impl Frame {
    /// Convert frame samples to bytes (little-endian).
    pub fn as_bytes(&self) -> [u8; FRAME_SIZE * 2] {
        let mut bytes = [0u8; FRAME_SIZE * 2];
        for (i, sample) in self.0.iter().enumerate() {
            let le = sample.to_le_bytes();
            bytes[i * 2] = le[0];
            bytes[i * 2 + 1] = le[1];
        }
        bytes
    }

    /// Create a frame from bytes (little-endian).
    /// Returns None if the slice is not exactly FRAME_SIZE * 2 bytes.
    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        if bytes.len() != FRAME_SIZE * 2 {
            return None;
        }
        let mut frame = Self::default();
        for (i, sample) in frame.0.iter_mut().enumerate() {
            *sample = u16::from_le_bytes([bytes[i * 2], bytes[i * 2 + 1]]);
        }
        Some(frame)
    }

    /// Calculate the energy of this frame.
    /// Energy is the sum of absolute values of samples (treated as signed i16).
    pub fn energy(&self) -> u32 {
        self.0
            .iter()
            .map(|&s| (s as i16).unsigned_abs() as u32)
            .sum()
    }
}

/// I2S audio error types.
#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub enum AudioError {
    /// Audio overrun - data was lost.
    Overrun,
    /// DMA became unsynchronized with the ring buffer.
    DmaUnsynced,
}

/// Unified audio system trait - combines codec control and audio streaming.
///
/// Implementors manage both the audio codec (e.g., WM8960 via I2C) and the
/// audio transport (e.g., I2S). Control methods accept I2C/delay references
/// since the bus may be shared with other peripherals (e.g., EEPROM).
///
/// # Initialization Contract
///
/// The `init()` method MUST configure the codec before the audio transport
/// becomes active. This ensures proper initialization order (codec configured
/// before I2S clocks start).
#[allow(async_fn_in_trait)]
pub trait AudioSystem {
    /// Initialize the audio subsystem.
    ///
    /// This configures the codec via I2C and prepares the audio transport.
    /// The implementation ensures proper initialization order.
    fn init<I: I2c, D: DelayNs>(&mut self, i2c: &mut I, delay: &mut D);

    /// Enable or disable the audio input path (microphone).
    fn set_input_enabled<I: I2c>(&mut self, i2c: &mut I, enable: bool);

    /// Enable or disable the audio output path (speaker/headphone).
    fn set_output_enabled<I: I2c>(&mut self, i2c: &mut I, enable: bool);

    /// Start the audio stream.
    async fn start(&mut self);

    /// Stop the audio stream.
    async fn stop(&mut self);

    /// Perform a full-duplex audio frame transfer.
    async fn read_write(&mut self, tx: &Frame, rx: &mut Frame) -> Result<(), AudioError>;
}

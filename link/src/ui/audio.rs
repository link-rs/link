//! Audio types and traits for the UI chip.
//!
//! This module defines the interface between the library and board-level
//! audio implementations. The board is responsible for both codec control
//! (e.g., WM8960 via I2C) and audio streaming (e.g., I2S).
//!
//! # Audio Format
//!
//! - I2S operates in stereo mode with interleaved L/R samples
//! - Transmitted frames use A-law encoding (1 byte per mono sample)
//! - Recording: stereo → mono (sum L+R) → A-law encode
//! - Playback: A-law decode → mono → stereo (duplicate)

use audio_codec_algorithms::{decode_alaw, encode_alaw};
use embedded_hal::i2c::I2c;

/// Size of an encoded audio frame in bytes (A-law mono samples).
/// This is the transmitted frame size over the network.
/// At 8kHz sample rate, 160 samples = 20ms of audio.
pub const ENCODED_FRAME_SIZE: usize = 160;

/// Size of stereo I2S frame in 16-bit samples.
/// Contains ENCODED_FRAME_SIZE stereo pairs (L/R interleaved).
pub const STEREO_FRAME_SIZE: usize = ENCODED_FRAME_SIZE * 2;

/// Legacy frame size constant for compatibility.
/// Now refers to encoded frame size.
pub const FRAME_SIZE: usize = ENCODED_FRAME_SIZE;

/// Stereo PCM frame for I2S hardware.
/// Contains interleaved left/right 16-bit samples: L0, R0, L1, R1, ...
#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub struct StereoFrame(pub [u16; STEREO_FRAME_SIZE]);

impl Default for StereoFrame {
    fn default() -> Self {
        Self([0; STEREO_FRAME_SIZE])
    }
}

impl StereoFrame {
    /// Extract the active input channel and A-law encode.
    pub fn encode(&self) -> Frame {
        let mut encoded = Frame::default();
        for i in 0..ENCODED_FRAME_SIZE {
            // Left channel (even indices) contains the microphone input
            let sample = self.0[i * 2] as i16;
            encoded.0[i] = encode_alaw(sample);
        }
        encoded
    }

    /// Create from mono samples by duplicating to stereo.
    pub fn from_mono(mono: &[i16; ENCODED_FRAME_SIZE]) -> Self {
        let mut stereo = Self::default();
        for i in 0..ENCODED_FRAME_SIZE {
            let sample = mono[i] as u16;
            stereo.0[i * 2] = sample; // Left
            stereo.0[i * 2 + 1] = sample; // Right
        }
        stereo
    }
}

/// An encoded audio frame containing A-law compressed mono samples.
/// This is the format used for network transmission.
#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub struct Frame(pub [u8; ENCODED_FRAME_SIZE]);

impl Default for Frame {
    fn default() -> Self {
        // A-law silence is 0xD5 (encodes to 0)
        Self([0xD5; ENCODED_FRAME_SIZE])
    }
}

impl Frame {
    /// Get the raw A-law encoded bytes.
    pub fn as_bytes(&self) -> &[u8; ENCODED_FRAME_SIZE] {
        &self.0
    }

    /// Create a frame from bytes.
    /// Returns None if the slice is not exactly ENCODED_FRAME_SIZE bytes.
    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        if bytes.len() != ENCODED_FRAME_SIZE {
            return None;
        }
        let mut frame = Self::default();
        frame.0.copy_from_slice(bytes);
        Some(frame)
    }

    /// Decode A-law to mono PCM samples.
    pub fn decode(&self) -> [i16; ENCODED_FRAME_SIZE] {
        let mut mono = [0i16; ENCODED_FRAME_SIZE];
        for i in 0..ENCODED_FRAME_SIZE {
            mono[i] = decode_alaw(self.0[i]);
        }
        mono
    }

    /// Decode and expand to stereo frame for I2S playback.
    pub fn decode_to_stereo(&self) -> StereoFrame {
        StereoFrame::from_mono(&self.decode())
    }

    /// Calculate the energy of this frame (after decoding).
    /// Energy is the sum of absolute values of decoded samples.
    pub fn energy(&self) -> u32 {
        self.decode().iter().map(|&s| s.unsigned_abs() as u32).sum()
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
/// audio transport (e.g., I2S). Control methods accept I2C references
/// since the bus may be shared with other peripherals (e.g., EEPROM).
///
/// # Initialization
///
/// Implementations are expected to be fully initialized at construction time.
/// The codec should be configured before the audio transport becomes active.
///
/// # Audio Format
///
/// The `read_write` method operates on stereo I2S frames. The caller is
/// responsible for encoding/decoding between stereo PCM and A-law mono
/// using the `StereoFrame::encode()` and `Frame::decode_to_stereo()` methods.
#[allow(async_fn_in_trait)]
pub trait AudioSystem {
    /// Enable or disable the audio input path (microphone).
    fn set_input_enabled<I: I2c>(&mut self, i2c: &mut I, enable: bool);

    /// Enable or disable the audio output path (speaker/headphone).
    fn set_output_enabled<I: I2c>(&mut self, i2c: &mut I, enable: bool);

    /// Start the audio stream.
    async fn start(&mut self);

    /// Stop the audio stream.
    async fn stop(&mut self);

    /// Perform a full-duplex stereo audio frame transfer.
    /// - tx: stereo frame to send to DAC
    /// - rx: stereo frame received from ADC
    async fn read_write(
        &mut self,
        tx: &StereoFrame,
        rx: &mut StereoFrame,
    ) -> Result<(), AudioError>;
}

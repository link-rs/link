//! Audio capture from UI chip.
//!
//! This module handles receiving audio frames from the UI chip,
//! decrypting them with SFrame, decoding A-law, and outputting PCM samples.

use crate::shared::chunk;
use crate::shared::sframe::{self, SFrameState};
use audio_codec_algorithms::decode_alaw;

/// Trait for audio output sinks.
///
/// Implemented by platform-specific audio libraries (e.g., cpal) or mocks for testing.
pub trait AudioSink {
    /// Write PCM samples to the audio output.
    ///
    /// Samples are 16-bit signed integers at 8kHz mono.
    fn write_samples(&mut self, samples: &[i16]);
}

/// Audio capture session.
///
/// Handles decrypting and decoding audio frames from the UI chip.
pub struct CaptureSession {
    sframe: SFrameState,
}

impl CaptureSession {
    /// Create a new capture session with the given SFrame key.
    ///
    /// The key should be the 16-byte SFrame key from the UI chip's EEPROM.
    pub fn new(sframe_key: &[u8; 16]) -> Self {
        Self {
            sframe: SFrameState::new(sframe_key, 0),
        }
    }

    /// Process an incoming audio frame from the UI chip.
    ///
    /// The frame format is: channel_id (1 byte) + encrypted chunk
    /// (SFrame header + encrypted data + auth tag).
    ///
    /// On success, decoded PCM samples are written to the sink.
    /// Returns Ok(true) if audio was played, Ok(false) if the frame was invalid,
    /// Err if decryption failed.
    pub fn process_frame<S: AudioSink>(
        &mut self,
        frame_data: &[u8],
        sink: &mut S,
    ) -> Result<bool, CaptureError> {
        // Frame format: channel_id (1 byte) + encrypted chunk
        if frame_data.len() < 2 {
            return Ok(false);
        }

        // Extract channel_id (unused for now, but could be used for routing)
        let _channel_id = frame_data[0];
        let encrypted = &frame_data[1..];

        // Decrypt the SFrame data
        let mut buf = Vec::with_capacity(encrypted.len());
        buf.extend_from_slice(encrypted);

        // Convert to heapless::Vec for SFrame API
        let mut heapless_buf: heapless::Vec<u8, 256> = heapless::Vec::new();
        heapless_buf
            .extend_from_slice(&buf)
            .map_err(|_| CaptureError::BufferTooSmall)?;

        // Parse header for debugging before attempting decryption
        let header_info = sframe::parse_header_info(&heapless_buf).map(|h| (h.kid, h.ctr));

        if let Err(e) = self.sframe.unprotect(&[], &mut heapless_buf) {
            return Err(CaptureError::DecryptionFailedDetail(e, header_info));
        }

        // Parse the chunk to extract audio data
        let parsed = chunk::parse_chunk(&heapless_buf).ok_or(CaptureError::InvalidChunk)?;

        let alaw_data =
            &heapless_buf[parsed.audio_offset..parsed.audio_offset + parsed.audio_length];

        // Decode A-law to PCM
        let mut pcm_samples = Vec::with_capacity(alaw_data.len());
        for &alaw_byte in alaw_data {
            pcm_samples.push(decode_alaw(alaw_byte));
        }

        // Write to sink
        sink.write_samples(&pcm_samples);

        Ok(true)
    }
}

/// Errors that can occur during audio capture.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptureError {
    /// Buffer too small for operation
    BufferTooSmall,
    /// SFrame decryption failed
    DecryptionFailed,
    /// SFrame decryption failed with detail (error, kid, ctr)
    DecryptionFailedDetail(crate::shared::sframe::Error, Option<(u64, u64)>),
    /// Invalid chunk format
    InvalidChunk,
}

impl core::fmt::Display for CaptureError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            CaptureError::BufferTooSmall => write!(f, "buffer too small"),
            CaptureError::DecryptionFailed => write!(f, "decryption failed"),
            CaptureError::DecryptionFailedDetail(e, Some((kid, ctr))) => {
                write!(f, "decryption failed: {:?} (kid={}, ctr={})", e, kid, ctr)
            }
            CaptureError::DecryptionFailedDetail(e, None) => {
                write!(f, "decryption failed: {:?} (no header)", e)
            }
            CaptureError::InvalidChunk => write!(f, "invalid chunk format"),
        }
    }
}

impl std::error::Error for CaptureError {}

/// Audio playback session.
///
/// Handles encoding and encrypting audio frames to send to the UI chip.
pub struct PlaybackSession {
    sframe: SFrameState,
}

impl PlaybackSession {
    /// Create a new playback session with the given SFrame key.
    ///
    /// The key should be the 16-byte SFrame key from the UI chip's EEPROM.
    pub fn new(sframe_key: &[u8; 16]) -> Self {
        Self {
            sframe: SFrameState::new(sframe_key, 0),
        }
    }

    /// Create an audio frame from PCM samples.
    ///
    /// The frame format is: channel_id (1 byte) + encrypted chunk
    /// (SFrame header + encrypted data + auth tag).
    ///
    /// Returns the frame ready to be sent as an AudioFrame TLV value.
    pub fn create_frame(
        &mut self,
        samples: &[i16],
        channel_id: u8,
    ) -> Result<Vec<u8>, PlaybackError> {
        use audio_codec_algorithms::encode_alaw;

        // Encode PCM to A-law
        let alaw_data: Vec<u8> = samples.iter().map(|&s| encode_alaw(s)).collect();

        // Build chunk (media format) - prepend header
        let mut chunk_buf: heapless::Vec<u8, 256> = heapless::Vec::new();
        chunk_buf
            .extend_from_slice(&alaw_data)
            .map_err(|_| PlaybackError::BufferTooSmall)?;
        chunk::prepend_media_header(&mut chunk_buf, false)
            .map_err(|_| PlaybackError::BufferTooSmall)?;

        // Encrypt with SFrame
        self.sframe
            .protect(&[], &mut chunk_buf)
            .map_err(|_| PlaybackError::EncryptionFailed)?;

        // Build final frame: channel_id + encrypted chunk
        let mut frame = Vec::with_capacity(1 + chunk_buf.len());
        frame.push(channel_id);
        frame.extend_from_slice(&chunk_buf);

        Ok(frame)
    }
}

/// Errors that can occur during audio playback.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlaybackError {
    /// Buffer too small for operation
    BufferTooSmall,
    /// SFrame encryption failed
    EncryptionFailed,
}

impl core::fmt::Display for PlaybackError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            PlaybackError::BufferTooSmall => write!(f, "buffer too small"),
            PlaybackError::EncryptionFailed => write!(f, "encryption failed"),
        }
    }
}

impl std::error::Error for PlaybackError {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shared::sframe::SFrameState;
    use audio_codec_algorithms::encode_alaw;

    /// Mock audio sink that records samples for verification.
    struct MockSink {
        samples: Vec<i16>,
    }

    impl MockSink {
        fn new() -> Self {
            Self {
                samples: Vec::new(),
            }
        }
    }

    impl AudioSink for MockSink {
        fn write_samples(&mut self, samples: &[i16]) {
            self.samples.extend_from_slice(samples);
        }
    }

    /// Helper to create a test frame (same format UI sends to CTL).
    fn create_test_frame(sframe: &mut SFrameState, pcm_samples: &[i16], channel_id: u8) -> Vec<u8> {
        // Encode PCM to A-law
        let alaw_data: Vec<u8> = pcm_samples.iter().map(|&s| encode_alaw(s)).collect();

        // Build chunk (media format)
        let mut chunk_buf: heapless::Vec<u8, 256> = heapless::Vec::new();
        chunk_buf.extend_from_slice(&alaw_data).unwrap();
        chunk::prepend_media_header(&mut chunk_buf, false).unwrap();

        // Encrypt with SFrame
        sframe.protect(&[], &mut chunk_buf).unwrap();

        // Build final frame: channel_id + encrypted chunk
        let mut frame = vec![channel_id];
        frame.extend_from_slice(&chunk_buf);
        frame
    }

    #[test]
    fn test_capture_session_roundtrip() {
        let key = [0xAA; 16];

        // Create matching SFrame states for sender and receiver
        let mut sender_sframe = SFrameState::new(&key, 0);
        let mut session = CaptureSession::new(&key);
        let mut sink = MockSink::new();

        // Create test samples
        let test_pcm: Vec<i16> = vec![1000, 2000, 3000, -1000, -2000, -3000];

        // Create encrypted frame
        let frame = create_test_frame(&mut sender_sframe, &test_pcm, 0);

        // Process frame
        let result = session.process_frame(&frame, &mut sink);
        assert!(result.is_ok());
        assert!(result.unwrap());

        // Verify samples were decoded (A-law is lossy, so we check approximate values)
        assert_eq!(sink.samples.len(), test_pcm.len());

        // A-law encoding is lossy but should preserve sign and approximate magnitude
        for (i, (&expected, &actual)) in test_pcm.iter().zip(sink.samples.iter()).enumerate() {
            let expected_decoded = decode_alaw(encode_alaw(expected));
            assert_eq!(
                actual, expected_decoded,
                "Sample {} mismatch: expected {} (from {}), got {}",
                i, expected_decoded, expected, actual
            );
        }
    }

    #[test]
    fn test_capture_session_empty_frame() {
        let key = [0xBB; 16];
        let mut session = CaptureSession::new(&key);
        let mut sink = MockSink::new();

        // Empty frame should return Ok(false)
        let result = session.process_frame(&[], &mut sink);
        assert_eq!(result, Ok(false));

        // Single byte frame should also return Ok(false)
        let result = session.process_frame(&[0x00], &mut sink);
        assert_eq!(result, Ok(false));
    }

    #[test]
    fn test_capture_session_invalid_decryption() {
        let key = [0xCC; 16];
        let wrong_key = [0xDD; 16];

        let mut sender_sframe = SFrameState::new(&key, 0);
        let mut session = CaptureSession::new(&wrong_key); // Wrong key!
        let mut sink = MockSink::new();

        let test_pcm: Vec<i16> = vec![1000, 2000, 3000];
        let frame = create_test_frame(&mut sender_sframe, &test_pcm, 0);

        // Should fail decryption
        let result = session.process_frame(&frame, &mut sink);
        assert!(matches!(
            result,
            Err(CaptureError::DecryptionFailedDetail(_, Some((0, 0))))
        ));
    }
}

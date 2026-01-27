//! Fixed jitter buffer for smoothing audio frame delivery.
//!
//! Buffers incoming frames and outputs them at a steady rate to absorb
//! network jitter.

extern crate alloc;
use alloc::vec::Vec;

/// Maximum number of frames the buffer can hold.
pub const BUFFER_FRAMES: usize = 32; // 640ms at 20ms/frame

/// Target buffer level before starting playback.
/// 5 frames = 100ms at 20ms/frame (matches HACTAR jitter buffer).
pub const MIN_START_LEVEL: usize = 5;

/// Jitter buffer state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub enum JitterState {
    /// Initial buffering - accumulating frames before playback starts.
    #[default]
    Buffering,
    /// Normal playback - outputting frames at steady rate.
    Playing,
}

/// Fixed jitter buffer for audio frames.
#[derive(Debug)]
pub struct JitterBuffer {
    /// Ring buffer of frames. None = empty slot.
    frames: [Option<Vec<u8>>; BUFFER_FRAMES],
    /// Next position to write incoming frame.
    write_idx: usize,
    /// Next position to read for playback.
    read_idx: usize,
    /// Current number of frames in buffer.
    level: usize,
    /// Buffer state.
    state: JitterState,
    /// Statistics: frames received.
    stats_received: u32,
    /// Statistics: frames output.
    stats_output: u32,
    /// Statistics: underruns (buffer empty when trying to read).
    stats_underruns: u32,
    /// Statistics: overruns (buffer full when trying to write).
    stats_overruns: u32,
}

/// Statistics from the jitter buffer.
#[derive(Debug, Clone, Default)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub struct JitterStats {
    /// Total frames received.
    pub received: u32,
    /// Total frames output.
    pub output: u32,
    /// Number of underruns (had to output silence).
    pub underruns: u32,
    /// Number of overruns (had to drop frames).
    pub overruns: u32,
    /// Current buffer level.
    pub level: usize,
    /// Current state.
    pub state: JitterState,
}

impl Default for JitterBuffer {
    fn default() -> Self {
        Self::new()
    }
}

impl JitterBuffer {
    /// Create a new empty jitter buffer.
    pub fn new() -> Self {
        Self {
            frames: core::array::from_fn(|_| None),
            write_idx: 0,
            read_idx: 0,
            level: 0,
            state: JitterState::Buffering,
            stats_received: 0,
            stats_output: 0,
            stats_underruns: 0,
            stats_overruns: 0,
        }
    }

    /// Reset the buffer to initial state.
    pub fn reset(&mut self) {
        for frame in &mut self.frames {
            *frame = None;
        }
        self.write_idx = 0;
        self.read_idx = 0;
        self.level = 0;
        self.state = JitterState::Buffering;
        self.stats_received = 0;
        self.stats_output = 0;
        self.stats_underruns = 0;
        self.stats_overruns = 0;
    }

    /// Push a frame into the buffer.
    ///
    /// Returns true if the frame was added, false if buffer was full (overrun).
    pub fn push(&mut self, data: &[u8]) -> bool {
        self.stats_received += 1;

        if self.level >= BUFFER_FRAMES {
            // Buffer full - overrun
            self.stats_overruns += 1;
            return false;
        }

        self.frames[self.write_idx] = Some(data.to_vec());
        self.write_idx = (self.write_idx + 1) % BUFFER_FRAMES;
        self.level += 1;

        true
    }

    /// Pop a frame from the buffer for playback.
    ///
    /// Call this at a steady rate (e.g., every 20ms).
    /// Returns None if buffer is empty or still buffering (play silence).
    /// Returns Some(frame) if a frame is available.
    pub fn pop(&mut self) -> Option<Vec<u8>> {
        match self.state {
            JitterState::Buffering => {
                if self.level >= MIN_START_LEVEL {
                    // Enough frames buffered, start playing
                    self.state = JitterState::Playing;
                    self.take_frame()
                } else {
                    // Still buffering
                    None
                }
            }
            JitterState::Playing => {
                if self.level > 0 {
                    self.take_frame()
                } else {
                    // Underrun - go back to buffering
                    self.stats_underruns += 1;
                    self.state = JitterState::Buffering;
                    None
                }
            }
        }
    }

    /// Take a frame from the read position.
    fn take_frame(&mut self) -> Option<Vec<u8>> {
        let frame = self.frames[self.read_idx].take();
        if frame.is_some() {
            self.read_idx = (self.read_idx + 1) % BUFFER_FRAMES;
            self.level = self.level.saturating_sub(1);
            self.stats_output += 1;
        }
        frame
    }

    /// Get current buffer level.
    pub fn level(&self) -> usize {
        self.level
    }

    /// Get current state.
    pub fn state(&self) -> JitterState {
        self.state
    }

    /// Get buffer statistics.
    pub fn stats(&self) -> JitterStats {
        JitterStats {
            received: self.stats_received,
            output: self.stats_output,
            underruns: self.stats_underruns,
            overruns: self.stats_overruns,
            level: self.level,
            state: self.state,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_initial_state() {
        let buf: JitterBuffer = JitterBuffer::new();
        assert_eq!(buf.level(), 0);
        assert_eq!(buf.state(), JitterState::Buffering);
    }

    #[test]
    fn test_buffering_until_min_level() {
        let mut buf: JitterBuffer = JitterBuffer::new();

        // Push frames but don't reach MIN_START_LEVEL
        for i in 0..MIN_START_LEVEL - 1 {
            assert!(buf.push(&[i as u8; 10]));
            assert_eq!(buf.state(), JitterState::Buffering);
            assert!(buf.pop().is_none()); // Should return None while buffering
        }

        // Push one more to reach MIN_START_LEVEL
        assert!(buf.push(&[99; 10]));

        // Now pop should transition to Playing and return a frame
        let frame = buf.pop();
        assert!(frame.is_some());
        assert_eq!(buf.state(), JitterState::Playing);
    }

    #[test]
    fn test_underrun_resets_to_buffering() {
        let mut buf: JitterBuffer = JitterBuffer::new();

        // Fill to MIN_START_LEVEL
        for i in 0..MIN_START_LEVEL {
            buf.push(&[i as u8; 10]);
        }

        // Drain all frames
        for _ in 0..MIN_START_LEVEL {
            assert!(buf.pop().is_some());
        }

        // Next pop should detect underrun
        assert!(buf.pop().is_none());
        assert_eq!(buf.state(), JitterState::Buffering);
        assert_eq!(buf.stats().underruns, 1);
    }

    #[test]
    fn test_overrun() {
        let mut buf: JitterBuffer = JitterBuffer::new();

        // Fill buffer completely
        for i in 0..BUFFER_FRAMES {
            assert!(buf.push(&[i as u8; 10]));
        }

        // Next push should fail (overrun)
        assert!(!buf.push(&[255; 10]));
        assert_eq!(buf.stats().overruns, 1);
    }

    #[test]
    fn test_fifo_order() {
        let mut buf: JitterBuffer = JitterBuffer::new();

        // Push MIN_START_LEVEL frames with distinct data
        for i in 0..MIN_START_LEVEL {
            buf.push(&[i as u8; 10]);
        }

        // Pop and verify order
        for i in 0..MIN_START_LEVEL {
            let frame = buf.pop().unwrap();
            assert_eq!(frame[0], i as u8);
        }
    }

    #[test]
    fn test_reset() {
        let mut buf: JitterBuffer = JitterBuffer::new();

        // Fill and start playing
        for i in 0..MIN_START_LEVEL {
            buf.push(&[i as u8; 10]);
        }
        buf.pop();
        assert_eq!(buf.state(), JitterState::Playing);

        // Reset should return to initial state
        buf.reset();
        assert_eq!(buf.level(), 0);
        assert_eq!(buf.state(), JitterState::Buffering);
        assert_eq!(buf.stats().received, 0);
    }
}

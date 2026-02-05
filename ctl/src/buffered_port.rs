//! BufferedPort - buffered reading for serial ports.
//!
//! This module provides `BufferedPort<P>`, a wrapper that provides buffered reading
//! for serial ports while passing writes directly through.

use serialport::SerialPort;
use std::io::{self, BufRead, Read, Write};
use std::time::Duration;

/// A wrapper that provides buffered reading for a serial port.
///
/// This struct provides buffered reading while passing writes directly through.
/// Serial ports already handle write buffering, so we only need read buffering
/// to allow peeking/parsing without losing data.
pub struct BufferedPort<P> {
    port: P,
    read_buf: Vec<u8>,
    read_pos: usize,
    read_cap: usize,
}

impl<P> BufferedPort<P> {
    const DEFAULT_BUF_SIZE: usize = 8192;

    /// Create a new BufferedPort wrapping the given serial port.
    pub fn new(port: P) -> Self {
        Self::with_capacity(Self::DEFAULT_BUF_SIZE, port)
    }

    /// Create a new BufferedPort with specified read buffer capacity.
    pub fn with_capacity(read_capacity: usize, port: P) -> Self {
        Self {
            port,
            read_buf: vec![0; read_capacity],
            read_pos: 0,
            read_cap: 0,
        }
    }

    /// Get a reference to the underlying port.
    pub fn get_ref(&self) -> &P {
        &self.port
    }

    /// Get a mutable reference to the underlying port.
    pub fn get_mut(&mut self) -> &mut P {
        &mut self.port
    }

    /// Consume the BufferedPort and return the underlying port.
    pub fn into_inner(self) -> P {
        self.port
    }
}

impl<P: Read> BufferedPort<P> {
    fn fill_buf_internal(&mut self) -> io::Result<&[u8]> {
        if self.read_pos >= self.read_cap {
            self.read_cap = self.port.read(&mut self.read_buf)?;
            self.read_pos = 0;
        }
        Ok(&self.read_buf[self.read_pos..self.read_cap])
    }
}

impl<P: Read> Read for BufferedPort<P> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        // If buffer is empty, fill it
        if self.read_pos >= self.read_cap {
            // For large reads, bypass the buffer
            if buf.len() >= self.read_buf.len() {
                return self.port.read(buf);
            }
            self.read_cap = self.port.read(&mut self.read_buf)?;
            self.read_pos = 0;
        }
        // Copy from buffer to output
        let available = self.read_cap - self.read_pos;
        let to_copy = available.min(buf.len());
        buf[..to_copy].copy_from_slice(&self.read_buf[self.read_pos..self.read_pos + to_copy]);
        self.read_pos += to_copy;
        Ok(to_copy)
    }
}

impl<P: Read> BufRead for BufferedPort<P> {
    fn fill_buf(&mut self) -> io::Result<&[u8]> {
        self.fill_buf_internal()
    }

    fn consume(&mut self, amt: usize) {
        self.read_pos = (self.read_pos + amt).min(self.read_cap);
    }
}

impl<P: Write> Write for BufferedPort<P> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        // For simplicity, write directly to port (no write buffering needed for serial)
        // The underlying serial port already handles write buffering
        self.port.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.port.flush()
    }
}

// Forward SerialPort methods when available
impl<P: SerialPort> BufferedPort<P> {
    /// Set the timeout for read operations.
    pub fn set_timeout(&mut self, timeout: Duration) -> serialport::Result<()> {
        self.port.set_timeout(timeout)
    }

    /// Get the current timeout.
    pub fn timeout(&self) -> Duration {
        self.port.timeout()
    }

    /// Set the baud rate.
    pub fn set_baud_rate(&mut self, baud_rate: u32) -> serialport::Result<()> {
        self.port.set_baud_rate(baud_rate)
    }

    /// Get the current baud rate.
    pub fn baud_rate(&self) -> serialport::Result<u32> {
        self.port.baud_rate()
    }
}

// ============================================================================
// Implement SetTimeout and SetBaudRate from link::ctl for flashing support
// ============================================================================

use link::ctl::{SetBaudRate, SetTimeout};

// Implement SetTimeout for BufferedPort wrapping Box<dyn SerialPort>
impl SetTimeout for BufferedPort<Box<dyn SerialPort>> {
    fn set_timeout(&mut self, timeout: Duration) -> std::io::Result<()> {
        self.port
            .set_timeout(timeout)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e.to_string()))
    }
}

// Implement SetBaudRate for BufferedPort wrapping Box<dyn SerialPort>
impl SetBaudRate for BufferedPort<Box<dyn SerialPort>> {
    fn set_baud_rate(&mut self, baud_rate: u32) -> std::io::Result<()> {
        self.port
            .set_baud_rate(baud_rate)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e.to_string()))
    }
}

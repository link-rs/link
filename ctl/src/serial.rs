//! Async serial port wrapper for tokio-serial.
//!
//! Provides `TokioSerialPort`, a wrapper around `tokio_serial::SerialStream` that adds:
//! - Internal read buffering
//! - Timeout support via `tokio::time::timeout()`
//! - Implementations of `SetTimeout`, `SetBaudRate`, and `clear_buffer()`

use link::ctl::{CtlPort, SetBaudRate, SetTimeout};
use std::collections::VecDeque;
use std::io;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::time::timeout;
use tokio_serial::{SerialPort, SerialStream};

/// Default read buffer size.
const DEFAULT_BUF_SIZE: usize = 8192;

/// Default read timeout.
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(3);

/// Async serial port wrapper with buffering and timeout support.
///
/// This wraps `tokio_serial::SerialStream` to provide:
/// - Internal read buffering for efficient byte-by-byte reads
/// - Configurable read timeout via `tokio::time::timeout()`
/// - `CtlPort`, `SetTimeout`, and `SetBaudRate` trait implementations
pub struct TokioSerialPort {
    stream: SerialStream,
    read_buffer: VecDeque<u8>,
    timeout: Duration,
}

impl TokioSerialPort {
    /// Create a new TokioSerialPort wrapping the given SerialStream.
    pub fn new(stream: SerialStream) -> Self {
        Self {
            stream,
            read_buffer: VecDeque::with_capacity(DEFAULT_BUF_SIZE),
            timeout: DEFAULT_TIMEOUT,
        }
    }

    /// Get a reference to the underlying SerialStream.
    pub fn get_ref(&self) -> &SerialStream {
        &self.stream
    }

    /// Get a mutable reference to the underlying SerialStream.
    pub fn get_mut(&mut self) -> &mut SerialStream {
        &mut self.stream
    }

    /// Consume the wrapper and return the underlying SerialStream.
    pub fn into_inner(self) -> SerialStream {
        self.stream
    }

    /// Fill the internal buffer by reading from the stream.
    async fn fill_buffer(&mut self) -> io::Result<usize> {
        let mut tmp = [0u8; 1024];
        let n = timeout(self.timeout, self.stream.read(&mut tmp))
            .await
            .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "read timeout"))??;
        self.read_buffer.extend(&tmp[..n]);
        Ok(n)
    }
}

impl CtlPort for TokioSerialPort {
    type Error = io::Error;

    async fn read(&mut self, buf: &mut [u8]) -> Result<usize, Self::Error> {
        // Return buffered data first
        if !self.read_buffer.is_empty() {
            let to_read = buf.len().min(self.read_buffer.len());
            for (i, byte) in self.read_buffer.drain(..to_read).enumerate() {
                buf[i] = byte;
            }
            return Ok(to_read);
        }

        // Buffer empty, read from stream with timeout
        let n = self.fill_buffer().await?;
        if n == 0 {
            return Ok(0);
        }

        // Return data from buffer
        let to_read = buf.len().min(self.read_buffer.len());
        for (i, byte) in self.read_buffer.drain(..to_read).enumerate() {
            buf[i] = byte;
        }
        Ok(to_read)
    }

    async fn write_all(&mut self, buf: &[u8]) -> Result<(), Self::Error> {
        AsyncWriteExt::write_all(&mut self.stream, buf).await
    }

    async fn flush(&mut self) -> Result<(), Self::Error> {
        AsyncWriteExt::flush(&mut self.stream).await
    }

    async fn read_exact(&mut self, buf: &mut [u8]) -> Result<(), Self::Error> {
        let mut filled = 0;
        while filled < buf.len() {
            // First drain from buffer
            while filled < buf.len() && !self.read_buffer.is_empty() {
                buf[filled] = self.read_buffer.pop_front().unwrap();
                filled += 1;
            }

            if filled < buf.len() {
                // Need more data
                let n = self.fill_buffer().await?;
                if n == 0 {
                    return Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "unexpected EOF in read_exact",
                    ));
                }
            }
        }
        Ok(())
    }

    fn clear_buffer(&mut self) {
        self.read_buffer.clear();
    }

    async fn drain_port(&mut self) {
        self.read_buffer.clear();
        // Use a short timeout for draining regardless of configured timeout
        let old_timeout = self.timeout;
        self.timeout = Duration::from_millis(50);
        let mut junk = [0u8; 256];
        loop {
            match <Self as CtlPort>::read(self, &mut junk).await {
                Ok(0) | Err(_) => break,
                Ok(_) => continue,
            }
        }
        self.timeout = old_timeout;
    }

    async fn write_dtr(&mut self, level: bool) -> Result<(), Self::Error> {
        self.stream
            .write_data_terminal_ready(level)
            .map_err(|e| io::Error::new(io::ErrorKind::Other, e.to_string()))
    }

    async fn write_rts(&mut self, level: bool) -> Result<(), Self::Error> {
        self.stream
            .write_request_to_send(level)
            .map_err(|e| io::Error::new(io::ErrorKind::Other, e.to_string()))
    }

    fn supports_dtr_rts(&self) -> bool {
        true
    }
}

impl SetTimeout for TokioSerialPort {
    fn set_timeout(&mut self, timeout: Duration) -> io::Result<()> {
        self.timeout = timeout;
        Ok(())
    }
}

impl SetBaudRate for TokioSerialPort {
    async fn set_baud_rate(&mut self, baud_rate: u32) -> io::Result<()> {
        self.stream
            .set_baud_rate(baud_rate)
            .map_err(|e| io::Error::new(io::ErrorKind::Other, e.to_string()))
    }
}


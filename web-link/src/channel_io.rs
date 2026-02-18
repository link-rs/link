//! Channel-based I/O for inter-chip communication.

#![allow(dead_code)]

use async_channel::{Receiver, Sender};
use embedded_io_async::{ErrorType, Read, Write};
use std::collections::VecDeque;

/// Error type for channel I/O.
#[derive(Debug, Clone)]
pub enum ChannelError {
    Closed,
}

impl core::fmt::Display for ChannelError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            ChannelError::Closed => write!(f, "Channel closed"),
        }
    }
}

impl core::error::Error for ChannelError {}

impl embedded_io_async::Error for ChannelError {
    fn kind(&self) -> embedded_io_async::ErrorKind {
        embedded_io_async::ErrorKind::Other
    }
}

/// Channel-based reader implementing embedded_io_async::Read.
pub struct ChannelReader {
    rx: Receiver<Vec<u8>>,
    buffer: VecDeque<u8>,
}

impl ChannelReader {
    pub fn new(rx: Receiver<Vec<u8>>) -> Self {
        Self {
            rx,
            buffer: VecDeque::new(),
        }
    }
}

impl ErrorType for ChannelReader {
    type Error = ChannelError;
}

impl Read for ChannelReader {
    async fn read(&mut self, buf: &mut [u8]) -> Result<usize, Self::Error> {
        // If buffer has data, return from it first
        if !self.buffer.is_empty() {
            let to_read = std::cmp::min(buf.len(), self.buffer.len());
            for (i, byte) in self.buffer.drain(..to_read).enumerate() {
                buf[i] = byte;
            }
            return Ok(to_read);
        }

        // Wait for more data
        match self.rx.recv().await {
            Ok(data) => {
                self.buffer.extend(data);
                let to_read = std::cmp::min(buf.len(), self.buffer.len());
                for (i, byte) in self.buffer.drain(..to_read).enumerate() {
                    buf[i] = byte;
                }
                Ok(to_read)
            }
            Err(_) => Err(ChannelError::Closed),
        }
    }
}

/// Channel-based writer implementing embedded_io_async::Write.
pub struct ChannelWriter {
    tx: Sender<Vec<u8>>,
}

impl ChannelWriter {
    pub fn new(tx: Sender<Vec<u8>>) -> Self {
        Self { tx }
    }
}

impl ErrorType for ChannelWriter {
    type Error = ChannelError;
}

impl Write for ChannelWriter {
    async fn write(&mut self, buf: &[u8]) -> Result<usize, Self::Error> {
        self.tx
            .send(buf.to_vec())
            .await
            .map_err(|_| ChannelError::Closed)?;
        Ok(buf.len())
    }

    async fn flush(&mut self) -> Result<(), Self::Error> {
        Ok(())
    }
}

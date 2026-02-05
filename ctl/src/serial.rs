//! Async serial port wrapper for tokio-serial.

use link::ctl::CtlPort;
use std::io;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_serial::SerialStream;

/// CtlPort implementation for tokio-serial's SerialStream.
impl CtlPort for SerialStream {
    type Error = io::Error;

    async fn read(&mut self, buf: &mut [u8]) -> Result<usize, Self::Error> {
        AsyncReadExt::read(self, buf).await
    }

    async fn write_all(&mut self, buf: &[u8]) -> Result<(), Self::Error> {
        AsyncWriteExt::write_all(self, buf).await
    }

    async fn flush(&mut self) -> Result<(), Self::Error> {
        AsyncWriteExt::flush(self).await
    }

    async fn read_exact(&mut self, buf: &mut [u8]) -> Result<(), Self::Error> {
        AsyncReadExt::read_exact(self, buf).await?;
        Ok(())
    }
}

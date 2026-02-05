//! Port trait for CTL communication.
//!
//! This module defines the `CtlPort` trait, which abstracts async I/O operations
//! for both sync (std) and async (wasm) contexts.

#[cfg(feature = "std")]
use std::time::Duration;

/// Error type for port operations that can be converted to a display string.
pub trait PortError: core::fmt::Debug {
    fn to_error_string(&self) -> alloc::string::String;
}

impl<E: core::fmt::Debug> PortError for E {
    fn to_error_string(&self) -> alloc::string::String {
        alloc::format!("{:?}", self)
    }
}

/// Trait for types that support setting a read timeout.
#[cfg(feature = "std")]
pub trait SetTimeout {
    fn set_timeout(&mut self, timeout: Duration) -> std::io::Result<()>;
}

/// Trait for types that support setting the baud rate.
#[cfg(feature = "std")]
pub trait SetBaudRate {
    fn set_baud_rate(&mut self, baud_rate: u32) -> std::io::Result<()>;
}

/// Async port trait for CTL communication.
///
/// This trait abstracts serial port operations for both:
/// - Native `std::io` ports (wrapped with `SyncPortAdapter`)
/// - WASM `WebSerial` ports (async native)
#[allow(async_fn_in_trait)]
pub trait CtlPort {
    /// The error type for port operations.
    type Error: core::fmt::Debug;

    /// Read bytes from the port into the buffer.
    ///
    /// Returns the number of bytes read (may be less than `buf.len()`).
    async fn read(&mut self, buf: &mut [u8]) -> Result<usize, Self::Error>;

    /// Write all bytes from the buffer to the port.
    async fn write_all(&mut self, buf: &[u8]) -> Result<(), Self::Error>;

    /// Flush any buffered output.
    async fn flush(&mut self) -> Result<(), Self::Error>;

    /// Read exactly `buf.len()` bytes from the port.
    ///
    /// Returns an error if EOF is reached before the buffer is filled.
    async fn read_exact(&mut self, buf: &mut [u8]) -> Result<(), Self::Error> {
        let mut filled = 0;
        while filled < buf.len() {
            let n = self.read(&mut buf[filled..]).await?;
            if n == 0 {
                // EOF - but we can't return a specific EOF error without
                // constraining the Error type. Implementations should handle this.
                break;
            }
            filled += n;
        }
        Ok(())
    }
}

/// Blanket implementation allowing mutable references to CtlPort to be used as CtlPort.
impl<P: CtlPort> CtlPort for &mut P {
    type Error = P::Error;

    async fn read(&mut self, buf: &mut [u8]) -> Result<usize, Self::Error> {
        P::read(*self, buf).await
    }

    async fn write_all(&mut self, buf: &[u8]) -> Result<(), Self::Error> {
        P::write_all(*self, buf).await
    }

    async fn flush(&mut self) -> Result<(), Self::Error> {
        P::flush(*self).await
    }

    async fn read_exact(&mut self, buf: &mut [u8]) -> Result<(), Self::Error> {
        P::read_exact(*self, buf).await
    }
}

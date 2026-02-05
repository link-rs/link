//! Sync-to-async port adapter for CLI usage.
//!
//! This module provides `SyncPortAdapter<T>` for using synchronous I/O with async `CtlCore`.

use crate::buffered_port::BufferedPort;
use link::ctl::{CtlPort, SetBaudRate, SetTimeout};
use std::io::{Read, Write};
use std::time::Duration;

/// Trait for types that support clearing their read buffer.
pub trait ClearBuffer {
    fn clear_buffer(&mut self);
}

// Implement ClearBuffer for BufferedPort
impl<P> ClearBuffer for BufferedPort<P> {
    fn clear_buffer(&mut self) {
        BufferedPort::clear_buffer(self)
    }
}

/// A wrapper that adapts sync I/O to the async CtlPort trait.
///
/// This allows using standard serial ports (which use blocking I/O)
/// with the async `CtlCore`. The async methods execute synchronously,
/// which is appropriate when using `futures::executor::block_on()`.
pub struct SyncPortAdapter<P> {
    port: Option<P>,
}

impl<P> SyncPortAdapter<P> {
    /// Create a new SyncPortAdapter wrapping the given port.
    pub fn new(port: P) -> Self {
        Self { port: Some(port) }
    }

    /// Get a reference to the underlying port.
    ///
    /// # Panics
    /// Panics if the port has been taken and not yet returned.
    pub fn get_ref(&self) -> &P {
        self.port.as_ref().expect("port has been taken")
    }

    /// Get a mutable reference to the underlying port.
    ///
    /// # Panics
    /// Panics if the port has been taken and not yet returned.
    pub fn get_mut(&mut self) -> &mut P {
        self.port.as_mut().expect("port has been taken")
    }

    /// Consume the adapter and return the underlying port.
    ///
    /// # Panics
    /// Panics if the port has been taken and not yet returned.
    pub fn into_inner(self) -> P {
        self.port.expect("port has been taken")
    }

    /// Temporarily take the port out for exclusive use.
    ///
    /// The port must be returned via `put_port()` before using any CtlCore methods.
    ///
    /// # Panics
    /// Panics if the port has already been taken.
    pub fn take_port(&mut self) -> P {
        self.port.take().expect("port has already been taken")
    }

    /// Return a previously taken port.
    ///
    /// # Panics
    /// Panics if a port is already present (port wasn't taken, or was returned twice).
    pub fn put_port(&mut self, port: P) {
        if self.port.is_some() {
            panic!("port is already present");
        }
        self.port = Some(port);
    }
}

impl<P: Read + Write + ClearBuffer> CtlPort for SyncPortAdapter<P> {
    type Error = std::io::Error;

    async fn read(&mut self, buf: &mut [u8]) -> Result<usize, Self::Error> {
        // Synchronous read executed in async context
        Read::read(self.get_mut(), buf)
    }

    async fn write_all(&mut self, buf: &[u8]) -> Result<(), Self::Error> {
        Write::write_all(self.get_mut(), buf)
    }

    async fn flush(&mut self) -> Result<(), Self::Error> {
        Write::flush(self.get_mut())
    }

    async fn read_exact(&mut self, buf: &mut [u8]) -> Result<(), Self::Error> {
        Read::read_exact(self.get_mut(), buf)
    }

    fn clear_buffer(&mut self) {
        self.get_mut().clear_buffer()
    }
}

// Implement std::io::Read for SyncPortAdapter to support flashing operations
impl<P: Read> Read for SyncPortAdapter<P> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.get_mut().read(buf)
    }
}

// Implement std::io::Write for SyncPortAdapter to support flashing operations
impl<P: Write> Write for SyncPortAdapter<P> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.get_mut().write(buf)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.get_mut().flush()
    }
}

// Implement SetTimeout for SyncPortAdapter to support flashing operations
impl<P: SetTimeout> SetTimeout for SyncPortAdapter<P> {
    fn set_timeout(&mut self, timeout: Duration) -> std::io::Result<()> {
        self.get_mut().set_timeout(timeout)
    }
}

// Implement SetBaudRate for SyncPortAdapter to support flashing operations
impl<P: SetBaudRate> SetBaudRate for SyncPortAdapter<P> {
    fn set_baud_rate(&mut self, baud_rate: u32) -> std::io::Result<()> {
        self.get_mut().set_baud_rate(baud_rate)
    }
}

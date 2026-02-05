//! WebSerial transport for embedded-io-async.

use embedded_io_async::{ErrorType, Read, Write};
use js_sys::{Object, Reflect, Uint8Array};
use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;
use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::JsFuture;
use web_sys::{ReadableStreamDefaultReader, SerialPort, WritableStreamDefaultWriter};

/// Error type for WebSerial operations.
#[derive(Debug, Clone)]
pub enum WebSerialError {
    /// JavaScript error occurred.
    JsError(String),
    /// Port is not open.
    NotConnected,
    /// Read operation failed.
    ReadError(String),
    /// Write operation failed.
    WriteError(String),
}

impl core::fmt::Display for WebSerialError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            WebSerialError::JsError(s) => write!(f, "JS error: {}", s),
            WebSerialError::NotConnected => write!(f, "Serial port not connected"),
            WebSerialError::ReadError(s) => write!(f, "Read error: {}", s),
            WebSerialError::WriteError(s) => write!(f, "Write error: {}", s),
        }
    }
}

impl embedded_io_async::Error for WebSerialError {
    fn kind(&self) -> embedded_io_async::ErrorKind {
        embedded_io_async::ErrorKind::Other
    }
}

/// Internal state for the WebSerial connection.
struct WebSerialState {
    port: SerialPort,
    reader: ReadableStreamDefaultReader,
    writer: WritableStreamDefaultWriter,
    read_buffer: VecDeque<u8>,
}

/// WebSerial transport implementing embedded-io-async traits.
pub struct WebSerial {
    state: Rc<RefCell<Option<WebSerialState>>>,
}

impl WebSerial {
    /// Create a new WebSerial transport (not yet connected).
    pub fn new() -> Self {
        Self {
            state: Rc::new(RefCell::new(None)),
        }
    }

    /// Request a serial port from the user and connect to it.
    pub async fn connect(&self, baud_rate: u32) -> Result<(), WebSerialError> {
        let window = web_sys::window().ok_or(WebSerialError::JsError("No window".into()))?;
        let navigator = window.navigator();

        // Get the Serial API
        let serial = Reflect::get(&navigator, &JsValue::from_str("serial"))
            .map_err(|e| WebSerialError::JsError(format!("{:?}", e)))?;

        if serial.is_undefined() {
            return Err(WebSerialError::JsError(
                "WebSerial API not available. Use Chrome/Edge with HTTPS or localhost.".into(),
            ));
        }

        // Request a port from the user
        let request_port = Reflect::get(&serial, &JsValue::from_str("requestPort"))
            .map_err(|e| WebSerialError::JsError(format!("{:?}", e)))?;
        let request_port_fn: js_sys::Function = request_port.into();

        let port_promise = request_port_fn
            .call0(&serial)
            .map_err(|e| WebSerialError::JsError(format!("{:?}", e)))?;

        let port: SerialPort = JsFuture::from(js_sys::Promise::from(port_promise))
            .await
            .map_err(|e| WebSerialError::JsError(format!("{:?}", e)))?
            .into();

        // Open the port with specified baud rate and even parity (required by Link)
        let options = web_sys::SerialOptions::new(baud_rate);
        options.set_parity(web_sys::ParityType::Even);
        JsFuture::from(port.open(&options))
            .await
            .map_err(|e| WebSerialError::JsError(format!("Failed to open port: {:?}", e)))?;

        // Get reader and writer
        let readable = port.readable();
        let writable = port.writable();

        let reader: ReadableStreamDefaultReader = readable.get_reader().unchecked_into();
        let writer: WritableStreamDefaultWriter = writable
            .get_writer()
            .map_err(|e| WebSerialError::JsError(format!("Failed to get writer: {:?}", e)))?;

        *self.state.borrow_mut() = Some(WebSerialState {
            port,
            reader,
            writer,
            read_buffer: VecDeque::new(),
        });

        Ok(())
    }

    /// Check if connected to a serial port.
    pub fn is_connected(&self) -> bool {
        self.state.borrow().is_some()
    }

    /// Clear the internal read buffer.
    ///
    /// This discards any data that has been read from the serial port but not yet
    /// consumed. Useful before flashing operations where stale data might interfere.
    pub fn clear_read_buffer(&self) {
        if let Some(state) = self.state.borrow_mut().as_mut() {
            state.read_buffer.clear();
        }
    }

    /// Disconnect from the serial port.
    pub async fn disconnect(&self) -> Result<(), WebSerialError> {
        let state = self.state.borrow_mut().take();
        if let Some(state) = state {
            // Release the reader and writer locks
            state.reader.release_lock();
            state.writer.release_lock();

            // Close the port
            JsFuture::from(state.port.close())
                .await
                .map_err(|e| WebSerialError::JsError(format!("Failed to close port: {:?}", e)))?;
        }
        Ok(())
    }

    /// Internal: read more data from the port into the buffer.
    async fn fill_buffer(&self) -> Result<(), WebSerialError> {
        let reader = {
            let state = self.state.borrow();
            let state = state.as_ref().ok_or(WebSerialError::NotConnected)?;
            state.reader.clone()
        };

        let result = JsFuture::from(reader.read())
            .await
            .map_err(|e| WebSerialError::ReadError(format!("{:?}", e)))?;

        let result: Object = result.into();
        let done = Reflect::get(&result, &JsValue::from_str("done"))
            .map_err(|e| WebSerialError::ReadError(format!("{:?}", e)))?;

        if done.as_bool().unwrap_or(true) {
            return Err(WebSerialError::ReadError("Stream ended".into()));
        }

        let value = Reflect::get(&result, &JsValue::from_str("value"))
            .map_err(|e| WebSerialError::ReadError(format!("{:?}", e)))?;

        let array: Uint8Array = value.into();
        let data = array.to_vec();

        let mut state = self.state.borrow_mut();
        let state = state.as_mut().ok_or(WebSerialError::NotConnected)?;
        state.read_buffer.extend(data);

        Ok(())
    }
}

impl Default for WebSerial {
    fn default() -> Self {
        Self::new()
    }
}

impl Clone for WebSerial {
    fn clone(&self) -> Self {
        Self {
            state: self.state.clone(),
        }
    }
}

impl ErrorType for WebSerial {
    type Error = WebSerialError;
}

impl Read for WebSerial {
    async fn read(&mut self, buf: &mut [u8]) -> Result<usize, Self::Error> {
        // First try to read from buffer
        {
            let mut state = self.state.borrow_mut();
            let state = state.as_mut().ok_or(WebSerialError::NotConnected)?;

            if !state.read_buffer.is_empty() {
                let to_read = std::cmp::min(buf.len(), state.read_buffer.len());
                for (i, byte) in state.read_buffer.drain(..to_read).enumerate() {
                    buf[i] = byte;
                }
                return Ok(to_read);
            }
        }

        // Buffer empty, read more from port
        self.fill_buffer().await?;

        // Now read from buffer
        let mut state = self.state.borrow_mut();
        let state = state.as_mut().ok_or(WebSerialError::NotConnected)?;

        let to_read = std::cmp::min(buf.len(), state.read_buffer.len());
        for (i, byte) in state.read_buffer.drain(..to_read).enumerate() {
            buf[i] = byte;
        }
        Ok(to_read)
    }
}

impl Write for WebSerial {
    async fn write(&mut self, buf: &[u8]) -> Result<usize, Self::Error> {
        let writer = {
            let state = self.state.borrow();
            let state = state.as_ref().ok_or(WebSerialError::NotConnected)?;
            state.writer.clone()
        };

        let array = Uint8Array::from(buf);
        JsFuture::from(writer.write_with_chunk(&array))
            .await
            .map_err(|e| WebSerialError::WriteError(format!("{:?}", e)))?;

        Ok(buf.len())
    }

    async fn flush(&mut self) -> Result<(), Self::Error> {
        // WebSerial doesn't have an explicit flush
        Ok(())
    }
}

// ============================================================================
// CtlPort implementation for WebSerial
// ============================================================================

impl link::ctl::CtlPort for WebSerial {
    type Error = WebSerialError;

    async fn read(&mut self, buf: &mut [u8]) -> Result<usize, Self::Error> {
        // Use the embedded_io_async::Read implementation
        <Self as Read>::read(self, buf).await
    }

    async fn write_all(&mut self, buf: &[u8]) -> Result<(), Self::Error> {
        // Use embedded_io_async::Write implementation, looping until all bytes written
        let mut written = 0;
        while written < buf.len() {
            let n = <Self as Write>::write(self, &buf[written..]).await?;
            written += n;
        }
        Ok(())
    }

    async fn flush(&mut self) -> Result<(), Self::Error> {
        <Self as Write>::flush(self).await
    }

    async fn read_exact(&mut self, buf: &mut [u8]) -> Result<(), Self::Error> {
        let mut filled = 0;
        while filled < buf.len() {
            let n = <Self as Read>::read(self, &mut buf[filled..]).await?;
            if n == 0 {
                return Err(WebSerialError::ReadError("Unexpected EOF".into()));
            }
            filled += n;
        }
        Ok(())
    }
}

// ============================================================================
// WebSerialAdapter - wraps WebSerial with std::io::Error for flash operations
// ============================================================================

/// Adapter that wraps WebSerial to use std::io::Error instead of WebSerialError.
///
/// This is needed because the CtlCore flash methods require CtlPort<Error = std::io::Error>.
pub struct WebSerialAdapter {
    inner: WebSerial,
}

impl WebSerialAdapter {
    /// Create a new adapter wrapping a WebSerial.
    pub fn new(inner: WebSerial) -> Self {
        Self { inner }
    }

    /// Consume the adapter and return the inner WebSerial.
    pub fn into_inner(self) -> WebSerial {
        self.inner
    }
}

impl From<WebSerialError> for std::io::Error {
    fn from(e: WebSerialError) -> Self {
        std::io::Error::new(std::io::ErrorKind::Other, e.to_string())
    }
}

impl link::ctl::CtlPort for WebSerialAdapter {
    type Error = std::io::Error;

    async fn read(&mut self, buf: &mut [u8]) -> Result<usize, Self::Error> {
        <WebSerial as link::ctl::CtlPort>::read(&mut self.inner, buf).await.map_err(Into::into)
    }

    async fn write_all(&mut self, buf: &[u8]) -> Result<(), Self::Error> {
        link::ctl::CtlPort::write_all(&mut self.inner, buf).await.map_err(Into::into)
    }

    async fn flush(&mut self) -> Result<(), Self::Error> {
        link::ctl::CtlPort::flush(&mut self.inner).await.map_err(Into::into)
    }

    async fn read_exact(&mut self, buf: &mut [u8]) -> Result<(), Self::Error> {
        link::ctl::CtlPort::read_exact(&mut self.inner, buf).await.map_err(Into::into)
    }

    fn clear_buffer(&mut self) {
        self.inner.clear_read_buffer();
    }
}

impl link::ctl::SetTimeout for WebSerialAdapter {
    fn set_timeout(&mut self, _timeout: std::time::Duration) -> std::io::Result<()> {
        // WebSerial doesn't support timeouts directly - they're handled by the browser.
        // The espflash code uses timeouts for flow control, but on WASM we rely on
        // the browser's built-in timeout handling.
        Ok(())
    }
}

impl link::ctl::SetBaudRate for WebSerialAdapter {
    fn set_baud_rate(&mut self, _baud_rate: u32) -> std::io::Result<()> {
        // WebSerial requires closing and reopening the port to change baud rate.
        // For flashing, we stay at the initial baud rate (115200).
        // The TunnelSerialInterface handles MGMT-NET baud rate via TLV commands.
        Ok(())
    }
}

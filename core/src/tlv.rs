use embedded_io_async::{Read, ReadExactError, Write};
use heapless::Vec;
use num_enum::{IntoPrimitive, TryFromPrimitive};

// Verbose TLV tracing - enable with `trace-tlv` feature, easy to disable
#[cfg(feature = "trace-tlv")]
macro_rules! trace {
    ($($arg:tt)*) => { defmt::debug!($($arg)*) };
}

#[cfg(not(feature = "trace-tlv"))]
macro_rules! trace {
    ($($arg:tt)*) => {};
}

// Trace IO errors with the error value when trace-tlv is enabled
#[cfg(feature = "trace-tlv")]
macro_rules! trace_io_error {
    ($label:expr, $msg:expr, $err:expr) => {
        defmt::debug!("{}: {} {:?}", $label, $msg, defmt::Debug2Format(&$err))
    };
}

#[cfg(not(feature = "trace-tlv"))]
macro_rules! trace_io_error {
    ($label:expr, $msg:expr, $err:expr) => {
        let _ = &$err; // suppress unused warning
    };
}

pub type Type = u16;
pub type Length = u32;
pub type Value = Vec<u8, MAX_VALUE_SIZE>;

pub const HEADER_SIZE: usize = core::mem::size_of::<Type>() + core::mem::size_of::<Length>();
pub type Header = [u8; HEADER_SIZE];

pub const MAX_VALUE_SIZE: usize = 32;
pub const MAX_TLV_SIZE: usize = HEADER_SIZE + MAX_VALUE_SIZE;
pub type TlvVec = Vec<u8, MAX_TLV_SIZE>;

#[derive(Copy, Clone, PartialEq, Eq, Debug, IntoPrimitive, TryFromPrimitive)]
#[repr(u16)]
pub enum CtlToMgmt {
    Ping = 0x00,
    ToUi,
    ToNet,
    UiFirstCircularPing,
    NetFirstCircularPing,
}

#[derive(Copy, Clone, PartialEq, Eq, Debug, IntoPrimitive, TryFromPrimitive)]
#[repr(u16)]
pub enum MgmtToCtl {
    Pong = 0x10,
    FromUi,
    FromNet,
    UiFirstCircularPing,
    NetFirstCircularPing,
}

#[derive(Copy, Clone, PartialEq, Eq, Debug, IntoPrimitive, TryFromPrimitive)]
#[repr(u16)]
pub enum MgmtToUi {
    Ping = 0x20,
    CircularPing,
}

#[derive(Copy, Clone, PartialEq, Eq, Debug, IntoPrimitive, TryFromPrimitive)]
#[repr(u16)]
pub enum UiToMgmt {
    Pong = 0x30,
    CircularPing,
}

#[derive(Copy, Clone, PartialEq, Eq, Debug, IntoPrimitive, TryFromPrimitive)]
#[repr(u16)]
pub enum MgmtToNet {
    Ping = 0x40,
    CircularPing,
}

#[derive(Copy, Clone, PartialEq, Eq, Debug, IntoPrimitive, TryFromPrimitive)]
#[repr(u16)]
pub enum NetToMgmt {
    Pong = 0x50,
    CircularPing,
}

#[derive(Copy, Clone, PartialEq, Eq, Debug, IntoPrimitive, TryFromPrimitive)]
#[repr(u16)]
pub enum UiToNet {
    CircularPing = 0x60,
}

#[derive(Copy, Clone, PartialEq, Eq, Debug, IntoPrimitive, TryFromPrimitive)]
#[repr(u16)]
pub enum NetToUi {
    CircularPing = 0x70,
}

fn decode_header<T: TryFrom<u16>>(header: &Header) -> Result<(T, usize), T::Error> {
    let raw_type = Type::from_be_bytes([header[0], header[1]]);
    let tlv_type = T::try_from(raw_type)?;
    let length = Length::from_be_bytes([header[2], header[3], header[4], header[5]]);
    Ok((tlv_type, length as usize))
}

fn encode_header(tlv_type: impl Into<u16>, length: usize) -> Header {
    let mut header = Header::default();
    let type_val: Type = tlv_type.into();
    let length_val: Length = length as Length;
    header[0..2].copy_from_slice(&type_val.to_be_bytes());
    header[2..6].copy_from_slice(&length_val.to_be_bytes());
    header
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tlv<T> {
    pub tlv_type: T,
    pub value: Value,
}

impl<T> Tlv<T> {
    pub const fn new(tlv_type: T, value: Value) -> Self {
        Self { tlv_type, value }
    }

    pub async fn encode(tlv_type: T, value: &[u8]) -> TlvVec
    where
        T: Into<u16>,
    {
        let mut enc = TlvVec::new();
        enc.resize(HEADER_SIZE + value.len(), 0).unwrap();
        enc.as_mut_slice().write_tlv(tlv_type, value).await.unwrap();
        enc
    }
}

#[derive(Debug)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub enum ReadError<E> {
    Io(E),
    InvalidType,
    TooLong,
}

pub trait ReadTlv {
    type Error: core::fmt::Debug;

    async fn read_tlv<T: TryFrom<u16>>(&mut self) -> Result<Option<Tlv<T>>, Self::Error>;

    async fn must_read_tlv<T: TryFrom<u16>>(&mut self) -> Tlv<T> {
        self.read_tlv().await.unwrap().unwrap()
    }
}

/// A labeled TLV reader that includes a label in all trace output.
pub struct LabeledReader<R> {
    #[allow(dead_code)] // Used by trace! macro when trace-tlv feature is enabled
    label: &'static str,
    reader: R,
}

impl<R> LabeledReader<R> {
    pub fn new(label: &'static str, reader: R) -> Self {
        Self { label, reader }
    }
}

impl<R> ReadTlv for LabeledReader<R>
where
    R: Read,
{
    type Error = ReadError<R::Error>;

    async fn read_tlv<T: TryFrom<u16>>(&mut self) -> Result<Option<Tlv<T>>, Self::Error> {
        trace!("{}: waiting for header ({} bytes)", self.label, HEADER_SIZE);

        let mut header = [0u8; HEADER_SIZE];
        for i in 0..HEADER_SIZE {
            let mut done = false;
            while !done {
                match self.reader.read_exact(&mut header[i..i + 1]).await {
                    Ok(()) => {
                        trace!("{}: header[{}] = {:#04x}", self.label, i, header[i]);
                        done = true;
                    }
                    _ => {
                        // Read past errors
                    } /*
                      Err(ReadExactError::UnexpectedEof) => {
                          trace!("{}: unexpected EOF at header[{}]", self.label, i);
                          return Ok(None);
                      }
                      Err(ReadExactError::Other(e)) => {
                          trace_io_error!(self.label, "IO error at header", e);
                          return Err(ReadError::Io(e));
                      }
                      */
                }
            }
        }

        trace!("{}: header complete: {=[u8]:02x}", self.label, header);

        let (tlv_type, length) = match decode_header(&header) {
            Ok(v) => v,
            Err(_) => {
                trace!("{}: invalid type in header", self.label);
                return Err(ReadError::InvalidType);
            }
        };

        trace!(
            "{}: type={:#06x}, length={:#x}",
            self.label,
            u16::from_be_bytes([header[0], header[1]]),
            length
        );

        let mut value = Value::new();
        if value.resize(length, 0).is_err() {
            trace!("{}: value too long ({:#x})", self.label, length);
            return Err(ReadError::TooLong);
        }

        for i in 0..length {
            match self.reader.read_exact(&mut value[i..i + 1]).await {
                Ok(()) => {
                    trace!("{}: value[{}] = {:#04x}", self.label, i, value[i]);
                }
                Err(ReadExactError::UnexpectedEof) => {
                    trace!("{}: unexpected EOF at value[{}]", self.label, i);
                    return Ok(None);
                }
                Err(ReadExactError::Other(e)) => {
                    trace_io_error!(self.label, "IO error at value", e);
                    return Err(ReadError::Io(e));
                }
            }
        }

        trace!(
            "{}: complete, value={=[u8]:02x}",
            self.label,
            value.as_slice()
        );

        Ok(Some(Tlv { tlv_type, value }))
    }
}

pub trait WriteTlv {
    type Error: core::fmt::Debug;

    async fn write_tlv<T: Into<u16>>(
        &mut self,
        tlv_type: T,
        value: &[u8],
    ) -> Result<(), Self::Error>;

    async fn must_write_tlv<T: Into<u16>>(&mut self, tlv_type: T, value: &[u8]) {
        self.write_tlv(tlv_type, value).await.unwrap()
    }
}

/// A labeled TLV writer that includes a label in all trace output.
pub struct LabeledWriter<W> {
    #[allow(dead_code)] // Used by trace! macro when trace-tlv feature is enabled
    label: &'static str,
    writer: W,
}

impl<W> LabeledWriter<W> {
    pub fn new(label: &'static str, writer: W) -> Self {
        Self { label, writer }
    }
}

impl<W> embedded_io_async::ErrorType for LabeledWriter<W>
where
    W: Write,
{
    type Error = W::Error;
}

impl<W> Write for LabeledWriter<W>
where
    W: Write,
{
    async fn write(&mut self, buf: &[u8]) -> Result<usize, Self::Error> {
        self.writer.write(buf).await
    }

    async fn flush(&mut self) -> Result<(), Self::Error> {
        self.writer.flush().await
    }
}

impl<W> WriteTlv for LabeledWriter<W>
where
    W: Write,
{
    type Error = W::Error;

    async fn write_tlv<T: Into<u16>>(&mut self, tlv_type: T, value: &[u8]) -> Result<(), W::Error> {
        let type_val: u16 = tlv_type.into();
        trace!(
            "{}: write type={:#06x}, length={:#x}",
            self.label,
            type_val,
            value.len()
        );
        let header = encode_header(type_val, value.len());
        self.writer.write_all(&header).await?;
        self.writer.write_all(&value).await?;
        self.writer.flush().await?;
        trace!("{}: write complete", self.label);
        Ok(())
    }
}

/// A guarded TLV reader that waits for a signal before reading.
/// Used to synchronize UART communication between chips.
pub struct GuardedReadTlv<S, R> {
    #[allow(dead_code)] // Used by trace! macro when trace-tlv feature is enabled
    label: &'static str,
    signal: S,
    reader: LabeledReader<R>,
}

impl<S, R> GuardedReadTlv<S, R> {
    pub fn new(label: &'static str, signal: S, reader: R) -> Self {
        Self {
            label,
            signal,
            reader: LabeledReader::new(label, reader),
        }
    }
}

impl<S, R> ReadTlv for GuardedReadTlv<S, R>
where
    S: embedded_hal_async::digital::Wait,
    R: Read,
{
    type Error = ReadError<R::Error>;

    async fn read_tlv<T: TryFrom<u16>>(&mut self) -> Result<Option<Tlv<T>>, Self::Error> {
        trace!("{}: waiting for signal high", self.label);
        let _ = self.signal.wait_for_high().await;
        trace!("{}: signal is high, reading", self.label);
        self.reader.read_tlv().await
    }
}

/// A guarded TLV writer that signals during writes.
/// Sets signal high for the duration of a write, low otherwise.
pub struct GuardedWriteTlv<S, W> {
    #[allow(dead_code)] // Used by trace! macro when trace-tlv feature is enabled
    label: &'static str,
    signal: S,
    writer: LabeledWriter<W>,
}

impl<S, W> GuardedWriteTlv<S, W>
where
    S: embedded_hal::digital::OutputPin,
{
    pub fn new(label: &'static str, mut signal: S, writer: W) -> Self {
        trace!("{}: init signal low", label);
        let _ = signal.set_low();
        Self {
            label,
            signal,
            writer: LabeledWriter::new(label, writer),
        }
    }
}

impl<S, W> embedded_io_async::ErrorType for GuardedWriteTlv<S, W>
where
    W: Write,
{
    type Error = W::Error;
}

impl<S, W> Write for GuardedWriteTlv<S, W>
where
    S: embedded_hal::digital::OutputPin,
    W: Write,
{
    async fn write(&mut self, buf: &[u8]) -> Result<usize, Self::Error> {
        trace!("{}: signal high, writing {} bytes", self.label, buf.len());
        let _ = self.signal.set_high();
        self.writer.write(buf).await
    }

    async fn flush(&mut self) -> Result<(), Self::Error> {
        let result = self.writer.flush().await;
        trace!("{}: signal low", self.label);
        let _ = self.signal.set_low();
        result
    }
}

impl<S, W> WriteTlv for GuardedWriteTlv<S, W>
where
    S: embedded_hal::digital::OutputPin,
    W: Write,
{
    type Error = W::Error;

    async fn write_tlv<T: Into<u16>>(&mut self, tlv_type: T, value: &[u8]) -> Result<(), W::Error> {
        // Signal high at start
        trace!("{}: signal high", self.label);
        let _ = self.signal.set_high();
        // Delegate to labeled writer (which logs the TLV details)
        let result = self.writer.write_tlv(tlv_type, value).await;
        // Signal low after flush
        trace!("{}: signal low", self.label);
        let _ = self.signal.set_low();
        result
    }
}

/// WriteTlv impl for byte slices - used internally for encoding TLVs to buffers.
/// No logging since this is just in-memory encoding.
impl WriteTlv for &mut [u8] {
    type Error = core::convert::Infallible;

    async fn write_tlv<T: Into<u16>>(
        &mut self,
        tlv_type: T,
        value: &[u8],
    ) -> Result<(), Self::Error> {
        let header = encode_header(tlv_type, value.len());
        // Write header
        let header_len = header.len();
        self[..header_len].copy_from_slice(&header);
        *self = &mut core::mem::take(self)[header_len..];
        // Write value
        let value_len = value.len();
        self[..value_len].copy_from_slice(value);
        *self = &mut core::mem::take(self)[value_len..];
        Ok(())
    }
}

/// ReadTlv impl for byte slices - used internally for decoding TLVs from buffers.
/// No logging since this is just in-memory decoding.
impl ReadTlv for &[u8] {
    type Error = ReadError<core::convert::Infallible>;

    async fn read_tlv<T: TryFrom<u16>>(&mut self) -> Result<Option<Tlv<T>>, Self::Error> {
        if self.len() < HEADER_SIZE {
            return Ok(None);
        }

        let mut header = Header::default();
        header.copy_from_slice(&self[..HEADER_SIZE]);
        *self = &self[HEADER_SIZE..];

        let (tlv_type, length) = match decode_header(&header) {
            Ok(v) => v,
            Err(_) => return Err(ReadError::InvalidType),
        };

        if self.len() < length {
            return Err(ReadError::TooLong);
        }

        let mut value = Value::new();
        if value.resize(length, 0).is_err() {
            return Err(ReadError::TooLong);
        }
        value.copy_from_slice(&self[..length]);
        *self = &self[length..];

        Ok(Some(Tlv { tlv_type, value }))
    }
}

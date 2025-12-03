use embedded_io_async::{Read, ReadExactError, Write};
use heapless::Vec;
use num_enum::{IntoPrimitive, TryFromPrimitive};

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

impl<R> ReadTlv for R
where
    R: Read,
{
    type Error = ReadError<R::Error>;

    async fn read_tlv<T: TryFrom<u16>>(&mut self) -> Result<Option<Tlv<T>>, Self::Error> {
        let mut header = [0u8; HEADER_SIZE];
        match self.read_exact(&mut header).await {
            Ok(()) => {}
            Err(ReadExactError::UnexpectedEof) => return Ok(None),
            Err(ReadExactError::Other(e)) => return Err(ReadError::Io(e)),
        }

        let (tlv_type, length) = decode_header(&header).map_err(|_| ReadError::InvalidType)?;

        let mut value = Value::new();
        value.resize(length, 0).map_err(|_| ReadError::TooLong)?;

        match self.read_exact(&mut value).await {
            Ok(()) => {}
            Err(ReadExactError::UnexpectedEof) => return Ok(None),
            Err(ReadExactError::Other(e)) => return Err(ReadError::Io(e)),
        }

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

impl<W> WriteTlv for W
where
    W: Write,
{
    type Error = W::Error;

    async fn write_tlv<T: Into<u16>>(&mut self, tlv_type: T, value: &[u8]) -> Result<(), W::Error> {
        let header = encode_header(tlv_type, value.len());
        self.write_all(&header).await?;
        self.write_all(&value).await?;
        Ok(())
    }
}

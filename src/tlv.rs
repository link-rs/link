use embedded_io_async::{Read, ReadExactError, Write};
use heapless::Vec;
use num_enum::{IntoPrimitive, TryFromPrimitive};

const HEADER_SIZE: usize = 6;
pub const MAX_VALUE_SIZE: usize = 512;

pub type Type = u16;
pub type Length = u32;
pub type Value = Vec<u8, MAX_VALUE_SIZE>;

#[derive(Copy, Clone, PartialEq, Eq, Debug, IntoPrimitive, TryFromPrimitive)]
#[repr(u16)]
pub enum CtlToMgmt {
    Ping = 0,
}

#[derive(Copy, Clone, PartialEq, Eq, Debug, IntoPrimitive, TryFromPrimitive)]
#[repr(u16)]
pub enum MgmtToCtl {
    Pong = 0,
}

fn decode_header<T: TryFrom<u16>>(header: &[u8; HEADER_SIZE]) -> Result<(T, usize), T::Error> {
    let raw_type = Type::from_be_bytes([header[0], header[1]]);
    let tlv_type = T::try_from(raw_type)?;
    let length = Length::from_be_bytes([header[2], header[3], header[4], header[5]]);
    Ok((tlv_type, length as usize))
}

fn encode_header(tlv_type: impl Into<u16>, length: usize) -> [u8; HEADER_SIZE] {
    let mut header = [0u8; HEADER_SIZE];
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
}

#[derive(Debug)]
pub enum ReadError<E> {
    Io(E),
    InvalidType,
    TooLong,
}

pub trait ReadTlv {
    type Error;
    async fn read_tlv<T>(&mut self) -> Result<Option<Tlv<T>>, Self::Error>
    where
        T: TryFrom<u16>;
}

impl<R> ReadTlv for R
where
    R: Read,
{
    type Error = ReadError<R::Error>;

    async fn read_tlv<T>(&mut self) -> Result<Option<Tlv<T>>, Self::Error>
    where
        T: TryFrom<u16>,
    {
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
    type Error;
    async fn write_tlv<T: Into<u16>>(
        &mut self,
        tlv_type: T,
        value: &[u8],
    ) -> Result<(), Self::Error>;
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

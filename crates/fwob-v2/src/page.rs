use std::io::{Read, Write};

use byteorder::{LittleEndian, ReadBytesExt, WriteBytesExt};
use crc32fast::Hasher;
use fwob_core::Key;

use crate::{Codec, Result, V2Error};

pub const PAGE_MAGIC: &[u8; 4] = b"FWP2";
pub const PAGE_HEADER_LEN: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Encoding {
    RowRawV1 = 0,
    ColumnarBasicV1 = 1,
    ColumnarDeltaV1 = 2,
}

impl Encoding {
    pub fn from_id(id: u8) -> Result<Self> {
        match id {
            0 => Ok(Self::RowRawV1),
            1 => Ok(Self::ColumnarBasicV1),
            2 => Ok(Self::ColumnarDeltaV1),
            other => Err(V2Error::UnsupportedEncoding(other)),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PageHeader {
    pub header_version: u8,
    pub codec: Codec,
    pub encoding: Encoding,
    pub flags: u8,
    pub header_crc32: u32,
    pub payload_crc32: u32,
    pub first_key: Key,
    pub last_key: Key,
    pub frame_count: u32,
    pub uncompressed_len: u32,
    pub compressed_len: u32,
    pub first_frame_index: u64,
}

impl PageHeader {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        codec: Codec,
        encoding: Encoding,
        first_key: Key,
        last_key: Key,
        frame_count: u32,
        uncompressed_len: u32,
        compressed_len: u32,
        first_frame_index: u64,
        payload: &[u8],
    ) -> Self {
        let payload_crc32 = crc32(payload);
        let mut header = Self {
            header_version: 1,
            codec,
            encoding,
            flags: 0,
            header_crc32: 0,
            payload_crc32,
            first_key,
            last_key,
            frame_count,
            uncompressed_len,
            compressed_len,
            first_frame_index,
        };
        header.header_crc32 = crc32(&header.bytes_with_zero_crc());
        header
    }

    pub fn read<R: Read>(reader: &mut R, page_index: u64) -> Result<Self> {
        let mut magic = [0u8; 4];
        reader.read_exact(&mut magic)?;
        if &magic != PAGE_MAGIC {
            return Err(V2Error::InvalidPageHeader(page_index));
        }
        let header_version = reader.read_u8()?;
        let codec = Codec::from_id(reader.read_u8()?)?;
        let encoding = Encoding::from_id(reader.read_u8()?)?;
        let flags = reader.read_u8()?;
        let header_crc32 = reader.read_u32::<LittleEndian>()?;
        let payload_crc32 = reader.read_u32::<LittleEndian>()?;
        let first_key = read_key(reader)?;
        let last_key = read_key(reader)?;
        let frame_count = reader.read_u32::<LittleEndian>()?;
        let uncompressed_len = reader.read_u32::<LittleEndian>()?;
        let compressed_len = reader.read_u32::<LittleEndian>()?;
        let first_frame_index = reader.read_u64::<LittleEndian>()?;

        let mut reserved = [0u8; 10];
        reader.read_exact(&mut reserved)?;

        let header = Self {
            header_version,
            codec,
            encoding,
            flags,
            header_crc32,
            payload_crc32,
            first_key,
            last_key,
            frame_count,
            uncompressed_len,
            compressed_len,
            first_frame_index,
        };
        if crc32(&header.bytes_with_zero_crc()) != header.header_crc32 {
            return Err(V2Error::InvalidPageHeader(page_index));
        }
        Ok(header)
    }

    pub fn write<W: Write>(&self, writer: &mut W) -> Result<()> {
        writer.write_all(PAGE_MAGIC)?;
        writer.write_u8(self.header_version)?;
        writer.write_u8(self.codec as u8)?;
        writer.write_u8(self.encoding as u8)?;
        writer.write_u8(self.flags)?;
        writer.write_u32::<LittleEndian>(self.header_crc32)?;
        writer.write_u32::<LittleEndian>(self.payload_crc32)?;
        write_key(writer, self.first_key)?;
        write_key(writer, self.last_key)?;
        writer.write_u32::<LittleEndian>(self.frame_count)?;
        writer.write_u32::<LittleEndian>(self.uncompressed_len)?;
        writer.write_u32::<LittleEndian>(self.compressed_len)?;
        writer.write_u64::<LittleEndian>(self.first_frame_index)?;
        writer.write_all(&[0u8; 10])?;
        Ok(())
    }

    pub fn validate_payload(&self, payload: &[u8]) -> Result<()> {
        if crc32(payload) == self.payload_crc32 {
            Ok(())
        } else {
            Err(V2Error::ChecksumMismatch)
        }
    }

    fn bytes_with_zero_crc(&self) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(PAGE_HEADER_LEN);
        bytes.extend_from_slice(PAGE_MAGIC);
        bytes.push(self.header_version);
        bytes.push(self.codec as u8);
        bytes.push(self.encoding as u8);
        bytes.push(self.flags);
        bytes.extend_from_slice(&0u32.to_le_bytes());
        bytes.extend_from_slice(&self.payload_crc32.to_le_bytes());
        write_key(&mut bytes, self.first_key).unwrap();
        write_key(&mut bytes, self.last_key).unwrap();
        bytes.extend_from_slice(&self.frame_count.to_le_bytes());
        bytes.extend_from_slice(&self.uncompressed_len.to_le_bytes());
        bytes.extend_from_slice(&self.compressed_len.to_le_bytes());
        bytes.extend_from_slice(&self.first_frame_index.to_le_bytes());
        bytes.extend_from_slice(&[0u8; 10]);
        bytes
    }
}

fn crc32(bytes: &[u8]) -> u32 {
    let mut hasher = Hasher::new();
    hasher.update(bytes);
    hasher.finalize()
}

fn write_key<W: Write>(writer: &mut W, key: Key) -> std::io::Result<()> {
    let tag = match key {
        Key::I8(_) => 0,
        Key::I16(_) => 1,
        Key::I32(_) => 2,
        Key::I64(_) => 3,
        Key::U8(_) => 4,
        Key::U16(_) => 5,
        Key::U32(_) => 6,
        Key::U64(_) => 7,
    };
    writer.write_all(&[tag])?;
    let mut raw = Vec::with_capacity(8);
    key.encode(&mut raw);
    raw.resize(8, 0);
    writer.write_all(&raw)?;
    Ok(())
}

fn read_key<R: Read>(reader: &mut R) -> Result<Key> {
    let tag = reader.read_u8()?;
    let mut raw = [0u8; 8];
    reader.read_exact(&mut raw)?;
    Ok(match tag {
        0 => Key::I8(raw[0] as i8),
        1 => Key::I16(i16::from_le_bytes(raw[..2].try_into().unwrap())),
        2 => Key::I32(i32::from_le_bytes(raw[..4].try_into().unwrap())),
        3 => Key::I64(i64::from_le_bytes(raw)),
        4 => Key::U8(raw[0]),
        5 => Key::U16(u16::from_le_bytes(raw[..2].try_into().unwrap())),
        6 => Key::U32(u32::from_le_bytes(raw[..4].try_into().unwrap())),
        7 => Key::U64(u64::from_le_bytes(raw)),
        _ => return Err(V2Error::InvalidPageHeader(0)),
    })
}

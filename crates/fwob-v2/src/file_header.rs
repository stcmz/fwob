use std::io::{Cursor, Read, Seek, SeekFrom, Write};

use byteorder::{LittleEndian, ReadBytesExt, WriteBytesExt};
use fwob_core::{Field, FieldType, Schema};

use crate::{Result, V2Error};

pub const MAGIC: &[u8; 4] = b"FWB2";
pub const VERSION: u8 = 2;
pub const FILE_HEADER_LEN: u64 = 4096;
pub const MIN_PAGE_SIZE: u32 = 1024;
pub const MAX_PAGE_SIZE: u32 = 16 * 1024 * 1024;
const MAX_HEADER_FIELDS: u16 = 768;
const MAX_HEADER_STRINGS: u32 = 2048;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileHeader {
    pub page_size: u32,
    pub page_count: u64,
    pub frame_count: u64,
    pub key_field_index: u16,
    pub title: String,
    pub schema: Schema,
    pub string_table: Vec<String>,
}

impl FileHeader {
    pub fn page_offset(&self, page_index: u64) -> u64 {
        FILE_HEADER_LEN + page_index * u64::from(self.page_size)
    }
}

pub fn read_file_header<R: Read + Seek>(reader: &mut R) -> Result<FileHeader> {
    reader.seek(SeekFrom::Start(0))?;
    let mut header_bytes = vec![0; FILE_HEADER_LEN as usize];
    reader.read_exact(&mut header_bytes)?;
    let reader = &mut Cursor::new(header_bytes);
    let mut magic = [0u8; 4];
    reader.read_exact(&mut magic)?;
    if &magic != MAGIC {
        return Err(V2Error::InvalidFileHeader);
    }
    let version = reader.read_u8()?;
    if version != VERSION {
        return Err(V2Error::InvalidFileHeader);
    }
    let page_size = reader.read_u32::<LittleEndian>()?;
    if !(MIN_PAGE_SIZE..=MAX_PAGE_SIZE).contains(&page_size) {
        return Err(V2Error::InvalidFileHeader);
    }
    let page_count = reader.read_u64::<LittleEndian>()?;
    let frame_count = reader.read_u64::<LittleEndian>()?;
    let key_field_index = reader.read_u16::<LittleEndian>()?;
    let title = read_len_string(reader)?;
    let frame_type = read_len_string(reader)?;
    let field_count = reader.read_u16::<LittleEndian>()?;
    if field_count == 0 || field_count > MAX_HEADER_FIELDS {
        return Err(V2Error::InvalidFileHeader);
    }
    let mut fields = Vec::with_capacity(field_count as usize);
    let mut offset = 0u32;
    for _ in 0..field_count {
        let name = read_len_string(reader)?;
        let field_type = FieldType::from_v1_id(reader.read_u8()?)?;
        let length = reader.read_u16::<LittleEndian>()?;
        fields.push(Field::new(name, field_type, length, offset));
        offset += u32::from(length);
    }
    let string_count = reader.read_u32::<LittleEndian>()?;
    if string_count > MAX_HEADER_STRINGS {
        return Err(V2Error::InvalidFileHeader);
    }
    let mut string_table = Vec::with_capacity(string_count as usize);
    for _ in 0..string_count {
        string_table.push(read_len_string(reader)?);
    }
    let schema = Schema::new(frame_type, fields, key_field_index as usize)?;
    Ok(FileHeader {
        page_size,
        page_count,
        frame_count,
        key_field_index,
        title,
        schema,
        string_table,
    })
}

pub fn write_file_header<W: Write + Seek>(writer: &mut W, header: &FileHeader) -> Result<()> {
    writer.seek(SeekFrom::Start(0))?;
    let mut bytes = Vec::new();
    bytes.extend_from_slice(MAGIC);
    bytes.push(VERSION);
    bytes.extend_from_slice(&header.page_size.to_le_bytes());
    bytes.extend_from_slice(&header.page_count.to_le_bytes());
    bytes.extend_from_slice(&header.frame_count.to_le_bytes());
    bytes.extend_from_slice(&header.key_field_index.to_le_bytes());
    write_len_string(&mut bytes, &header.title)?;
    write_len_string(&mut bytes, &header.schema.frame_type)?;
    bytes.extend_from_slice(&(header.schema.fields.len() as u16).to_le_bytes());
    for field in &header.schema.fields {
        write_len_string(&mut bytes, &field.name)?;
        bytes.push(field.field_type as u8);
        bytes.extend_from_slice(&field.length.to_le_bytes());
    }
    bytes.extend_from_slice(&(header.string_table.len() as u32).to_le_bytes());
    for value in &header.string_table {
        write_len_string(&mut bytes, value)?;
    }
    if bytes.len() > FILE_HEADER_LEN as usize {
        return Err(V2Error::InvalidFileHeader);
    }
    writer.write_all(&bytes)?;
    writer.write_all(&vec![0u8; FILE_HEADER_LEN as usize - bytes.len()])?;
    Ok(())
}

pub fn update_counts<W: Write + Seek>(
    writer: &mut W,
    page_count: u64,
    frame_count: u64,
) -> Result<()> {
    writer.seek(SeekFrom::Start(9))?;
    writer.write_u64::<LittleEndian>(page_count)?;
    writer.write_u64::<LittleEndian>(frame_count)?;
    Ok(())
}

fn read_len_string<R: Read>(reader: &mut R) -> Result<String> {
    let len = reader.read_u16::<LittleEndian>()?;
    let mut bytes = vec![0u8; len as usize];
    reader.read_exact(&mut bytes)?;
    String::from_utf8(bytes).map_err(|_| V2Error::InvalidFileHeader)
}

fn write_len_string<W: Write>(writer: &mut W, value: &str) -> Result<()> {
    if value.len() > u16::MAX as usize {
        return Err(V2Error::InvalidFileHeader);
    }
    writer.write_u16::<LittleEndian>(value.len() as u16)?;
    writer.write_all(value.as_bytes())?;
    Ok(())
}

use std::io::{Read, Seek, SeekFrom, Write};

use byteorder::{LittleEndian, ReadBytesExt, WriteBytesExt};
use fwob_core::{Field, FieldType, Schema};

use crate::{Result, V1Error};

pub const SIGNATURE: &[u8; 4] = b"FWOB";
pub const VERSION: u8 = 1;
pub const HEADER_LEN: u64 = 214;
pub const DEFAULT_PREFIX_LEN: u32 = 2048;
pub const DEFAULT_STRING_TABLE_PRESERVED_LEN: u32 = DEFAULT_PREFIX_LEN - HEADER_LEN as u32;
pub const MAX_FIELDS: usize = 16;
pub const MAX_FIELD_NAME_LEN: usize = 8;
pub const MAX_FRAME_TYPE_LEN: usize = 16;
pub const MAX_TITLE_LEN: usize = 16;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Header {
    pub version: u8,
    pub field_count: u8,
    pub field_lengths: Vec<u8>,
    pub field_types: u64,
    pub field_names: Vec<String>,
    pub string_count: u32,
    pub string_table_length: u32,
    pub string_table_preserved_length: u32,
    pub frame_count: u64,
    pub frame_length: u32,
    pub frame_type: String,
    pub title: String,
}

impl Header {
    pub fn first_frame_position(&self) -> u64 {
        HEADER_LEN + u64::from(self.string_table_preserved_length)
    }

    pub fn string_table_position(&self) -> u64 {
        HEADER_LEN
    }

    pub fn string_table_ending(&self) -> u64 {
        HEADER_LEN + u64::from(self.string_table_length)
    }

    pub fn file_length(&self) -> u64 {
        self.first_frame_position() + u64::from(self.frame_length) * self.frame_count
    }

    pub fn schema(&self, key_field_index: usize) -> Result<Schema> {
        if key_field_index >= self.field_count as usize {
            return Err(V1Error::KeyFieldIndexOutOfRange(key_field_index));
        }

        let mut offset = 0u32;
        let mut fields = Vec::with_capacity(self.field_count as usize);
        for i in 0..self.field_count as usize {
            let field_type_id = ((self.field_types >> (i * 4)) & 0x0f) as u8;
            let field_type = FieldType::from_v1_id(field_type_id)?;
            let length = u16::from(self.field_lengths[i]);
            fields.push(Field::new(
                self.field_names[i].clone(),
                field_type,
                length,
                offset,
            ));
            offset += u32::from(length);
        }
        Schema::new(self.frame_type.clone(), fields, key_field_index).map_err(Into::into)
    }
}

pub fn read_header<R: Read>(reader: &mut R) -> Result<Header> {
    let mut sig = [0u8; 4];
    reader.read_exact(&mut sig)?;
    if &sig != SIGNATURE {
        return Err(V1Error::CorruptedHeader);
    }

    let version = reader.read_u8()?;
    if version != VERSION {
        return Err(V1Error::CorruptedHeader);
    }

    let field_count = reader.read_u8()?;
    if field_count as usize > MAX_FIELDS {
        return Err(V1Error::CorruptedHeader);
    }

    let mut all_lengths = [0u8; MAX_FIELDS];
    reader.read_exact(&mut all_lengths)?;
    let field_lengths = all_lengths[..field_count as usize].to_vec();
    let field_types = reader.read_u64::<LittleEndian>()?;

    let mut all_names = Vec::with_capacity(MAX_FIELDS);
    for _ in 0..MAX_FIELDS {
        let mut raw = [0u8; MAX_FIELD_NAME_LEN];
        reader.read_exact(&mut raw)?;
        all_names.push(trim_ascii_field(&raw));
    }
    let field_names = all_names[..field_count as usize].to_vec();
    if field_names.iter().any(|name| name.is_empty()) {
        return Err(V1Error::CorruptedHeader);
    }

    let string_count = reader.read_i32::<LittleEndian>()?;
    let string_table_length = reader.read_i32::<LittleEndian>()?;
    let string_table_preserved_length = reader.read_i32::<LittleEndian>()?;
    if string_count < 0
        || string_table_length < 0
        || string_table_preserved_length < string_table_length
    {
        return Err(V1Error::CorruptedHeader);
    }

    let frame_count = reader.read_i64::<LittleEndian>()?;
    let frame_length = reader.read_i32::<LittleEndian>()?;
    if frame_count < 0 || frame_length < 0 {
        return Err(V1Error::CorruptedHeader);
    }
    let expected_frame_length: u32 = field_lengths.iter().map(|&v| u32::from(v)).sum();
    if frame_length as u32 != expected_frame_length {
        return Err(V1Error::CorruptedHeader);
    }

    let mut frame_type = [0u8; MAX_FRAME_TYPE_LEN];
    reader.read_exact(&mut frame_type)?;
    let frame_type = trim_ascii_field(&frame_type);
    if frame_type.is_empty() {
        return Err(V1Error::CorruptedHeader);
    }

    let mut title = [0u8; MAX_TITLE_LEN];
    reader.read_exact(&mut title)?;
    let title = trim_ascii_field(&title);
    if title.is_empty() {
        return Err(V1Error::CorruptedHeader);
    }

    Ok(Header {
        version,
        field_count,
        field_lengths,
        field_types,
        field_names,
        string_count: string_count as u32,
        string_table_length: string_table_length as u32,
        string_table_preserved_length: string_table_preserved_length as u32,
        frame_count: frame_count as u64,
        frame_length: frame_length as u32,
        frame_type,
        title,
    })
}

pub fn write_header<W: Write>(writer: &mut W, header: &Header) -> Result<()> {
    writer.write_all(SIGNATURE)?;
    writer.write_u8(header.version)?;
    writer.write_u8(header.field_count)?;

    writer.write_all(&header.field_lengths)?;
    if header.field_lengths.len() < MAX_FIELDS {
        writer.write_all(&vec![0; MAX_FIELDS - header.field_lengths.len()])?;
    }

    writer.write_u64::<LittleEndian>(header.field_types)?;

    for name in &header.field_names {
        write_fixed_ascii(writer, name, MAX_FIELD_NAME_LEN)?;
    }
    for _ in header.field_names.len()..MAX_FIELDS {
        writer.write_all(&[0u8; MAX_FIELD_NAME_LEN])?;
    }

    writer.write_i32::<LittleEndian>(header.string_count as i32)?;
    writer.write_i32::<LittleEndian>(header.string_table_length as i32)?;
    writer.write_i32::<LittleEndian>(header.string_table_preserved_length as i32)?;
    writer.write_i64::<LittleEndian>(header.frame_count as i64)?;
    writer.write_i32::<LittleEndian>(header.frame_length as i32)?;
    write_fixed_ascii(writer, &header.frame_type, MAX_FRAME_TYPE_LEN)?;
    write_fixed_ascii(writer, &header.title, MAX_TITLE_LEN)?;
    Ok(())
}

pub fn update_frame_count<W: Write + Seek>(writer: &mut W, frame_count: u64) -> Result<()> {
    writer.seek(SeekFrom::Start(170))?;
    writer.write_i64::<LittleEndian>(frame_count as i64)?;
    Ok(())
}

pub fn update_string_table_len<W: Write + Seek>(
    writer: &mut W,
    string_count: u32,
    string_table_length: u32,
) -> Result<()> {
    writer.seek(SeekFrom::Start(158))?;
    writer.write_i32::<LittleEndian>(string_count as i32)?;
    writer.write_i32::<LittleEndian>(string_table_length as i32)?;
    Ok(())
}

fn trim_ascii_field(bytes: &[u8]) -> String {
    let end = bytes
        .iter()
        .rposition(|&b| b != 0 && b != b' ')
        .map(|idx| idx + 1)
        .unwrap_or(0);
    String::from_utf8_lossy(&bytes[..end]).into_owned()
}

fn write_fixed_ascii<W: Write>(writer: &mut W, value: &str, len: usize) -> Result<()> {
    let bytes = value.as_bytes();
    if bytes.len() > len {
        return Err(V1Error::CorruptedHeader);
    }
    writer.write_all(bytes)?;
    if bytes.len() < len {
        writer.write_all(&vec![b' '; len - bytes.len()])?;
    }
    Ok(())
}

use std::io::{Cursor, Read, Seek, SeekFrom, Write};

use byteorder::{LittleEndian, ReadBytesExt, WriteBytesExt};
use fwob_core::{Field, FieldSemantic, FieldType, Schema};

use crate::{Result, V2Error};

pub const MAGIC: &[u8; 4] = b"FWB2";
pub const VERSION: u8 = 3;
pub const FILE_HEADER_LEN: u64 = 4096;
pub const MIN_PAGE_SIZE: u32 = 1024;
pub const MAX_PAGE_SIZE: u32 = 16 * 1024 * 1024;
const MAX_HEADER_FIELDS: u16 = 768;
const MAX_HEADER_STRINGS: u32 = 2048;
const TITLE_LENGTH_OFFSET: u64 = 4 + 1 + 4 + 8 + 8 + 2;
const TITLE_BYTES_OFFSET: u64 = TITLE_LENGTH_OFFSET + 2;

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
        let semantic = FieldSemantic::from_id(reader.read_u8()?)?;
        fields.push(Field::new(name, field_type, length, offset).with_semantic(semantic));
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
    if header.schema.fields.len() > MAX_HEADER_FIELDS as usize
        || header.string_table.len() > MAX_HEADER_STRINGS as usize
    {
        return Err(V2Error::InvalidFileHeader);
    }
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
        bytes.push(field.semantic.id());
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

pub fn update_metadata(
    path: impl AsRef<std::path::Path>,
    frame_type: Option<&str>,
    title: Option<&str>,
    string_table: Option<&[String]>,
) -> Result<()> {
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)?;
    let mut header = read_file_header(&mut file)?;
    let actual_len = file.metadata()?.len();
    let expected_len = FILE_HEADER_LEN + header.page_count * u64::from(header.page_size);
    if actual_len != expected_len {
        return Err(V2Error::InvalidFileHeader);
    }
    if let Some(title) = title {
        if title.is_empty() {
            return Err(V2Error::InvalidFileHeader);
        }
        // Fast path: only an equal-length title changes, so overwrite the title bytes in place.
        if frame_type.is_none() && string_table.is_none() && title.len() == header.title.len() {
            file.seek(SeekFrom::Start(TITLE_BYTES_OFFSET))?;
            file.write_all(title.as_bytes())?;
            file.flush()?;
            return Ok(());
        }
        header.title = title.to_owned();
    }
    if let Some(frame_type) = frame_type {
        if frame_type.is_empty() {
            return Err(V2Error::InvalidFileHeader);
        }
        header.schema.frame_type = frame_type.to_owned();
    }
    if let Some(strings) = string_table {
        header.string_table = strings.to_vec();
    }
    // The v2 header is a fixed FILE_HEADER_LEN region, so this rewrite never shifts frame data;
    // write_file_header rejects a header that no longer fits.
    write_file_header(&mut file, &header)?;
    file.flush()?;
    Ok(())
}

/// Updates the per-field `semantic` of an existing v2 file in place. Field semantics are metadata
/// only (they do not affect frame layout), and the header is fixed-size, so this rewrites just the
/// header and leaves all pages untouched. Each update names a field; the resulting schema is
/// re-validated (e.g. timestamp semantics are only allowed on integer fields).
pub fn update_field_semantics(
    path: impl AsRef<std::path::Path>,
    updates: &[(String, FieldSemantic)],
) -> Result<()> {
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)?;
    let mut header = read_file_header(&mut file)?;
    let actual_len = file.metadata()?.len();
    let expected_len = FILE_HEADER_LEN + header.page_count * u64::from(header.page_size);
    if actual_len != expected_len {
        return Err(V2Error::InvalidFileHeader);
    }
    let mut fields = header.schema.fields.clone();
    for (name, semantic) in updates {
        let field = fields
            .iter_mut()
            .find(|field| &field.name == name)
            .ok_or(V2Error::InvalidFileHeader)?;
        field.semantic = *semantic;
    }
    let schema = Schema::new(
        header.schema.frame_type.clone(),
        fields,
        header.schema.key_field_index,
    )?;
    header.schema = schema;
    write_file_header(&mut file, &header)?;
    file.flush()?;
    Ok(())
}

/// Renames schema fields (columns) of an existing v2 file in place. Field names are metadata only
/// (they do not affect frame layout), and the header is fixed-size, so this rewrites just the
/// header and leaves all pages untouched. Each rename names an existing field by its current name;
/// the resulting schema is re-validated (rejecting empty or duplicate names).
pub fn update_field_names(
    path: impl AsRef<std::path::Path>,
    renames: &[(String, String)],
) -> Result<()> {
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)?;
    let mut header = read_file_header(&mut file)?;
    let actual_len = file.metadata()?.len();
    let expected_len = FILE_HEADER_LEN + header.page_count * u64::from(header.page_size);
    if actual_len != expected_len {
        return Err(V2Error::InvalidFileHeader);
    }
    let mut fields = header.schema.fields.clone();
    for (old, new) in renames {
        let field = fields
            .iter_mut()
            .find(|field| &field.name == old)
            .ok_or(V2Error::InvalidFileHeader)?;
        field.name = new.clone();
    }
    let schema = Schema::new(
        header.schema.frame_type.clone(),
        fields,
        header.schema.key_field_index,
    )?;
    header.schema = schema;
    write_file_header(&mut file, &header)?;
    file.flush()?;
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

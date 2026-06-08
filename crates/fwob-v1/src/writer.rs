use std::{
    fs::File,
    io::{Seek, SeekFrom, Write},
    path::Path,
};

use fwob_core::{FrameRef, Key, KeyType, Schema};

use crate::{
    header::{
        update_frame_count, update_string_table_len, write_header, Header,
        DEFAULT_STRING_TABLE_PRESERVED_LEN, VERSION,
    },
    Result, V1Error,
};

#[derive(Debug, Clone)]
pub struct WriterOptions {
    pub title: String,
    pub string_table_preserved_length: u32,
}

impl WriterOptions {
    pub fn new(title: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            string_table_preserved_length: DEFAULT_STRING_TABLE_PRESERVED_LEN,
        }
    }
}

pub struct Writer<W> {
    inner: W,
    header: Header,
    schema: Schema,
    key_type: KeyType,
    last_key: Option<Key>,
}

impl Writer<File> {
    pub fn create(path: impl AsRef<Path>, schema: Schema, options: WriterOptions) -> Result<Self> {
        let file = File::create(path)?;
        Self::new(file, schema, options)
    }
}

impl<W: Write + Seek> Writer<W> {
    pub fn new(mut inner: W, schema: Schema, options: WriterOptions) -> Result<Self> {
        let key_type = KeyType::from_field(schema.key_field())?;
        let header = Header {
            version: VERSION,
            field_count: schema.fields.len() as u8,
            field_lengths: schema.fields.iter().map(|f| f.length as u8).collect(),
            field_types: schema
                .fields
                .iter()
                .enumerate()
                .fold(0u64, |acc, (i, f)| acc | ((f.field_type as u64) << (i * 4))),
            field_names: schema.fields.iter().map(|f| f.name.clone()).collect(),
            string_count: 0,
            string_table_length: 0,
            string_table_preserved_length: options.string_table_preserved_length,
            frame_count: 0,
            frame_length: schema.frame_len,
            frame_type: schema.frame_type.clone(),
            title: options.title,
        };
        write_header(&mut inner, &header)?;
        inner.write_all(&vec![0; header.string_table_preserved_length as usize])?;
        inner.flush()?;
        Ok(Self {
            inner,
            header,
            schema,
            key_type,
            last_key: None,
        })
    }

    pub fn header(&self) -> &Header {
        &self.header
    }

    pub fn append_string(&mut self, value: &str) -> Result<u32> {
        let encoded_len = dotnet_string_len(value);
        let required = self.header.string_table_length + encoded_len;
        if required > self.header.string_table_preserved_length {
            return Err(V1Error::StringTableOutOfSpace {
                required,
                preserved: self.header.string_table_preserved_length,
            });
        }

        self.inner
            .seek(SeekFrom::Start(self.header.string_table_ending()))?;
        write_dotnet_string(&mut self.inner, value)?;
        let index = self.header.string_count;
        self.header.string_count += 1;
        self.header.string_table_length = required;
        update_string_table_len(
            &mut self.inner,
            self.header.string_count,
            self.header.string_table_length,
        )?;
        self.inner.flush()?;
        Ok(index)
    }

    pub fn append_frame(&mut self, bytes: &[u8]) -> Result<()> {
        let frame = FrameRef::new(&self.schema, bytes)?;
        let key = frame.key(&self.schema, self.key_type)?;
        if let Some(last_key) = self.last_key {
            if key < last_key {
                return Err(V1Error::KeyOrderViolation {
                    index: self.header.frame_count,
                });
            }
        }
        self.inner
            .seek(SeekFrom::Start(self.header.file_length()))?;
        self.inner.write_all(bytes)?;
        self.header.frame_count += 1;
        self.last_key = Some(key);
        update_frame_count(&mut self.inner, self.header.frame_count)?;
        self.inner.flush()?;
        Ok(())
    }

    pub fn append_presorted_raw_frames(&mut self, bytes: &[u8]) -> Result<()> {
        let frame_len = self.schema.frame_len as usize;
        if bytes.len() % frame_len != 0 {
            return Err(V1Error::Core(fwob_core::FwobError::InvalidFrameLength {
                expected: frame_len,
                actual: bytes.len(),
            }));
        }
        if bytes.is_empty() {
            return Ok(());
        }

        let first = FrameRef::new(&self.schema, &bytes[..frame_len])?;
        let first_key = first.key(&self.schema, self.key_type)?;
        if let Some(last_key) = self.last_key {
            if first_key < last_key {
                return Err(V1Error::KeyOrderViolation {
                    index: self.header.frame_count,
                });
            }
        }

        let last_offset = bytes.len() - frame_len;
        let last = FrameRef::new(&self.schema, &bytes[last_offset..])?;
        self.last_key = Some(last.key(&self.schema, self.key_type)?);

        self.inner
            .seek(SeekFrom::Start(self.header.file_length()))?;
        self.inner.write_all(bytes)?;
        self.header.frame_count += (bytes.len() / frame_len) as u64;
        update_frame_count(&mut self.inner, self.header.frame_count)?;
        self.inner.flush()?;
        Ok(())
    }
}

pub(crate) fn write_dotnet_string<W: Write>(writer: &mut W, value: &str) -> Result<()> {
    write_7bit_encoded_int(writer, value.len() as u32)?;
    writer.write_all(value.as_bytes())?;
    Ok(())
}

fn dotnet_string_len(value: &str) -> u32 {
    let len = value.len() as u32;
    let prefix = if len < 0x80 {
        1
    } else if len < 0x4000 {
        2
    } else if len < 0x20_0000 {
        3
    } else if len < 0x1000_0000 {
        4
    } else {
        5
    };
    prefix + len
}

fn write_7bit_encoded_int<W: Write>(writer: &mut W, mut value: u32) -> Result<()> {
    while value >= 0x80 {
        writer.write_all(&[((value as u8) & 0x7f) | 0x80])?;
        value >>= 7;
    }
    writer.write_all(&[value as u8])?;
    Ok(())
}

use std::{
    fs::OpenOptions,
    io::{Seek, SeekFrom, Write},
    path::Path,
};

use crate::{
    header::{
        read_header, update_string_table_len, write_header, HEADER_LEN, MAX_FIELD_NAME_LEN,
        MAX_FRAME_TYPE_LEN, MAX_TITLE_LEN,
    },
    writer::write_dotnet_string,
    Result, V1Error,
};

const TITLE_OFFSET: u64 = HEADER_LEN - MAX_TITLE_LEN as u64;
const FRAME_TYPE_OFFSET: u64 = TITLE_OFFSET - MAX_FRAME_TYPE_LEN as u64;

/// Updates v1 metadata in place without moving or rewriting frame data.
pub fn update_metadata(
    path: impl AsRef<Path>,
    frame_type: Option<&str>,
    title: Option<&str>,
    string_table: Option<&[String]>,
) -> Result<()> {
    if frame_type.is_none() && title.is_none() && string_table.is_none() {
        return Ok(());
    }

    let mut file = OpenOptions::new().read(true).write(true).open(path)?;
    let actual_len = file.metadata()?.len();
    let header = read_header(&mut file)?;
    let expected_len = header.file_length();
    if actual_len != expected_len {
        return Err(V1Error::CorruptedFileLength {
            expected: expected_len,
            actual: actual_len,
        });
    }

    let encoded_strings = string_table.map(encode_string_table).transpose()?;
    if let Some(encoded) = &encoded_strings {
        let required =
            u32::try_from(encoded.len()).map_err(|_| V1Error::StringTableOutOfSpace {
                required: u32::MAX,
                preserved: header.string_table_preserved_length,
            })?;
        if required > header.string_table_preserved_length {
            return Err(V1Error::StringTableOutOfSpace {
                required,
                preserved: header.string_table_preserved_length,
            });
        }
    }
    if let Some(frame_type) = frame_type {
        validate_frame_type(frame_type)?;
    }
    if let Some(title) = title {
        validate_title(title)?;
    }

    if let Some(frame_type) = frame_type {
        file.seek(SeekFrom::Start(FRAME_TYPE_OFFSET))?;
        file.write_all(frame_type.as_bytes())?;
        file.write_all(&vec![b' '; MAX_FRAME_TYPE_LEN - frame_type.len()])?;
    }

    if let Some(title) = title {
        file.seek(SeekFrom::Start(TITLE_OFFSET))?;
        file.write_all(title.as_bytes())?;
        file.write_all(&vec![b' '; MAX_TITLE_LEN - title.len()])?;
    }

    if let (Some(strings), Some(encoded)) = (string_table, encoded_strings.as_deref()) {
        file.seek(SeekFrom::Start(HEADER_LEN))?;
        file.write_all(encoded)?;
        let new_len = encoded.len() as u32;
        if new_len < header.string_table_length {
            file.write_all(&vec![0; (header.string_table_length - new_len) as usize])?;
        }
        update_string_table_len(&mut file, strings.len() as u32, new_len)?;
    }
    file.flush()?;
    Ok(())
}

/// Renames v1 schema fields (columns) in place. v1 stores field names in a fixed-size header
/// region, so this rewrites just the header and never moves frame data. Each rename names an
/// existing field by its current name; new names must be non-empty ASCII within the v1 field-name
/// limit, and the resulting set of names must stay unique.
pub fn update_field_names(path: impl AsRef<Path>, renames: &[(String, String)]) -> Result<()> {
    if renames.is_empty() {
        return Ok(());
    }
    let mut file = OpenOptions::new().read(true).write(true).open(path)?;
    let actual_len = file.metadata()?.len();
    let mut header = read_header(&mut file)?;
    let expected_len = header.file_length();
    if actual_len != expected_len {
        return Err(V1Error::CorruptedFileLength {
            expected: expected_len,
            actual: actual_len,
        });
    }
    for (old, new) in renames {
        validate_field_name(new)?;
        let index = header
            .field_names
            .iter()
            .position(|name| name == old)
            .ok_or_else(|| {
                V1Error::Core(fwob_core::FwobError::InvalidSchema(format!(
                    "field '{old}' not found"
                )))
            })?;
        header.field_names[index] = new.clone();
    }
    for i in 0..header.field_names.len() {
        for j in (i + 1)..header.field_names.len() {
            if header.field_names[i] == header.field_names[j] {
                return Err(V1Error::Core(fwob_core::FwobError::InvalidSchema(format!(
                    "duplicate field name '{}'",
                    header.field_names[i]
                ))));
            }
        }
    }
    file.seek(SeekFrom::Start(0))?;
    write_header(&mut file, &header)?;
    file.flush()?;
    Ok(())
}

fn validate_field_name(name: &str) -> Result<()> {
    if name.is_empty() || !name.is_ascii() || name.len() > MAX_FIELD_NAME_LEN {
        return Err(V1Error::Core(fwob_core::FwobError::InvalidSchema(format!(
            "field name '{name}' exceeds FWOB v1 limits (1..={MAX_FIELD_NAME_LEN} ASCII bytes)"
        ))));
    }
    Ok(())
}

fn validate_title(title: &str) -> Result<()> {
    if title.is_empty() || !title.is_ascii() || title.len() > MAX_TITLE_LEN {
        return Err(V1Error::Core(fwob_core::FwobError::InvalidSchema(
            "title exceeds FWOB v1 limits".into(),
        )));
    }
    Ok(())
}

fn validate_frame_type(frame_type: &str) -> Result<()> {
    if frame_type.is_empty() || !frame_type.is_ascii() || frame_type.len() > MAX_FRAME_TYPE_LEN {
        return Err(V1Error::Core(fwob_core::FwobError::InvalidSchema(
            "frame type exceeds FWOB v1 limits".into(),
        )));
    }
    Ok(())
}

fn encode_string_table(strings: &[String]) -> Result<Vec<u8>> {
    if strings.len() > i32::MAX as usize {
        return Err(V1Error::Core(fwob_core::FwobError::InvalidSchema(
            "string table exceeds FWOB v1 limits".into(),
        )));
    }
    let mut encoded = Vec::new();
    for value in strings {
        write_dotnet_string(&mut encoded, value)?;
    }
    Ok(encoded)
}

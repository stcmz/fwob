use std::{
    fs::OpenOptions,
    io::{Seek, SeekFrom, Write},
    path::Path,
};

use crate::{
    header::{read_header, update_string_table_len, HEADER_LEN, MAX_TITLE_LEN},
    writer::write_dotnet_string,
    Result, V1Error,
};

const TITLE_OFFSET: u64 = HEADER_LEN - MAX_TITLE_LEN as u64;

/// Updates v1 metadata in place without moving or rewriting frame data.
pub fn update_metadata(
    path: impl AsRef<Path>,
    title: Option<&str>,
    string_table: Option<&[String]>,
) -> Result<()> {
    if title.is_none() && string_table.is_none() {
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
    if let Some(title) = title {
        validate_title(title)?;
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

fn validate_title(title: &str) -> Result<()> {
    if title.is_empty() || !title.is_ascii() || title.len() > MAX_TITLE_LEN {
        return Err(V1Error::Core(fwob_core::FwobError::InvalidSchema(
            "title exceeds FWOB v1 limits".into(),
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

use std::{
    fs::{File, OpenOptions},
    io::{Read, Seek, SeekFrom},
    path::Path,
};

use fwob_core::{Key, KeyType};

use crate::{
    header::{read_header, update_frame_count},
    Reader, Result, V1Error,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerificationReport {
    pub frame_count: u64,
    pub string_count: u32,
    pub file_length: u64,
}

pub fn verify_file(path: impl AsRef<Path>, key_field_index: usize) -> Result<VerificationReport> {
    let file = File::open(path.as_ref())?;
    let file_length = file.metadata()?.len();
    let mut reader = Reader::new(file, key_field_index)?;
    reader.read_string_table()?;
    reader.verify_key_order()?;
    Ok(VerificationReport {
        frame_count: reader.header().frame_count,
        string_count: reader.header().string_count,
        file_length,
    })
}

pub fn repair_committed_tail(
    path: impl AsRef<Path>,
    key_field_index: usize,
) -> Result<VerificationReport> {
    let path = path.as_ref();
    let mut file = OpenOptions::new().read(true).write(true).open(path)?;
    let actual_len = file.metadata()?.len();
    file.seek(SeekFrom::Start(0))?;
    let header = read_header(&mut file)?;
    let prefix_len = header.first_frame_position();
    if actual_len < prefix_len || header.frame_length == 0 {
        return Err(V1Error::CorruptedFileLength {
            expected: prefix_len,
            actual: actual_len,
        });
    }

    let complete_frames = (actual_len - prefix_len) / u64::from(header.frame_length);
    let committed_frames = header.frame_count.min(complete_frames);
    let schema = header.schema(key_field_index)?;
    let key_field = schema.key_field();
    let key_type = KeyType::from_field(key_field)?;
    let mut last_key = None;
    for index in 0..committed_frames {
        let key = read_key(
            &mut file,
            prefix_len,
            header.frame_length,
            key_field.offset,
            key_field.length,
            key_type,
            index,
        )?;
        if last_key.is_some_and(|last| key < last) {
            return Err(V1Error::KeyOrderViolation { index });
        }
        last_key = Some(key);
    }

    let mut repaired_count = committed_frames;
    for index in committed_frames..complete_frames {
        let key = read_key(
            &mut file,
            prefix_len,
            header.frame_length,
            key_field.offset,
            key_field.length,
            key_type,
            index,
        )?;
        if last_key.is_some_and(|last| key < last) {
            break;
        }
        last_key = Some(key);
        repaired_count += 1;
    }
    let repaired_len = prefix_len + repaired_count * u64::from(header.frame_length);
    file.set_len(repaired_len)?;
    update_frame_count(&mut file, repaired_count)?;
    drop(file);
    verify_file(path, key_field_index)
}

#[allow(clippy::too_many_arguments)]
fn read_key(
    file: &mut File,
    prefix_len: u64,
    frame_length: u32,
    key_offset: u32,
    key_length: u16,
    key_type: KeyType,
    index: u64,
) -> Result<Key> {
    let offset = prefix_len + index * u64::from(frame_length) + u64::from(key_offset);
    file.seek(SeekFrom::Start(offset))?;
    let mut bytes = vec![0; key_length as usize];
    file.read_exact(&mut bytes)?;
    Key::decode(key_type, &bytes).map_err(Into::into)
}

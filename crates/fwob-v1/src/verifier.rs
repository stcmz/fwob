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

/// Target byte size of each bulk read backing the repair key scan.
const REPAIR_CHUNK_BYTES: usize = 256 * 1024;

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
    let frame_len = header.frame_length as usize;
    let key_start = key_field.offset as usize;
    let key_end = key_start + key_field.length as usize;
    let batch = (REPAIR_CHUNK_BYTES / frame_len.max(1)).max(1);
    let mut last_key = None;

    // Committed frames must be ordered: any violation is corruption.
    let mut index = 0u64;
    while index < committed_frames {
        let want = ((committed_frames - index) as usize).min(batch);
        let raw = read_frame_chunk(&mut file, prefix_len, header.frame_length, index, want)?;
        for (offset, bytes) in raw.chunks_exact(frame_len).enumerate() {
            let key = Key::decode(key_type, &bytes[key_start..key_end])?;
            if let Some(last) = last_key {
                if key < last {
                    return Err(V1Error::KeyOrderViolation {
                        index: index + offset as u64,
                        key,
                        previous: last,
                    });
                }
            }
            last_key = Some(key);
        }
        index += want as u64;
    }

    // Tail frames recover up to the first out-of-order key, then stop.
    let mut repaired_count = committed_frames;
    let mut index = committed_frames;
    'tail: while index < complete_frames {
        let want = ((complete_frames - index) as usize).min(batch);
        let raw = read_frame_chunk(&mut file, prefix_len, header.frame_length, index, want)?;
        for bytes in raw.chunks_exact(frame_len) {
            let key = Key::decode(key_type, &bytes[key_start..key_end])?;
            if last_key.is_some_and(|last| key < last) {
                break 'tail;
            }
            last_key = Some(key);
            repaired_count += 1;
        }
        index += want as u64;
    }
    let repaired_len = prefix_len + repaired_count * u64::from(header.frame_length);
    file.set_len(repaired_len)?;
    update_frame_count(&mut file, repaired_count)?;
    drop(file);
    verify_file(path, key_field_index)
}

/// Reads `count` contiguous frames starting at `start` as packed raw bytes
/// using a single seek and a single bulk read.
fn read_frame_chunk(
    file: &mut File,
    prefix_len: u64,
    frame_length: u32,
    start: u64,
    count: usize,
) -> Result<Vec<u8>> {
    let offset = prefix_len + start * u64::from(frame_length);
    file.seek(SeekFrom::Start(offset))?;
    let mut bytes = vec![0u8; count * frame_length as usize];
    file.read_exact(&mut bytes)?;
    Ok(bytes)
}

use std::{
    fs::{File, OpenOptions},
    io::{Seek, SeekFrom},
    path::Path,
};

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
    let repaired_count = header.frame_count.min(complete_frames);
    let repaired_len = prefix_len + repaired_count * u64::from(header.frame_length);
    file.set_len(repaired_len)?;
    update_frame_count(&mut file, repaired_count)?;
    drop(file);
    verify_file(path, key_field_index)
}

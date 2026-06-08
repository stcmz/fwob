use std::{fs::File, path::Path};

use crate::{Reader, Result};

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

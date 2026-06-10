use std::{fs::OpenOptions, path::Path};

use fwob_core::KeyType;

use crate::{file_header::update_counts, Reader, Result, FILE_HEADER_LEN};

pub fn repair_committed_tail(path: impl AsRef<Path>) -> Result<()> {
    let path = path.as_ref();
    let metadata_len = std::fs::metadata(path)?.len();
    let mut reader = Reader::open(path)?;
    let header = reader.header().clone();
    if header.page_size == 0 || metadata_len < FILE_HEADER_LEN {
        return Err(crate::V2Error::InvalidFileHeader);
    }

    let physical_pages = (metadata_len - FILE_HEADER_LEN) / u64::from(header.page_size);
    let candidate_pages = header.page_count.min(physical_pages);
    let key_type = KeyType::from_field(header.schema.key_field())?;
    let mut valid_pages = 0u64;
    let mut frame_count = 0u64;
    let mut last_key = None;

    for page_index in 0..candidate_pages {
        let frames = match reader.read_page_frames(page_index) {
            Ok(frames) => frames,
            Err(_) => break,
        };
        let mut page_valid = true;
        for frame in &frames {
            let key = frame.as_ref().key(&header.schema, key_type)?;
            if last_key.is_some_and(|last| key < last) {
                page_valid = false;
                break;
            }
            last_key = Some(key);
        }
        if !page_valid {
            break;
        }
        frame_count += frames.len() as u64;
        valid_pages += 1;
    }
    drop(reader);

    let repaired_len = FILE_HEADER_LEN + valid_pages * u64::from(header.page_size);
    let mut file = OpenOptions::new().read(true).write(true).open(path)?;
    file.set_len(repaired_len)?;
    update_counts(&mut file, valid_pages, frame_count)?;
    drop(file);

    Reader::open(path)?.verify()
}

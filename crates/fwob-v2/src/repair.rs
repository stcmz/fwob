use std::{
    fs::OpenOptions,
    io::{Read, Seek, SeekFrom},
    path::Path,
};

use fwob_core::{FrameRef, KeyType};

use crate::{
    encoding::decode_page_payload,
    file_header::{read_file_header, update_counts},
    page::PageHeader,
    Reader, Result, FILE_HEADER_LEN,
};

pub fn repair_committed_tail(path: impl AsRef<Path>) -> Result<()> {
    let path = path.as_ref();
    let mut file = OpenOptions::new().read(true).write(true).open(path)?;
    let metadata_len = file.metadata()?.len();
    let header = read_file_header(&mut file)?;
    if header.page_size == 0 || metadata_len < FILE_HEADER_LEN {
        return Err(crate::V2Error::InvalidFileHeader);
    }

    let physical_pages = (metadata_len - FILE_HEADER_LEN) / u64::from(header.page_size);
    let key_type = KeyType::from_field(header.schema.key_field())?;
    let mut valid_pages = 0u64;
    let mut frame_count = 0u64;
    let mut last_key = None;

    for page_index in 0..physical_pages {
        file.seek(SeekFrom::Start(header.page_offset(page_index)))?;
        let page = match PageHeader::read(&mut file, page_index) {
            Ok(page) if page.first_frame_index == frame_count => page,
            _ => break,
        };
        let mut compressed = vec![0; page.compressed_len as usize];
        if file.read_exact(&mut compressed).is_err() || page.validate_payload(&compressed).is_err()
        {
            break;
        }
        let encoded = match page
            .codec
            .decompress(&compressed, page.uncompressed_len as usize)
        {
            Ok(encoded) => encoded,
            Err(_) => break,
        };
        let raw = match decode_page_payload(
            &header.schema,
            page.encoding,
            &encoded,
            page.frame_count as usize,
        ) {
            Ok(raw) => raw,
            Err(_) => break,
        };
        let mut page_valid = true;
        for bytes in raw.chunks_exact(header.schema.frame_len as usize) {
            let key = FrameRef::new(&header.schema, bytes)?.key(&header.schema, key_type)?;
            if last_key.is_some_and(|last| key < last) {
                page_valid = false;
                break;
            }
            last_key = Some(key);
        }
        if !page_valid {
            break;
        }
        frame_count += u64::from(page.frame_count);
        valid_pages += 1;
    }

    let repaired_len = FILE_HEADER_LEN + valid_pages * u64::from(header.page_size);
    file.set_len(repaired_len)?;
    update_counts(&mut file, valid_pages, frame_count)?;
    drop(file);

    Reader::open(path)?.verify()
}

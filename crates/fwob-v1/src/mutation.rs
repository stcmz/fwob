use std::{
    fs::OpenOptions,
    io::{Read, Seek, SeekFrom, Write},
    ops::Range,
    path::Path,
};

use crate::{
    header::{read_header, update_frame_count},
    Result, V1Error,
};

const COPY_BUFFER_BYTES: usize = 4 * 1024 * 1024;

/// Deletes ordered, non-overlapping frame ranges by compacting only the suffix
/// beginning at the first deleted frame.
pub fn delete_frame_ranges(path: impl AsRef<Path>, ranges: &[Range<u64>]) -> Result<u64> {
    if ranges.is_empty() {
        return Ok(0);
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

    let mut previous_end = 0;
    let mut removed = 0u64;
    for range in ranges {
        if range.start < previous_end || range.start > range.end || range.end > header.frame_count {
            return Err(fwob_core::FwobError::InvalidFrameRange {
                start: range.start,
                end: range.end,
                frame_count: header.frame_count,
            }
            .into());
        }
        previous_end = range.end;
        removed += range.end - range.start;
    }
    if removed == 0 {
        return Ok(0);
    }

    let frame_len = u64::from(header.frame_length);
    let first_frame = header.first_frame_position();
    let mut read_frame = ranges[0].end;
    let mut write_frame = ranges[0].start;
    for range in &ranges[1..] {
        copy_frames(
            &mut file,
            first_frame,
            frame_len,
            read_frame..range.start,
            write_frame,
        )?;
        write_frame += range.start - read_frame;
        read_frame = range.end;
    }
    copy_frames(
        &mut file,
        first_frame,
        frame_len,
        read_frame..header.frame_count,
        write_frame,
    )?;

    let new_frame_count = header.frame_count - removed;
    file.set_len(first_frame + new_frame_count * frame_len)?;
    update_frame_count(&mut file, new_frame_count)?;
    file.flush()?;
    Ok(removed)
}

fn copy_frames(
    file: &mut std::fs::File,
    first_frame: u64,
    frame_len: u64,
    source: Range<u64>,
    destination: u64,
) -> Result<()> {
    if source.is_empty() {
        return Ok(());
    }
    let mut read_pos = first_frame + source.start * frame_len;
    let mut write_pos = first_frame + destination * frame_len;
    let mut remaining = (source.end - source.start) * frame_len;
    let mut buffer = vec![0u8; COPY_BUFFER_BYTES];
    while remaining > 0 {
        let count = remaining.min(buffer.len() as u64) as usize;
        file.seek(SeekFrom::Start(read_pos))?;
        file.read_exact(&mut buffer[..count])?;
        file.seek(SeekFrom::Start(write_pos))?;
        file.write_all(&buffer[..count])?;
        read_pos += count as u64;
        write_pos += count as u64;
        remaining -= count as u64;
    }
    Ok(())
}

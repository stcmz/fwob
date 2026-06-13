use std::{
    fs,
    ops::Range,
    path::{Path, PathBuf},
};

use fwob_core::Key;
use tempfile::NamedTempFile;

use crate::{Error, Maintenance, Reader, ReaderOptions, Result};

const COPY_BUFFER_BYTES: usize = 4 * 1024 * 1024;

#[derive(Debug, Clone, Copy, Default)]
pub struct Organizer {
    pub reader_options: ReaderOptions,
    pub keep_empty_parts: bool,
}

impl Organizer {
    pub fn split(
        &self,
        source: impl AsRef<Path>,
        output_dir: impl AsRef<Path>,
        first_keys: &[Key],
    ) -> Result<Vec<PathBuf>> {
        split_by_keys(
            source,
            output_dir,
            first_keys,
            self.reader_options,
            self.keep_empty_parts,
        )
    }

    pub fn concat(&self, destination: impl AsRef<Path>, sources: &[PathBuf]) -> Result<u64> {
        concat_files(destination, sources, self.reader_options.v1_key_field_index)
    }
}

impl fwob_core::Organizer for Organizer {
    type Error = Error;

    fn split_by_keys(
        &self,
        source: &Path,
        output_dir: &Path,
        first_keys: &[Key],
    ) -> Result<Vec<PathBuf>> {
        self.split(source, output_dir, first_keys)
    }

    fn concat(&self, destination: &Path, sources: &[PathBuf]) -> Result<u64> {
        self.concat(destination, sources)
    }
}

fn split_by_keys(
    source: impl AsRef<Path>,
    output_dir: impl AsRef<Path>,
    first_keys: &[Key],
    reader_options: ReaderOptions,
    keep_empty_parts: bool,
) -> Result<Vec<PathBuf>> {
    if first_keys.is_empty() {
        return Err(Error::EmptySplitKeys);
    }
    if first_keys.windows(2).any(|pair| pair[0] > pair[1]) {
        return Err(Error::UnsortedSplitKeys);
    }

    let source = source.as_ref();
    let output_dir = output_dir.as_ref();
    fs::create_dir_all(output_dir)?;
    let mut reader = Reader::open_with_options(source, reader_options)?;
    let title = reader.title().to_owned();
    let string_table = reader.string_table().to_vec();
    let format = reader.format_version();
    let stem = source
        .file_stem()
        .or_else(|| source.file_name())
        .and_then(|value| value.to_str())
        .unwrap_or("output");

    let mut boundaries = Vec::with_capacity(first_keys.len() + 2);
    boundaries.push(0);
    let mut lower = 0;
    for key in first_keys {
        let boundary = reader.lower_bound(*key)?.max(lower);
        boundaries.push(boundary);
        lower = boundary;
    }
    boundaries.push(reader.frame_count());

    let mut outputs = Vec::new();
    for range in boundaries.windows(2).map(|pair| pair[0]..pair[1]) {
        if range.is_empty() && !keep_empty_parts {
            continue;
        }
        let path = output_dir.join(format!("{stem}.part{}.fwob", outputs.len()));
        if format == fwob_core::FormatVersion::V1 {
            write_v1_range_atomically(source, &path, range, reader_options.v1_key_field_index)?;
        } else {
            write_range_atomically(
                &mut reader,
                &path,
                range,
                &title,
                &string_table,
                reader_options.v1_key_field_index,
            )?;
        }
        outputs.push(path);
    }
    Ok(outputs)
}

fn concat_files(
    destination: impl AsRef<Path>,
    sources: &[PathBuf],
    v1_key_field_index: usize,
) -> Result<u64> {
    if sources.is_empty() {
        return Err(Error::EmptySources);
    }

    let reader_options = ReaderOptions { v1_key_field_index };
    let mut first = Reader::open_with_options(&sources[0], reader_options)?;
    let format = first.format_version();
    let schema = first.schema().clone();
    let title = first.title().to_owned();
    let mut string_table = first.string_table().to_vec();
    let mut total_frames = first.frame_count();
    let mut previous_last = first.last_key()?;
    drop(first);

    for path in &sources[1..] {
        let mut reader = Reader::open_with_options(path, reader_options)?;
        if reader.format_version() != format {
            return Err(Error::IncompatibleFormat);
        }
        if reader.schema() != &schema {
            return Err(Error::IncompatibleSchema);
        }
        if reader.title() != title {
            return Err(Error::IncompatibleTitle);
        }
        merge_string_tables(&mut string_table, reader.string_table())?;
        if let (Some(previous), Some(next)) = (previous_last, reader.first_key()?) {
            if next < previous {
                return Err(Error::IncompatibleKeyOrder);
            }
        }
        if let Some(last) = reader.last_key()? {
            previous_last = Some(last);
        }
        total_frames = total_frames
            .checked_add(reader.frame_count())
            .ok_or_else(|| {
                Error::Core(fwob_core::FwobError::InvalidSchema(
                    "concatenated frame count overflows u64".into(),
                ))
            })?;
    }

    if format == fwob_core::FormatVersion::V1 {
        return concat_v1_raw(
            destination.as_ref(),
            sources,
            &schema,
            &title,
            &string_table,
            total_frames,
            v1_key_field_index,
        );
    }

    let destination = destination.as_ref();
    let parent = destination.parent().unwrap_or_else(|| Path::new("."));
    let temporary = NamedTempFile::new_in(parent)?;
    let temporary_path = temporary.into_temp_path();
    let mut first = Reader::open_with_options(&sources[0], reader_options)?;
    let mut writer = first.create_rewrite_writer(&temporary_path, &title, &string_table)?;
    for path in sources {
        let mut reader = Reader::open_with_options(path, reader_options)?;
        let frame_count = reader.frame_count();
        copy_range(
            &mut reader,
            &mut writer,
            0..frame_count,
            schema.frame_len as usize,
        )?;
    }
    writer.finish()?;
    Maintenance::verify(&temporary_path, ReaderOptions { v1_key_field_index })?;
    temporary_path
        .persist(destination)
        .map_err(|error| Error::Io(error.error))?;
    Ok(total_frames)
}

fn write_v1_range_atomically(
    source: &Path,
    destination: &Path,
    range: Range<u64>,
    key_field_index: usize,
) -> Result<()> {
    let parent = destination.parent().unwrap_or_else(|| Path::new("."));
    let temporary = NamedTempFile::new_in(parent)?;
    let temporary_path = temporary.into_temp_path();
    let mut source = fwob_v1::Reader::open(source, key_field_index)?;
    let header = source.header().clone();
    let schema = source.schema().clone();
    let strings = source.read_string_table()?;
    let mut options = fwob_v1::WriterOptions::new(header.title);
    options.string_table_preserved_length = header.string_table_preserved_length;
    let mut destination_writer = fwob_v1::Writer::create(&temporary_path, schema, options)?;
    for value in &strings {
        destination_writer.append_string(value)?;
    }
    copy_v1_raw_range(&mut source, &mut destination_writer, range)?;
    drop(destination_writer);
    fwob_v1::verify_file(&temporary_path, key_field_index)?;
    temporary_path
        .persist(destination)
        .map_err(|error| Error::Io(error.error))?;
    Ok(())
}

fn concat_v1_raw(
    destination: &Path,
    sources: &[PathBuf],
    schema: &fwob_core::Schema,
    title: &str,
    string_table: &[String],
    total_frames: u64,
    key_field_index: usize,
) -> Result<u64> {
    let mut preserved_length = 0u32;
    for source in sources {
        preserved_length = preserved_length.max(
            fwob_v1::Reader::open(source, key_field_index)?
                .header()
                .string_table_preserved_length,
        );
    }
    let parent = destination.parent().unwrap_or_else(|| Path::new("."));
    let temporary = NamedTempFile::new_in(parent)?;
    let temporary_path = temporary.into_temp_path();
    let mut options = fwob_v1::WriterOptions::new(title);
    options.string_table_preserved_length = preserved_length;
    let mut writer = fwob_v1::Writer::create(&temporary_path, schema.clone(), options)?;
    for value in string_table {
        writer.append_string(value)?;
    }
    for source_path in sources {
        let mut source = fwob_v1::Reader::open(source_path, key_field_index)?;
        let frame_count = source.frame_count();
        copy_v1_raw_range(&mut source, &mut writer, 0..frame_count)?;
    }
    drop(writer);
    fwob_v1::verify_file(&temporary_path, key_field_index)?;
    temporary_path
        .persist(destination)
        .map_err(|error| Error::Io(error.error))?;
    Ok(total_frames)
}

fn copy_v1_raw_range(
    source: &mut fwob_v1::Reader<std::fs::File>,
    destination: &mut fwob_v1::Writer<std::fs::File>,
    range: Range<u64>,
) -> Result<()> {
    let frame_len = source.schema().frame_len as usize;
    let frames_per_buffer = (COPY_BUFFER_BYTES / frame_len).max(1);
    let mut next = range.start;
    while next < range.end {
        let count = (range.end - next).min(frames_per_buffer as u64) as usize;
        let bytes = source.read_raw_frames_chunk(next, count)?;
        destination.append_presorted_raw_frames(&bytes)?;
        next += count as u64;
    }
    Ok(())
}

fn merge_string_tables(destination: &mut Vec<String>, source: &[String]) -> Result<()> {
    for (index, value) in source.iter().enumerate() {
        if let Some(existing) = destination.get(index) {
            if existing != value {
                return Err(Error::IncompatibleStringTable);
            }
        } else {
            destination.push(value.clone());
        }
    }
    Ok(())
}

fn write_range_atomically(
    reader: &mut Reader,
    destination: &Path,
    range: Range<u64>,
    title: &str,
    string_table: &[String],
    v1_key_field_index: usize,
) -> Result<()> {
    let parent = destination.parent().unwrap_or_else(|| Path::new("."));
    let temporary = NamedTempFile::new_in(parent)?;
    let temporary_path = temporary.into_temp_path();
    let mut writer = reader.create_rewrite_writer(&temporary_path, title, string_table)?;
    copy_range(
        reader,
        &mut writer,
        range,
        reader.schema().frame_len as usize,
    )?;
    writer.finish()?;
    Maintenance::verify(&temporary_path, ReaderOptions { v1_key_field_index })?;
    temporary_path
        .persist(destination)
        .map_err(|error| Error::Io(error.error))?;
    Ok(())
}

fn copy_range(
    source: &mut Reader,
    destination: &mut fwob_core::Writer,
    range: Range<u64>,
    frame_len: usize,
) -> Result<()> {
    let frames_per_buffer = (COPY_BUFFER_BYTES / frame_len).max(1);
    let mut next = range.start;
    let mut bytes = Vec::with_capacity(frames_per_buffer * frame_len);
    while next < range.end {
        bytes.clear();
        let end = range.end.min(next + frames_per_buffer as u64);
        for frame in source.frames(next..end)? {
            bytes.extend_from_slice(frame?.bytes());
        }
        destination.append_presorted_frames(&bytes)?;
        next = end;
    }
    Ok(())
}

use std::{
    fs,
    ops::Range,
    path::{Path, PathBuf},
};

use fwob_core::{FieldSemantic, Key, Schema};
use tempfile::NamedTempFile;

use crate::{writer::inherited_v2_options, Error, OperationOptions, Reader, Result, Writer};

const COPY_BUFFER_BYTES: usize = 4 * 1024 * 1024;

#[derive(Debug, Clone, Default)]
pub struct Organizer {
    pub operation_options: OperationOptions,
    pub keep_empty_parts: bool,
    /// Force the concat output format. `None` infers it from the sources (all-v1 -> v1, else v2).
    pub output_format: Option<fwob_core::FormatVersion>,
    /// Override split/concat v2 output page size. `None` inherits the source page size.
    pub output_page_size: Option<u32>,
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
            &self.operation_options,
            self.keep_empty_parts,
            self.output_page_size,
        )
    }

    pub fn concat(&self, destination: impl AsRef<Path>, sources: &[PathBuf]) -> Result<u64> {
        concat_files(
            destination,
            sources,
            &self.operation_options,
            self.output_format,
            self.output_page_size,
        )
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
    operation_options: &OperationOptions,
    keep_empty_parts: bool,
    output_page_size: Option<u32>,
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
    let reader_options = operation_options.reader_options;
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
                source,
                &path,
                range,
                &title,
                &string_table,
                operation_options,
                output_page_size,
            )?;
        }
        outputs.push(path);
    }
    Ok(outputs)
}

fn concat_files(
    destination: impl AsRef<Path>,
    sources: &[PathBuf],
    operation_options: &OperationOptions,
    output_format: Option<fwob_core::FormatVersion>,
    output_page_size: Option<u32>,
) -> Result<u64> {
    if sources.is_empty() {
        return Err(Error::EmptySources);
    }

    let reader_options = operation_options.reader_options;
    let v1_key_field_index = reader_options.v1_key_field_index;
    let mut first = Reader::open_with_options(&sources[0], reader_options)?;
    let schema = first.schema().clone();
    // Pick the richest compatible schema (most non-None field semantics) as the output schema so a
    // v1 source (which can't persist semantics) doesn't strip a sibling v2 source's timestamp.
    let mut output_schema = schema.clone();
    let title = first.title().to_owned();
    let mut string_table = first.string_table().to_vec();
    let mut total_frames = first.frame_count();
    let mut previous_last = first.last_key()?;
    let mut all_v1 = first.format_version() == fwob_core::FormatVersion::V1;
    let mut any_v1 = all_v1;
    drop(first);

    let mut semantic_differs = false;
    for path in &sources[1..] {
        let mut reader = Reader::open_with_options(path, reader_options)?;
        let format = reader.format_version();
        all_v1 &= format == fwob_core::FormatVersion::V1;
        any_v1 |= format == fwob_core::FormatVersion::V1;
        // Sources may mix v1 and v2; require only structural schema compatibility. v1 cannot
        // persist field semantics, so they are ignored here; a pure-v2 concat still rejects a
        // semantic mismatch below.
        if !reader.schema().is_compatible(&schema) {
            return Err(Error::IncompatibleSchema);
        }
        if reader.schema() != &schema {
            semantic_differs = true;
        }
        if schema_semantic_richness(reader.schema()) > schema_semantic_richness(&output_schema) {
            output_schema = reader.schema().clone();
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

    // When no v1 file is involved every schema is fully comparable, so a semantic mismatch is a
    // real incompatibility (consistent with append/typed checks).
    if !any_v1 && semantic_differs {
        return Err(Error::IncompatibleSchema);
    }

    // Output format: forced by the caller, otherwise inferred (all-v1 -> v1, else v2).
    let final_format = output_format.unwrap_or(if all_v1 {
        fwob_core::FormatVersion::V1
    } else {
        fwob_core::FormatVersion::V2
    });

    if final_format == fwob_core::FormatVersion::V1 {
        if all_v1 {
            // Fast path: raw v1 frame copy.
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
        // Forced v1 output from v2/mixed sources: read every source through the version-neutral
        // reader and write a v1 file (semantics are dropped, which v1 cannot store anyway).
        return concat_to_v1(
            destination.as_ref(),
            sources,
            &output_schema,
            &title,
            &string_table,
            total_frames,
            reader_options,
        );
    }

    // v2 output (all-v2, mixed, or forced): read each source through the version-neutral reader.
    // Frame bytes are identical across formats, so mixing is lossless.
    let destination = destination.as_ref();
    let parent = destination.parent().unwrap_or_else(|| Path::new("."));
    let temporary = NamedTempFile::new_in(parent)?;
    let temporary_path = temporary.into_temp_path();
    let options = output_v2_options(
        &sources[0],
        &title,
        &string_table,
        operation_options,
        output_page_size,
    );
    let chunk_frames = fwob_v2::recommended_input_chunk_frames(
        options.codec,
        options.encoding_selection,
        options.page_size,
        &output_schema,
    );
    let mut writer = Writer::create_v2(&temporary_path, output_schema.clone(), options)?;
    for path in sources {
        let mut reader = Reader::open_with_options(path, reader_options)?;
        let frame_count = reader.frame_count();
        copy_range(&mut reader, &mut writer, 0..frame_count, chunk_frames)?;
    }
    writer.finish()?;
    temporary_path
        .persist(destination)
        .map_err(|error| Error::Io(error.error))?;
    Ok(total_frames)
}

#[allow(clippy::too_many_arguments)]
fn concat_to_v1(
    destination: &Path,
    sources: &[PathBuf],
    schema: &Schema,
    title: &str,
    string_table: &[String],
    total_frames: u64,
    reader_options: fwob_core::ReaderOptions,
) -> Result<u64> {
    let v1_key_field_index = reader_options.v1_key_field_index;
    let mut preserved_length = string_table
        .iter()
        .map(|value| value.len() + 5)
        .sum::<usize>()
        .max(1834) as u32;
    for source in sources {
        if let Ok(reader) = fwob_v1::Reader::open(source, v1_key_field_index) {
            preserved_length = preserved_length.max(reader.header().string_table_preserved_length);
        }
    }
    let parent = destination.parent().unwrap_or_else(|| Path::new("."));
    let temporary = NamedTempFile::new_in(parent)?;
    let temporary_path = temporary.into_temp_path();
    let mut options = fwob_v1::WriterOptions::new(title);
    options.string_table_preserved_length = preserved_length;
    let mut writer = Writer::create_v1(&temporary_path, schema.clone(), options, string_table)?;
    for source in sources {
        let mut reader = Reader::open_with_options(source, reader_options)?;
        let frame_count = reader.frame_count();
        let chunk_frames = (COPY_BUFFER_BYTES / schema.frame_len as usize).max(1);
        copy_range(&mut reader, &mut writer, 0..frame_count, chunk_frames)?;
    }
    writer.finish()?;
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

fn schema_semantic_richness(schema: &Schema) -> usize {
    schema
        .fields
        .iter()
        .filter(|field| field.semantic != FieldSemantic::None)
        .count()
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

#[allow(clippy::too_many_arguments)]
fn write_range_atomically(
    reader: &mut Reader,
    source: &Path,
    destination: &Path,
    range: Range<u64>,
    title: &str,
    string_table: &[String],
    operation_options: &OperationOptions,
    output_page_size: Option<u32>,
) -> Result<()> {
    let parent = destination.parent().unwrap_or_else(|| Path::new("."));
    let temporary = NamedTempFile::new_in(parent)?;
    let temporary_path = temporary.into_temp_path();
    let options = output_v2_options(
        source,
        title,
        string_table,
        operation_options,
        output_page_size,
    );
    let chunk_frames = fwob_v2::recommended_input_chunk_frames(
        options.codec,
        options.encoding_selection,
        options.page_size,
        reader.schema(),
    );
    let mut writer = Writer::create_v2(&temporary_path, reader.schema().clone(), options)?;
    copy_range(reader, &mut writer, range, chunk_frames)?;
    writer.finish()?;
    temporary_path
        .persist(destination)
        .map_err(|error| Error::Io(error.error))?;
    Ok(())
}

fn copy_range(
    source: &mut Reader,
    destination: &mut Writer,
    range: Range<u64>,
    frames_per_buffer: usize,
) -> Result<()> {
    let mut next = range.start;
    while next < range.end {
        let end = range.end.min(next + frames_per_buffer as u64);
        let bytes = source.read_raw_frames_chunk(next, (end - next) as usize)?;
        destination.append_presorted_frames_owned(bytes)?;
        next = end;
    }
    Ok(())
}

fn output_v2_options(
    source: &Path,
    title: &str,
    string_table: &[String],
    operation_options: &OperationOptions,
    page_size_override: Option<u32>,
) -> fwob_v2::WriterOptions {
    let mut options = operation_options
        .v2
        .clone()
        .unwrap_or_else(|| inherited_v2_options(source));
    // An explicit page size wins; otherwise inherit the source's (when the source is v2).
    if let Some(page_size) = page_size_override {
        options.page_size = page_size;
    } else if let Ok(reader) = fwob_v2::Reader::open(source) {
        options.page_size = reader.header().page_size;
    }
    options.title = title.to_owned();
    options.string_table = string_table.to_vec();
    options
}

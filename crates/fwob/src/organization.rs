use std::{
    fs::{self, File},
    ops::Range,
    path::{Path, PathBuf},
};

use fwob_core::{Key, Schema};
use tempfile::NamedTempFile;

use crate::{AnyReader, Error, FormatVersion, FwobFile, FwobReader, Result};

const COPY_BUFFER_BYTES: usize = 4 * 1024 * 1024;

#[derive(Debug, Clone, Copy)]
pub struct SplitOptions {
    pub ignore_empty_parts: bool,
    pub v1_key_field_index: usize,
}

impl Default for SplitOptions {
    fn default() -> Self {
        Self {
            ignore_empty_parts: true,
            v1_key_field_index: 0,
        }
    }
}

pub fn split_by_keys(
    source: impl AsRef<Path>,
    output_dir: impl AsRef<Path>,
    first_keys: &[Key],
    options: SplitOptions,
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
    let mut reader = AnyReader::open_with_v1_key(source, options.v1_key_field_index)?;
    let settings = OutputSettings::from_reader(&mut reader)?;
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
        if range.is_empty() && options.ignore_empty_parts {
            continue;
        }
        let path = output_dir.join(format!("{stem}.part{}.fwob", outputs.len()));
        write_range_atomically(
            &mut reader,
            &path,
            range,
            &settings,
            options.v1_key_field_index,
        )?;
        outputs.push(path);
    }
    Ok(outputs)
}

pub fn concat_files(
    destination: impl AsRef<Path>,
    sources: &[PathBuf],
    v1_key_field_index: usize,
) -> Result<u64> {
    if sources.is_empty() {
        return Err(Error::EmptySources);
    }

    let mut first = AnyReader::open_with_v1_key(&sources[0], v1_key_field_index)?;
    let mut settings = OutputSettings::from_reader(&mut first)?;
    let mut total_frames = first.frame_count();
    let mut previous_last = first.last_key()?;
    drop(first);

    for path in &sources[1..] {
        let mut reader = AnyReader::open_with_v1_key(path, v1_key_field_index)?;
        if reader.format_version() != settings.format {
            return Err(Error::IncompatibleFormat);
        }
        if reader.schema() != &settings.schema {
            return Err(Error::IncompatibleSchema);
        }
        if reader.title() != settings.title {
            return Err(Error::IncompatibleTitle);
        }
        merge_string_tables(&mut settings.string_table, reader.string_table())?;
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
        if let OutputKind::V1 { preserved_length } = &mut settings.kind {
            if let AnyReader::V1 { reader, .. } = &reader {
                *preserved_length =
                    (*preserved_length).max(reader.header().string_table_preserved_length);
            }
        }
    }

    let destination = destination.as_ref();
    let parent = destination.parent().unwrap_or_else(|| Path::new("."));
    let temporary = NamedTempFile::new_in(parent)?;
    let temporary_path = temporary.into_temp_path();
    let mut writer = settings.create_writer(&temporary_path)?;
    for path in sources {
        let mut reader = AnyReader::open_with_v1_key(path, v1_key_field_index)?;
        let frame_count = reader.frame_count();
        copy_range(
            &mut reader,
            &mut writer,
            0..frame_count,
            settings.schema.frame_len as usize,
        )?;
    }
    writer.finish()?;
    verify_file(&temporary_path, settings.format, v1_key_field_index)?;
    temporary_path
        .persist(destination)
        .map_err(|error| Error::Io(error.error))?;
    Ok(total_frames)
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

enum OutputKind {
    V1 { preserved_length: u32 },
    V2(fwob_v2::WriterOptions),
}

struct OutputSettings {
    format: FormatVersion,
    schema: Schema,
    title: String,
    string_table: Vec<String>,
    kind: OutputKind,
}

impl OutputSettings {
    fn from_reader(reader: &mut AnyReader) -> Result<Self> {
        let format = reader.format_version();
        let schema = reader.schema().clone();
        let title = reader.title().to_owned();
        let string_table = reader.string_table().to_vec();
        let kind = match reader {
            AnyReader::V1 { reader, .. } => OutputKind::V1 {
                preserved_length: reader.header().string_table_preserved_length,
            },
            AnyReader::V2(reader) => {
                let mut options = fwob_v2::WriterOptions::new(title.clone());
                options.page_size = reader.header().page_size;
                options.string_table = string_table.clone();
                if reader.header().page_count > 0 {
                    let first = reader.read_page_header(0)?;
                    options.codec = first.codec;
                    options.codec_selection = fwob_v2::CodecSelection::Fixed(first.codec);
                    options.encoding = first.encoding;
                    options.encoding_selection = fwob_v2::EncodingSelection::Fixed(first.encoding);
                }
                OutputKind::V2(options)
            }
        };
        Ok(Self {
            format,
            schema,
            title,
            string_table,
            kind,
        })
    }

    fn create_writer(&self, path: &Path) -> Result<OutputWriter> {
        match &self.kind {
            OutputKind::V1 { preserved_length } => {
                let mut options = fwob_v1::WriterOptions::new(self.title.clone());
                let required = self
                    .string_table
                    .iter()
                    .map(|value| value.len().saturating_add(5))
                    .sum::<usize>();
                options.string_table_preserved_length =
                    (*preserved_length).max(u32::try_from(required).unwrap_or(u32::MAX));
                let mut writer = fwob_v1::Writer::create(path, self.schema.clone(), options)?;
                for value in &self.string_table {
                    writer.append_string(value)?;
                }
                Ok(OutputWriter::V1(Box::new(writer)))
            }
            OutputKind::V2(options) => {
                let mut options = options.clone();
                options.title = self.title.clone();
                options.string_table = self.string_table.clone();
                Ok(OutputWriter::V2(Box::new(fwob_v2::Writer::create(
                    path,
                    self.schema.clone(),
                    options,
                )?)))
            }
        }
    }
}

enum OutputWriter {
    V1(Box<fwob_v1::Writer<File>>),
    V2(Box<fwob_v2::Writer<File>>),
}

impl OutputWriter {
    fn append(&mut self, bytes: &[u8]) -> Result<()> {
        match self {
            Self::V1(writer) => Ok(writer.append_presorted_raw_frames(bytes)?),
            Self::V2(writer) => Ok(writer.append_presorted_raw_frames(bytes)?),
        }
    }

    fn finish(self) -> Result<()> {
        match self {
            Self::V1(_) => Ok(()),
            Self::V2(writer) => Ok((*writer).finish()?),
        }
    }
}

fn write_range_atomically(
    reader: &mut AnyReader,
    destination: &Path,
    range: Range<u64>,
    settings: &OutputSettings,
    v1_key_field_index: usize,
) -> Result<()> {
    let parent = destination.parent().unwrap_or_else(|| Path::new("."));
    let temporary = NamedTempFile::new_in(parent)?;
    let temporary_path = temporary.into_temp_path();
    let mut writer = settings.create_writer(&temporary_path)?;
    copy_range(
        reader,
        &mut writer,
        range,
        settings.schema.frame_len as usize,
    )?;
    writer.finish()?;
    verify_file(&temporary_path, settings.format, v1_key_field_index)?;
    temporary_path
        .persist(destination)
        .map_err(|error| Error::Io(error.error))?;
    Ok(())
}

fn copy_range(
    source: &mut AnyReader,
    destination: &mut OutputWriter,
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
        destination.append(&bytes)?;
        next = end;
    }
    Ok(())
}

fn verify_file(path: &Path, format: FormatVersion, v1_key_field_index: usize) -> Result<()> {
    match format {
        FormatVersion::V1 => {
            fwob_v1::verify_file(path, v1_key_field_index)?;
        }
        FormatVersion::V2 => {
            fwob_v2::Reader::open(path)?.verify()?;
        }
    }
    Ok(())
}

use std::{
    fs::File,
    ops::{Range, RangeInclusive},
    path::{Path, PathBuf},
};

use fwob_core::{Key, Schema};
use tempfile::NamedTempFile;

use crate::{AnyReader, Error, FormatVersion, FwobEditor, FwobFile, FwobReader, Result};

const COPY_BUFFER_BYTES: usize = 4 * 1024 * 1024;

enum RewriteOptions {
    V1(fwob_v1::WriterOptions),
    V2(fwob_v2::WriterOptions),
}

pub struct AnyEditor {
    path: PathBuf,
    format_version: FormatVersion,
    schema: Schema,
    title: String,
    string_table: Vec<String>,
    frame_count: u64,
    v1_key_field_index: usize,
    rewrite_options: RewriteOptions,
}

impl AnyEditor {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        Self::open_with_v1_key(path, 0)
    }

    pub fn open_with_v1_key(path: impl AsRef<Path>, v1_key_field_index: usize) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        let mut reader = AnyReader::open_with_v1_key(&path, v1_key_field_index)?;
        let format_version = reader.format_version();
        let schema = reader.schema().clone();
        let title = reader.title().to_owned();
        let string_table = reader.string_table().to_vec();
        let frame_count = reader.frame_count();
        let rewrite_options = match &mut reader {
            AnyReader::V1 { reader, .. } => {
                let mut options = fwob_v1::WriterOptions::new(title.clone());
                options.string_table_preserved_length =
                    reader.header().string_table_preserved_length;
                RewriteOptions::V1(options)
            }
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
                RewriteOptions::V2(options)
            }
        };
        Ok(Self {
            path,
            format_version,
            schema,
            title,
            string_table,
            frame_count,
            v1_key_field_index,
            rewrite_options,
        })
    }

    fn rewrite_without(&mut self, deleted: Range<u64>) -> Result<u64> {
        if deleted.start > deleted.end || deleted.end > self.frame_count {
            return Err(Error::InvalidFrameRange {
                start: deleted.start,
                end: deleted.end,
                frame_count: self.frame_count,
            });
        }
        let removed = deleted.end - deleted.start;
        if removed == 0 {
            return Ok(0);
        }

        let parent = self.path.parent().unwrap_or_else(|| Path::new("."));
        let temporary = NamedTempFile::new_in(parent)?;
        let temporary_path = temporary.into_temp_path();
        let mut source = AnyReader::open_with_v1_key(&self.path, self.v1_key_field_index)?;
        let mut destination = RewriteWriter::create(
            &temporary_path,
            self.schema.clone(),
            &self.string_table,
            &self.rewrite_options,
        )?;

        copy_range(
            &mut source,
            &mut destination,
            0..deleted.start,
            self.schema.frame_len as usize,
        )?;
        copy_range(
            &mut source,
            &mut destination,
            deleted.end..self.frame_count,
            self.schema.frame_len as usize,
        )?;
        destination.finish()?;
        drop(source);

        verify_file(
            &temporary_path,
            self.format_version,
            self.v1_key_field_index,
        )?;
        temporary_path
            .persist(&self.path)
            .map_err(|error| Error::Io(error.error))?;
        self.frame_count -= removed;
        Ok(removed)
    }
}

impl FwobFile for AnyEditor {
    fn format_version(&self) -> FormatVersion {
        self.format_version
    }

    fn schema(&self) -> &Schema {
        &self.schema
    }

    fn title(&self) -> &str {
        &self.title
    }

    fn frame_count(&self) -> u64 {
        self.frame_count
    }

    fn string_table(&self) -> &[String] {
        &self.string_table
    }
}

impl FwobEditor for AnyEditor {
    fn delete_frame(&mut self, index: u64) -> Result<bool> {
        if index >= self.frame_count {
            return Ok(false);
        }
        Ok(self.rewrite_without(index..index + 1)? == 1)
    }

    fn delete_frames(&mut self, range: Range<u64>) -> Result<u64> {
        self.rewrite_without(range)
    }

    fn delete_key(&mut self, key: Key) -> Result<u64> {
        let mut reader = AnyReader::open_with_v1_key(&self.path, self.v1_key_field_index)?;
        let range = reader.equal_range(key)?;
        drop(reader);
        self.rewrite_without(range)
    }

    fn delete_key_range(&mut self, range: RangeInclusive<Key>) -> Result<u64> {
        if range.start() > range.end() {
            return Ok(0);
        }
        let mut reader = AnyReader::open_with_v1_key(&self.path, self.v1_key_field_index)?;
        let start = reader.lower_bound(*range.start())?;
        let end = reader.upper_bound(*range.end())?;
        drop(reader);
        self.rewrite_without(start..end)
    }

    fn delete_all_frames(&mut self) -> Result<u64> {
        self.rewrite_without(0..self.frame_count)
    }
}

enum RewriteWriter {
    V1(Box<fwob_v1::Writer<File>>),
    V2(Box<fwob_v2::Writer<File>>),
}

impl RewriteWriter {
    fn create(
        path: &Path,
        schema: Schema,
        strings: &[String],
        options: &RewriteOptions,
    ) -> Result<Self> {
        match options {
            RewriteOptions::V1(options) => {
                let mut writer = fwob_v1::Writer::create(path, schema, options.clone())?;
                for value in strings {
                    writer.append_string(value)?;
                }
                Ok(Self::V1(Box::new(writer)))
            }
            RewriteOptions::V2(options) => Ok(Self::V2(Box::new(fwob_v2::Writer::create(
                path,
                schema,
                options.clone(),
            )?))),
        }
    }

    fn append_presorted_frames(&mut self, bytes: &[u8]) -> Result<()> {
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

fn copy_range(
    source: &mut AnyReader,
    destination: &mut RewriteWriter,
    range: Range<u64>,
    frame_len: usize,
) -> Result<()> {
    let frames_per_buffer = (COPY_BUFFER_BYTES / frame_len).max(1);
    let mut next = range.start;
    let mut bytes = Vec::with_capacity(frames_per_buffer * frame_len);
    while next < range.end {
        bytes.clear();
        let chunk_end = range.end.min(next + frames_per_buffer as u64);
        for frame in source.frames(next..chunk_end)? {
            bytes.extend_from_slice(frame?.bytes());
        }
        destination.append_presorted_frames(&bytes)?;
        next = chunk_end;
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

use std::{fs::File, ops::Range, path::Path};

use fwob_core::{
    FileInfo, FormatVersion, Key, Maintenance, OpenOptions, OwnedFrame, Reader as CoreReader,
    ReaderBackend, Result as CoreResult, Schema, VerificationReport, Writer as CoreWriter,
    WriterBackend,
};

use crate::{Reader, Writer, WriterOptions};

pub struct ReaderAdapter {
    reader: Reader<File>,
    string_table: Vec<String>,
}

impl ReaderAdapter {
    pub fn open(path: impl AsRef<Path>, key_field_index: usize) -> crate::Result<Self> {
        let mut reader = Reader::open(path, key_field_index)?;
        let string_table = reader.read_string_table()?;
        Ok(Self {
            reader,
            string_table,
        })
    }
}

impl FileInfo for ReaderAdapter {
    fn format_version(&self) -> FormatVersion {
        FormatVersion::V1
    }

    fn schema(&self) -> &Schema {
        self.reader.schema()
    }

    fn title(&self) -> &str {
        &self.reader.header().title
    }

    fn frame_count(&self) -> u64 {
        self.reader.frame_count()
    }

    fn string_table(&self) -> &[String] {
        &self.string_table
    }
}

impl ReaderBackend for ReaderAdapter {
    fn read_frame(&mut self, index: u64) -> CoreResult<Option<OwnedFrame>> {
        self.reader
            .read_frame_at(index)
            .map_err(fwob_core::FwobError::backend)
    }

    fn read_key(&mut self, index: u64) -> CoreResult<Option<Key>> {
        self.reader
            .read_key_at(index)
            .map_err(fwob_core::FwobError::backend)
    }

    fn lower_bound(&mut self, key: Key) -> CoreResult<u64> {
        self.reader
            .lower_bound(key)
            .map_err(fwob_core::FwobError::backend)
    }

    fn upper_bound(&mut self, key: Key) -> CoreResult<u64> {
        self.reader
            .upper_bound(key)
            .map_err(fwob_core::FwobError::backend)
    }

    fn equal_range(&mut self, key: Key) -> CoreResult<Range<u64>> {
        let (start, end) = self
            .reader
            .equal_range(key)
            .map_err(fwob_core::FwobError::backend)?;
        Ok(start..end)
    }

    fn create_compatible_writer(
        &mut self,
        path: &Path,
        title: &str,
        string_table: &[String],
    ) -> CoreResult<CoreWriter> {
        let required = string_table
            .iter()
            .map(|value| value.len().saturating_add(5))
            .sum::<usize>();
        let mut options = WriterOptions::new(title);
        options.string_table_preserved_length = self
            .reader
            .header()
            .string_table_preserved_length
            .max(u32::try_from(required).unwrap_or(u32::MAX));
        create_writer(path, self.reader.schema().clone(), options, string_table)
            .map_err(fwob_core::FwobError::backend)
    }
}

pub struct WriterAdapter {
    writer: Writer<File>,
    string_table: Vec<String>,
}

impl WriterAdapter {
    pub fn create(
        path: impl AsRef<Path>,
        schema: Schema,
        options: WriterOptions,
        string_table: &[String],
    ) -> crate::Result<Self> {
        let mut writer = Writer::create(path, schema, options)?;
        for value in string_table {
            writer.append_string(value)?;
        }
        Ok(Self {
            writer,
            string_table: string_table.to_vec(),
        })
    }

    pub fn open_append(path: impl AsRef<Path>, key_field_index: usize) -> crate::Result<Self> {
        let mut reader = Reader::open(path.as_ref(), key_field_index)?;
        let string_table = reader.read_string_table()?;
        drop(reader);
        Ok(Self {
            writer: Writer::open_append(path, key_field_index)?,
            string_table,
        })
    }
}

impl FileInfo for WriterAdapter {
    fn format_version(&self) -> FormatVersion {
        FormatVersion::V1
    }

    fn schema(&self) -> &Schema {
        self.writer.schema()
    }

    fn title(&self) -> &str {
        &self.writer.header().title
    }

    fn frame_count(&self) -> u64 {
        self.writer.frame_count()
    }

    fn string_table(&self) -> &[String] {
        &self.string_table
    }
}

impl WriterBackend for WriterAdapter {
    fn append_frame(&mut self, frame: &[u8]) -> CoreResult<()> {
        self.writer
            .append_frame(frame)
            .map_err(fwob_core::FwobError::backend)
    }

    fn append_presorted_frames(&mut self, frames: &[u8]) -> CoreResult<()> {
        self.writer
            .append_presorted_raw_frames(frames)
            .map_err(fwob_core::FwobError::backend)
    }

    fn finish(self: Box<Self>) -> CoreResult<()> {
        Ok(())
    }
}

pub fn open_reader(path: impl AsRef<Path>, key_field_index: usize) -> crate::Result<CoreReader> {
    Ok(CoreReader::from_backend(ReaderAdapter::open(
        path,
        key_field_index,
    )?))
}

pub fn create_writer(
    path: impl AsRef<Path>,
    schema: Schema,
    options: WriterOptions,
    string_table: &[String],
) -> crate::Result<CoreWriter> {
    Ok(CoreWriter::from_backend(WriterAdapter::create(
        path,
        schema,
        options,
        string_table,
    )?))
}

pub fn open_writer(path: impl AsRef<Path>, key_field_index: usize) -> crate::Result<CoreWriter> {
    Ok(CoreWriter::from_backend(WriterAdapter::open_append(
        path,
        key_field_index,
    )?))
}

#[derive(Debug, Default, Clone, Copy)]
pub struct MaintenanceService;

impl Maintenance for MaintenanceService {
    fn format_version(&self) -> FormatVersion {
        FormatVersion::V1
    }

    fn light_verify(&self, path: &Path, options: OpenOptions) -> CoreResult<VerificationReport> {
        let reader = ReaderAdapter::open(path, options.v1_key_field_index)
            .map_err(fwob_core::FwobError::backend)?;
        Ok(VerificationReport {
            format_version: FormatVersion::V1,
            frame_count: reader.frame_count(),
            string_count: reader.string_table().len() as u32,
            file_length: std::fs::metadata(path)?.len(),
        })
    }

    fn verify(&self, path: &Path, options: OpenOptions) -> CoreResult<VerificationReport> {
        self.light_verify(path, options)?;
        let report = crate::verify_file(path, options.v1_key_field_index)
            .map_err(fwob_core::FwobError::backend)?;
        Ok(VerificationReport {
            format_version: FormatVersion::V1,
            frame_count: report.frame_count,
            string_count: report.string_count,
            file_length: report.file_length,
        })
    }

    fn repair(&self, path: &Path, options: OpenOptions) -> CoreResult<VerificationReport> {
        let report = crate::repair_committed_tail(path, options.v1_key_field_index)
            .map_err(fwob_core::FwobError::backend)?;
        Ok(VerificationReport {
            format_version: FormatVersion::V1,
            frame_count: report.frame_count,
            string_count: report.string_count,
            file_length: report.file_length,
        })
    }
}

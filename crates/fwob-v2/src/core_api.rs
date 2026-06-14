use std::{fs::File, ops::Range, path::Path};

use fwob_core::{
    FileInfo, FormatVersion, Key, Maintenance, OwnedFrame, Reader as CoreReader, ReaderBackend,
    ReaderOptions, Result as CoreResult, Schema, VerificationReport, Writer as CoreWriter,
    WriterBackend, WriterFactory,
};

use crate::{CodecSelection, EncodingSelection, Reader, Writer, WriterOptions, FILE_HEADER_LEN};

pub struct ReaderAdapter {
    reader: Reader<File>,
}

impl ReaderAdapter {
    pub fn open(path: impl AsRef<Path>) -> crate::Result<(Self, CompatibleWriterFactory)> {
        let mut reader = Reader::open(path)?;
        let header = reader.header().clone();
        let mut options = WriterOptions::new("");
        options.page_size = header.page_size;
        if header.page_count > 0 {
            let first = reader.read_page_header(0)?;
            options.codec = first.codec;
            options.codec_selection = CodecSelection::Fixed(first.codec);
            options.encoding = first.encoding;
            options.encoding_selection = EncodingSelection::Fixed(first.encoding);
        }
        Ok((
            Self { reader },
            CompatibleWriterFactory {
                schema: header.schema,
                options,
            },
        ))
    }
}

impl FileInfo for ReaderAdapter {
    fn format_version(&self) -> FormatVersion {
        FormatVersion::V2
    }

    fn schema(&self) -> &Schema {
        &self.reader.header().schema
    }

    fn title(&self) -> &str {
        &self.reader.header().title
    }

    fn frame_count(&self) -> u64 {
        self.reader.header().frame_count
    }

    fn string_table(&self) -> &[String] {
        &self.reader.header().string_table
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

    fn first_frame(&mut self) -> CoreResult<Option<OwnedFrame>> {
        self.reader
            .first_frame()
            .map_err(fwob_core::FwobError::backend)
    }

    fn last_frame(&mut self) -> CoreResult<Option<OwnedFrame>> {
        self.reader
            .last_frame()
            .map_err(fwob_core::FwobError::backend)
    }

    fn first_key(&mut self) -> CoreResult<Option<Key>> {
        self.reader
            .first_key()
            .map_err(fwob_core::FwobError::backend)
    }

    fn last_key(&mut self) -> CoreResult<Option<Key>> {
        self.reader
            .last_key()
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
}

pub struct CompatibleWriterFactory {
    schema: Schema,
    options: WriterOptions,
}

impl WriterFactory for CompatibleWriterFactory {
    fn create(
        &mut self,
        path: &Path,
        title: &str,
        string_table: &[String],
    ) -> CoreResult<CoreWriter> {
        let mut options = self.options.clone();
        options.title = title.to_owned();
        options.string_table = string_table.to_vec();
        create_writer(path, self.schema.clone(), options).map_err(fwob_core::FwobError::backend)
    }
}

pub struct WriterAdapter {
    writer: Writer<File>,
}

impl WriterAdapter {
    pub fn create(
        path: impl AsRef<Path>,
        schema: Schema,
        options: WriterOptions,
    ) -> crate::Result<Self> {
        Ok(Self {
            writer: Writer::create(path, schema, options)?,
        })
    }

    pub fn open_append(path: impl AsRef<Path>, options: WriterOptions) -> crate::Result<Self> {
        Ok(Self {
            writer: Writer::open_append(path, options)?,
        })
    }
}

impl FileInfo for WriterAdapter {
    fn format_version(&self) -> FormatVersion {
        FormatVersion::V2
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
        &self.writer.header().string_table
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

    fn append_frames_transactional(&mut self, frames: &[u8]) -> CoreResult<()> {
        self.writer
            .append_raw_frames(frames)
            .map_err(fwob_core::FwobError::backend)
    }

    fn finish(self: Box<Self>) -> CoreResult<()> {
        self.writer.finish().map_err(fwob_core::FwobError::backend)
    }
}

pub fn open_reader(path: impl AsRef<Path>) -> crate::Result<CoreReader> {
    let (reader, factory) = ReaderAdapter::open(path)?;
    Ok(CoreReader::from_parts(reader, factory))
}

pub fn create_writer(
    path: impl AsRef<Path>,
    schema: Schema,
    options: WriterOptions,
) -> crate::Result<CoreWriter> {
    Ok(CoreWriter::from_backend(WriterAdapter::create(
        path, schema, options,
    )?))
}

pub fn open_writer(path: impl AsRef<Path>, options: WriterOptions) -> crate::Result<CoreWriter> {
    Ok(CoreWriter::from_backend(WriterAdapter::open_append(
        path, options,
    )?))
}

#[derive(Debug, Default, Clone, Copy)]
pub struct MaintenanceService;

impl Maintenance for MaintenanceService {
    fn format_version(&self) -> FormatVersion {
        FormatVersion::V2
    }

    fn light_verify(&self, path: &Path, _options: ReaderOptions) -> CoreResult<VerificationReport> {
        let metadata_len = std::fs::metadata(path)?.len();
        let mut reader = Reader::open(path).map_err(fwob_core::FwobError::backend)?;
        let header = reader.header().clone();
        let expected_len = FILE_HEADER_LEN + header.page_count * u64::from(header.page_size);
        if metadata_len != expected_len {
            return Err(fwob_core::FwobError::backend(
                crate::V2Error::InvalidFileHeader,
            ));
        }
        if header.page_count > 0 {
            reader
                .read_page_header(header.page_count - 1)
                .map_err(fwob_core::FwobError::backend)?;
        }
        Ok(VerificationReport {
            format_version: FormatVersion::V2,
            frame_count: header.frame_count,
            string_count: header.string_table.len() as u32,
            file_length: metadata_len,
        })
    }

    fn verify(&self, path: &Path, options: ReaderOptions) -> CoreResult<VerificationReport> {
        let report = self.light_verify(path, options)?;
        Reader::open(path)
            .map_err(fwob_core::FwobError::backend)?
            .verify()
            .map_err(fwob_core::FwobError::backend)?;
        Ok(report)
    }

    fn repair(&self, path: &Path, options: ReaderOptions) -> CoreResult<VerificationReport> {
        crate::repair_committed_tail(path).map_err(fwob_core::FwobError::backend)?;
        self.verify(path, options)
    }
}

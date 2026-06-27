use std::path::Path;

use fwob_core::{FormatVersion, ReaderOptions, Schema};

use crate::{detect_format, DeletionPacking, Result};

#[derive(Debug, Clone)]
pub struct OperationOptions {
    pub reader_options: ReaderOptions,
    pub v2: Option<fwob_v2::WriterOptions>,
    pub deletion_packing: DeletionPacking,
}

impl Default for OperationOptions {
    fn default() -> Self {
        Self {
            reader_options: ReaderOptions::default(),
            v2: None,
            deletion_packing: DeletionPacking::LocalRepack,
        }
    }
}

pub type WriterOpenOptions = OperationOptions;
pub type MutationOptions = OperationOptions;

pub struct Writer {
    inner: fwob_core::Writer,
}

impl Writer {
    pub fn create_v1(
        path: impl AsRef<Path>,
        schema: Schema,
        options: fwob_v1::WriterOptions,
        strings: &[String],
    ) -> Result<Self> {
        Ok(Self {
            inner: fwob_v1::create_core_writer(path, schema, options, strings)?,
        })
    }

    pub fn create_v2(
        path: impl AsRef<Path>,
        schema: Schema,
        options: fwob_v2::WriterOptions,
    ) -> Result<Self> {
        Ok(Self {
            inner: fwob_v2::create_core_writer(path, schema, options)?,
        })
    }

    pub fn open(path: impl AsRef<Path>, options: OperationOptions) -> Result<Self> {
        let path = path.as_ref();
        let inner = match detect_format(path)? {
            FormatVersion::V1 => {
                fwob_v1::open_core_writer(path, options.reader_options.v1_key_field_index)?
            }
            FormatVersion::V2 => {
                let v2 = options.v2.unwrap_or_else(|| inherited_v2_options(path));
                fwob_v2::open_core_writer(path, v2)?
            }
        };
        Ok(Self { inner })
    }

    pub fn finish(self) -> Result<()> {
        Ok(self.inner.finish()?)
    }

    pub fn format_version(&self) -> FormatVersion {
        self.inner.format_version()
    }

    pub fn schema(&self) -> &Schema {
        self.inner.schema()
    }

    pub fn title(&self) -> &str {
        self.inner.title()
    }

    pub fn frame_count(&self) -> u64 {
        self.inner.frame_count()
    }

    pub fn string_table(&self) -> &[String] {
        self.inner.string_table()
    }

    pub fn append_frame(&mut self, frame: &[u8]) -> Result<()> {
        Ok(self.inner.append_frame(frame)?)
    }

    pub fn append_presorted_frames(&mut self, frames: &[u8]) -> Result<()> {
        Ok(self.inner.append_presorted_frames(frames)?)
    }

    pub fn append_presorted_frames_owned(&mut self, frames: Vec<u8>) -> Result<()> {
        Ok(self.inner.append_presorted_frames_owned(frames)?)
    }

    pub fn append_frames_transactional(&mut self, frames: &[u8]) -> Result<()> {
        Ok(self.inner.append_frames_transactional(frames)?)
    }

    /// Durably flushes appended data to disk without consuming the writer, so a reader can observe
    /// progress mid-write and a crash loses at most the data appended since the last `sync`. The
    /// eventual file is identical whether or not `sync` is called.
    pub fn sync(&mut self) -> Result<()> {
        Ok(self.inner.sync()?)
    }
}

pub(crate) fn inherited_v2_options(path: &Path) -> fwob_v2::WriterOptions {
    let Ok(mut reader) = fwob_v2::Reader::open(path) else {
        return fwob_v2::WriterOptions::new("");
    };
    let header = reader.header().clone();
    let mut options = fwob_v2::WriterOptions::new("");
    options.page_size = header.page_size;
    for page_index in (0..header.page_count).rev() {
        let Ok(page) = reader.read_page_header(page_index) else {
            break;
        };
        if page.codec != fwob_v2::Codec::None || page_index == 0 {
            options.codec = page.codec;
            options.codec_selection = fwob_v2::CodecSelection::Fixed(page.codec);
            options.encoding = page.encoding;
            options.encoding_selection = fwob_v2::EncodingSelection::Fixed(page.encoding);
            break;
        }
    }
    options
}

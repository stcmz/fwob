use std::path::Path;

use fwob_core::{FormatVersion, ReaderOptions, Schema};

use crate::{detect_format, Result};

pub struct WriterOpenOptions {
    pub reader_options: ReaderOptions,
    pub v2: fwob_v2::WriterOptions,
}

impl Default for WriterOpenOptions {
    fn default() -> Self {
        Self {
            reader_options: ReaderOptions::default(),
            v2: fwob_v2::WriterOptions::new(""),
        }
    }
}

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

    pub fn open(path: impl AsRef<Path>, options: WriterOpenOptions) -> Result<Self> {
        let path = path.as_ref();
        let inner = match detect_format(path)? {
            FormatVersion::V1 => {
                fwob_v1::open_core_writer(path, options.reader_options.v1_key_field_index)?
            }
            FormatVersion::V2 => fwob_v2::open_core_writer(path, options.v2)?,
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
}

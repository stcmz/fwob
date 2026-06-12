use std::{
    fs::File,
    io::Read,
    ops::{Range, RangeInclusive},
    path::{Path, PathBuf},
};

use fwob_core::{Key, OwnedFrame, Schema};

mod editor;
mod organization;
mod typed;

pub use editor::Editor;
pub use organization::Organizer;
pub use typed::{TypedEditor, TypedReader, TypedWriter};

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("unsupported FWOB file format: {0}")]
    UnsupportedFormat(PathBuf),
    #[error("at least one source file is required")]
    EmptySources,
    #[error("at least one split key is required")]
    EmptySplitKeys,
    #[error("split keys must be sorted")]
    UnsortedSplitKeys,
    #[error("source files use different FWOB format versions")]
    IncompatibleFormat,
    #[error("source files use incompatible schemas")]
    IncompatibleSchema,
    #[error("source files use incompatible titles")]
    IncompatibleTitle,
    #[error("source files use incompatible string tables")]
    IncompatibleStringTable,
    #[error("source frame keys are not globally ordered")]
    IncompatibleKeyOrder,
    #[error("typed frame schema does not match the file schema")]
    SchemaMismatch,
    #[error(transparent)]
    Core(#[from] fwob_core::FwobError),
    #[error(transparent)]
    V1(#[from] fwob_v1::V1Error),
    #[error(transparent)]
    V2(#[from] fwob_v2::V2Error),
}

pub use fwob_core::{FormatVersion, ReaderOptions, VerificationReport};

pub struct Reader {
    inner: fwob_core::Reader,
}

impl Reader {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        Self::open_with_options(path, ReaderOptions::default())
    }

    pub fn open_with_options(path: impl AsRef<Path>, options: ReaderOptions) -> Result<Self> {
        let path = path.as_ref();
        let inner = match detect_format(path)? {
            FormatVersion::V1 => fwob_v1::open_core_reader(path, options.v1_key_field_index)?,
            FormatVersion::V2 => fwob_v2::open_core_reader(path)?,
        };
        Ok(Self { inner })
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

    pub fn read_frame(&mut self, index: u64) -> Result<Option<OwnedFrame>> {
        Ok(self.inner.read_frame(index)?)
    }

    pub fn read_key(&mut self, index: u64) -> Result<Option<Key>> {
        Ok(self.inner.read_key(index)?)
    }

    pub fn first_frame(&mut self) -> Result<Option<OwnedFrame>> {
        Ok(self.inner.first_frame()?)
    }

    pub fn last_frame(&mut self) -> Result<Option<OwnedFrame>> {
        Ok(self.inner.last_frame()?)
    }

    pub fn first_key(&mut self) -> Result<Option<Key>> {
        Ok(self.inner.first_key()?)
    }

    pub fn last_key(&mut self) -> Result<Option<Key>> {
        Ok(self.inner.last_key()?)
    }

    pub fn lower_bound(&mut self, key: Key) -> Result<u64> {
        Ok(self.inner.lower_bound(key)?)
    }

    pub fn upper_bound(&mut self, key: Key) -> Result<u64> {
        Ok(self.inner.upper_bound(key)?)
    }

    pub fn equal_range(&mut self, key: Key) -> Result<Range<u64>> {
        Ok(self.inner.equal_range(key)?)
    }

    pub fn frames(
        &mut self,
        range: Range<u64>,
    ) -> Result<Box<dyn Iterator<Item = Result<OwnedFrame>> + '_>> {
        Ok(Box::new(
            self.inner
                .frames(range)?
                .map(|frame| frame.map_err(Into::into)),
        ))
    }

    pub fn frames_by_key(
        &mut self,
        range: RangeInclusive<Key>,
    ) -> Result<Box<dyn Iterator<Item = Result<OwnedFrame>> + '_>> {
        Ok(Box::new(
            self.inner
                .frames_by_key(range)?
                .map(|frame| frame.map_err(Into::into)),
        ))
    }

    pub fn read_all_frames(&mut self) -> Result<Vec<OwnedFrame>> {
        Ok(self.inner.read_all_frames()?)
    }

    pub(crate) fn create_rewrite_writer(
        &mut self,
        path: &Path,
        title: &str,
        string_table: &[String],
    ) -> Result<fwob_core::Writer> {
        Ok(self
            .inner
            .create_rewrite_writer(path, title, string_table)?)
    }
}

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

pub fn detect_format(path: impl AsRef<Path>) -> Result<FormatVersion> {
    let path = path.as_ref();
    let mut file = File::open(path)?;
    let mut magic = [0u8; 4];
    file.read_exact(&mut magic)?;
    if &magic == fwob_v1::SIGNATURE {
        Ok(FormatVersion::V1)
    } else if &magic == fwob_v2::MAGIC {
        Ok(FormatVersion::V2)
    } else {
        Err(Error::UnsupportedFormat(path.to_path_buf()))
    }
}

pub struct Maintenance;

impl Maintenance {
    pub fn light_verify(
        path: impl AsRef<Path>,
        options: ReaderOptions,
    ) -> Result<VerificationReport> {
        let path = path.as_ref();
        match detect_format(path)? {
            FormatVersion::V1 => Ok(fwob_core::Maintenance::light_verify(
                &fwob_v1::MaintenanceService,
                path,
                options,
            )?),
            FormatVersion::V2 => Ok(fwob_core::Maintenance::light_verify(
                &fwob_v2::MaintenanceService,
                path,
                options,
            )?),
        }
    }

    pub fn verify(path: impl AsRef<Path>, options: ReaderOptions) -> Result<VerificationReport> {
        let path = path.as_ref();
        match detect_format(path)? {
            FormatVersion::V1 => Ok(fwob_core::Maintenance::verify(
                &fwob_v1::MaintenanceService,
                path,
                options,
            )?),
            FormatVersion::V2 => Ok(fwob_core::Maintenance::verify(
                &fwob_v2::MaintenanceService,
                path,
                options,
            )?),
        }
    }

    pub fn repair(path: impl AsRef<Path>, options: ReaderOptions) -> Result<VerificationReport> {
        let path = path.as_ref();
        match detect_format(path)? {
            FormatVersion::V1 => Ok(fwob_core::Maintenance::repair(
                &fwob_v1::MaintenanceService,
                path,
                options,
            )?),
            FormatVersion::V2 => Ok(fwob_core::Maintenance::repair(
                &fwob_v2::MaintenanceService,
                path,
                options,
            )?),
        }
    }
}

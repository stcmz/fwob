use std::{
    fs::File,
    io::Read,
    ops::{Range, RangeInclusive},
    path::{Path, PathBuf},
};

use fwob_core::{FileInfo as _, Key, OwnedFrame, Schema};

mod editor;
mod organization;
mod typed;

pub use editor::AnyEditor;
pub use organization::{concat_files, split_by_keys, FileOrganizer, SplitOptions};
pub use typed::{TypedAppender, TypedEditor, TypedReader};

pub type Reader = AnyReader;
pub type Writer = AnyAppender;
pub type Editor = AnyEditor;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("unsupported FWOB file format: {0}")]
    UnsupportedFormat(PathBuf),
    #[error("invalid frame range {start}..{end} for {frame_count} frames")]
    InvalidFrameRange {
        start: u64,
        end: u64,
        frame_count: u64,
    },
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

pub use fwob_core::{FormatVersion, OpenOptions as VerificationOptions};

/// Version-neutral immutable file metadata.
pub trait FwobFile {
    fn format_version(&self) -> FormatVersion;
    fn schema(&self) -> &Schema;
    fn title(&self) -> &str;
    fn frame_count(&self) -> u64;
    fn string_table(&self) -> &[String];
}

/// Random and sequential logical-frame access.
///
/// Indexed frame/key reads are `O(1)` for v1 and `O(log P + D)` for v2, where
/// `P` is the number of internal storage units and `D` is one-unit decode cost.
/// Bound searches are `O(log N)` logical probes. Streams use memory bounded by
/// one decoded storage unit and do not load the complete file.
pub trait FwobReader: FwobFile {
    fn read_frame(&mut self, index: u64) -> Result<Option<OwnedFrame>>;
    fn read_key(&mut self, index: u64) -> Result<Option<Key>>;

    fn first_frame(&mut self) -> Result<Option<OwnedFrame>> {
        self.read_frame(0)
    }

    fn last_frame(&mut self) -> Result<Option<OwnedFrame>> {
        match self.frame_count().checked_sub(1) {
            Some(index) => self.read_frame(index),
            None => Ok(None),
        }
    }

    fn first_key(&mut self) -> Result<Option<Key>> {
        self.read_key(0)
    }

    fn last_key(&mut self) -> Result<Option<Key>> {
        match self.frame_count().checked_sub(1) {
            Some(index) => self.read_key(index),
            None => Ok(None),
        }
    }

    fn lower_bound(&mut self, key: Key) -> Result<u64>;
    fn upper_bound(&mut self, key: Key) -> Result<u64>;
    fn equal_range(&mut self, key: Key) -> Result<Range<u64>>;

    fn frames(
        &mut self,
        range: Range<u64>,
    ) -> Result<Box<dyn Iterator<Item = Result<OwnedFrame>> + '_>>;

    fn frames_by_key(
        &mut self,
        range: RangeInclusive<Key>,
    ) -> Result<Box<dyn Iterator<Item = Result<OwnedFrame>> + '_>>;

    fn read_all_frames(&mut self) -> Result<Vec<OwnedFrame>> {
        self.frames(0..self.frame_count())?.collect()
    }
}

/// Ordered fixed-width append operations.
///
/// A single append is amortized `O(F)` plus format encoding/compression work,
/// where `F` is frame size. Bulk append is `O(K * F)` for `K` frames and uses
/// bounded buffering in v2.
pub trait FwobAppender: FwobFile {
    fn append_frame(&mut self, frame: &[u8]) -> Result<()>;
    fn append_presorted_frames(&mut self, frames: &[u8]) -> Result<()>;
    fn finish(self: Box<Self>) -> Result<()>;
}

/// Copy-on-write logical deletion operations.
///
/// Every successful mutation rewrites retained frames in `O(N)` time with
/// bounded memory. Key selection adds `O(log N)` search work.
pub trait FwobEditor: FwobFile {
    fn delete_frame(&mut self, index: u64) -> Result<bool>;
    fn delete_frames(&mut self, range: Range<u64>) -> Result<u64>;
    fn delete_key(&mut self, key: Key) -> Result<u64>;
    fn delete_key_range(&mut self, range: RangeInclusive<Key>) -> Result<u64>;
    fn delete_all_frames(&mut self) -> Result<u64>;
    fn set_title(&mut self, title: &str) -> Result<()>;
    fn append_string(&mut self, value: &str) -> Result<u32>;
    fn replace_string_table(&mut self, strings: &[String]) -> Result<()>;

    fn clear_string_table(&mut self) -> Result<()> {
        self.replace_string_table(&[])
    }
}

pub struct AnyReader {
    inner: fwob_core::Reader,
}

impl AnyReader {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        Self::open_with_v1_key(path, 0)
    }

    pub fn open_with_v1_key(path: impl AsRef<Path>, key_field_index: usize) -> Result<Self> {
        let path = path.as_ref();
        let inner = match detect_format(path)? {
            FormatVersion::V1 => fwob_v1::open_core_reader(path, key_field_index)?,
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

    pub(crate) fn create_compatible_writer(
        &mut self,
        path: &Path,
        title: &str,
        string_table: &[String],
    ) -> Result<fwob_core::Writer> {
        Ok(self
            .inner
            .create_compatible_writer(path, title, string_table)?)
    }
}

impl FwobFile for AnyReader {
    fn format_version(&self) -> FormatVersion {
        self.inner.format_version()
    }

    fn schema(&self) -> &Schema {
        self.inner.schema()
    }

    fn title(&self) -> &str {
        self.inner.title()
    }

    fn frame_count(&self) -> u64 {
        self.inner.frame_count()
    }

    fn string_table(&self) -> &[String] {
        self.inner.string_table()
    }
}

impl FwobReader for AnyReader {
    fn read_frame(&mut self, index: u64) -> Result<Option<OwnedFrame>> {
        Ok(self.inner.read_frame(index)?)
    }

    fn read_key(&mut self, index: u64) -> Result<Option<Key>> {
        Ok(self.inner.read_key(index)?)
    }

    fn lower_bound(&mut self, key: Key) -> Result<u64> {
        Ok(self.inner.lower_bound(key)?)
    }

    fn upper_bound(&mut self, key: Key) -> Result<u64> {
        Ok(self.inner.upper_bound(key)?)
    }

    fn equal_range(&mut self, key: Key) -> Result<Range<u64>> {
        Ok(self.inner.equal_range(key)?)
    }

    fn frames(
        &mut self,
        range: Range<u64>,
    ) -> Result<Box<dyn Iterator<Item = Result<OwnedFrame>> + '_>> {
        let frame_count = self.frame_count();
        if range.start > range.end || range.end > frame_count {
            return Err(Error::InvalidFrameRange {
                start: range.start,
                end: range.end,
                frame_count,
            });
        }
        Ok(Box::new(FrameIter {
            reader: self,
            next: range.start,
            end: range.end,
        }))
    }

    fn frames_by_key(
        &mut self,
        range: RangeInclusive<Key>,
    ) -> Result<Box<dyn Iterator<Item = Result<OwnedFrame>> + '_>> {
        if range.start() > range.end() {
            return self.frames(0..0);
        }
        let start = self.lower_bound(*range.start())?;
        let end = self.upper_bound(*range.end())?;
        self.frames(start..end)
    }
}

struct FrameIter<'a> {
    reader: &'a mut AnyReader,
    next: u64,
    end: u64,
}

impl Iterator for FrameIter<'_> {
    type Item = Result<OwnedFrame>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.next >= self.end {
            return None;
        }
        let index = self.next;
        self.next += 1;
        match self.reader.read_frame(index) {
            Ok(Some(frame)) => Some(Ok(frame)),
            Ok(None) => Some(Err(Error::InvalidFrameRange {
                start: index,
                end: index + 1,
                frame_count: self.reader.frame_count(),
            })),
            Err(error) => Some(Err(error)),
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let remaining = (self.end - self.next).min(usize::MAX as u64) as usize;
        (remaining, Some(remaining))
    }
}

impl ExactSizeIterator for FrameIter<'_> {}

pub struct AppendOptions {
    pub v1_key_field_index: usize,
    pub v2: fwob_v2::WriterOptions,
}

impl Default for AppendOptions {
    fn default() -> Self {
        Self {
            v1_key_field_index: 0,
            v2: fwob_v2::WriterOptions::new(""),
        }
    }
}

pub struct AnyAppender {
    inner: fwob_core::Writer,
}

impl AnyAppender {
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

    pub fn open(path: impl AsRef<Path>, options: AppendOptions) -> Result<Self> {
        let path = path.as_ref();
        let inner = match detect_format(path)? {
            FormatVersion::V1 => fwob_v1::open_core_writer(path, options.v1_key_field_index)?,
            FormatVersion::V2 => fwob_v2::open_core_writer(path, options.v2)?,
        };
        Ok(Self { inner })
    }

    pub fn finish(self) -> Result<()> {
        Box::new(self).finish()
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

impl FwobFile for AnyAppender {
    fn format_version(&self) -> FormatVersion {
        self.inner.format_version()
    }

    fn schema(&self) -> &Schema {
        self.inner.schema()
    }

    fn title(&self) -> &str {
        self.inner.title()
    }

    fn frame_count(&self) -> u64 {
        self.inner.frame_count()
    }

    fn string_table(&self) -> &[String] {
        self.inner.string_table()
    }
}

impl FwobAppender for AnyAppender {
    fn append_frame(&mut self, frame: &[u8]) -> Result<()> {
        Ok(self.inner.append_frame(frame)?)
    }

    fn append_presorted_frames(&mut self, frames: &[u8]) -> Result<()> {
        Ok(self.inner.append_presorted_frames(frames)?)
    }

    fn finish(self: Box<Self>) -> Result<()> {
        Ok(self.inner.finish()?)
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

/// Performs header, metadata, and physical-size checks without scanning every frame.
pub fn light_verify_file(
    path: impl AsRef<Path>,
    options: VerificationOptions,
) -> Result<FormatVersion> {
    let path = path.as_ref();
    let format = detect_format(path)?;
    maintenance_for(format).light_verify(path, options)?;
    Ok(format)
}

/// Fully decodes and verifies all committed frames and their key ordering.
pub fn verify_file(path: impl AsRef<Path>, options: VerificationOptions) -> Result<FormatVersion> {
    let path = path.as_ref();
    let format = detect_format(path)?;
    maintenance_for(format).verify(path, options)?;
    Ok(format)
}

/// Truncates an interrupted write to the last committed valid boundary, then fully verifies it.
pub fn repair_file(path: impl AsRef<Path>, options: VerificationOptions) -> Result<FormatVersion> {
    let path = path.as_ref();
    let format = detect_format(path)?;
    maintenance_for(format).repair(path, options)?;
    Ok(format)
}

fn maintenance_for(format: FormatVersion) -> Box<dyn fwob_core::Maintenance> {
    match format {
        FormatVersion::V1 => Box::new(fwob_v1::MaintenanceService),
        FormatVersion::V2 => Box::new(fwob_v2::MaintenanceService),
    }
}

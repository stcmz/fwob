use std::{
    fs::File,
    io::Read,
    ops::{Range, RangeInclusive},
    path::{Path, PathBuf},
};

use fwob_core::{Key, OwnedFrame, Schema};

mod editor;
mod typed;

pub use editor::AnyEditor;
pub use typed::{TypedAppender, TypedEditor, TypedReader};

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
    #[error("typed frame schema does not match the file schema")]
    SchemaMismatch,
    #[error(transparent)]
    Core(#[from] fwob_core::FwobError),
    #[error(transparent)]
    V1(#[from] fwob_v1::V1Error),
    #[error(transparent)]
    V2(#[from] fwob_v2::V2Error),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FormatVersion {
    V1,
    V2,
}

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
}

pub enum AnyReader {
    V1 {
        reader: fwob_v1::Reader<File>,
        string_table: Vec<String>,
    },
    V2(fwob_v2::Reader<File>),
}

impl AnyReader {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        Self::open_with_v1_key(path, 0)
    }

    pub fn open_with_v1_key(path: impl AsRef<Path>, key_field_index: usize) -> Result<Self> {
        let path = path.as_ref();
        match detect_format(path)? {
            FormatVersion::V1 => {
                let mut reader = fwob_v1::Reader::open(path, key_field_index)?;
                let string_table = reader.read_string_table()?;
                Ok(Self::V1 {
                    reader,
                    string_table,
                })
            }
            FormatVersion::V2 => Ok(Self::V2(fwob_v2::Reader::open(path)?)),
        }
    }
}

impl FwobFile for AnyReader {
    fn format_version(&self) -> FormatVersion {
        match self {
            Self::V1 { .. } => FormatVersion::V1,
            Self::V2(_) => FormatVersion::V2,
        }
    }

    fn schema(&self) -> &Schema {
        match self {
            Self::V1 { reader, .. } => reader.schema(),
            Self::V2(reader) => &reader.header().schema,
        }
    }

    fn title(&self) -> &str {
        match self {
            Self::V1 { reader, .. } => &reader.header().title,
            Self::V2(reader) => &reader.header().title,
        }
    }

    fn frame_count(&self) -> u64 {
        match self {
            Self::V1 { reader, .. } => reader.frame_count(),
            Self::V2(reader) => reader.header().frame_count,
        }
    }

    fn string_table(&self) -> &[String] {
        match self {
            Self::V1 { string_table, .. } => string_table,
            Self::V2(reader) => &reader.header().string_table,
        }
    }
}

impl FwobReader for AnyReader {
    fn read_frame(&mut self, index: u64) -> Result<Option<OwnedFrame>> {
        match self {
            Self::V1 { reader, .. } => Ok(reader.read_frame_at(index)?),
            Self::V2(reader) => Ok(reader.read_frame_at(index)?),
        }
    }

    fn read_key(&mut self, index: u64) -> Result<Option<Key>> {
        match self {
            Self::V1 { reader, .. } => Ok(reader.read_key_at(index)?),
            Self::V2(reader) => Ok(reader.read_key_at(index)?),
        }
    }

    fn first_key(&mut self) -> Result<Option<Key>> {
        match self {
            Self::V1 { reader, .. } => Ok(reader.read_key_at(0)?),
            Self::V2(reader) => Ok(reader.first_key()?),
        }
    }

    fn last_key(&mut self) -> Result<Option<Key>> {
        match self {
            Self::V1 { reader, .. } => {
                let Some(index) = reader.frame_count().checked_sub(1) else {
                    return Ok(None);
                };
                Ok(reader.read_key_at(index)?)
            }
            Self::V2(reader) => Ok(reader.last_key()?),
        }
    }

    fn lower_bound(&mut self, key: Key) -> Result<u64> {
        match self {
            Self::V1 { reader, .. } => Ok(reader.lower_bound(key)?),
            Self::V2(reader) => Ok(reader.lower_bound(key)?),
        }
    }

    fn upper_bound(&mut self, key: Key) -> Result<u64> {
        match self {
            Self::V1 { reader, .. } => Ok(reader.upper_bound(key)?),
            Self::V2(reader) => Ok(reader.upper_bound(key)?),
        }
    }

    fn equal_range(&mut self, key: Key) -> Result<Range<u64>> {
        let (start, end) = match self {
            Self::V1 { reader, .. } => reader.equal_range(key)?,
            Self::V2(reader) => reader.equal_range(key)?,
        };
        Ok(start..end)
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

pub enum AnyAppender {
    V1 {
        writer: Box<fwob_v1::Writer<File>>,
        string_table: Vec<String>,
    },
    V2(Box<fwob_v2::Writer<File>>),
}

impl AnyAppender {
    pub fn create_v1(
        path: impl AsRef<Path>,
        schema: Schema,
        options: fwob_v1::WriterOptions,
        strings: &[String],
    ) -> Result<Self> {
        let mut writer = fwob_v1::Writer::create(path, schema, options)?;
        for value in strings {
            writer.append_string(value)?;
        }
        Ok(Self::V1 {
            writer: Box::new(writer),
            string_table: strings.to_vec(),
        })
    }

    pub fn create_v2(
        path: impl AsRef<Path>,
        schema: Schema,
        options: fwob_v2::WriterOptions,
    ) -> Result<Self> {
        Ok(Self::V2(Box::new(fwob_v2::Writer::create(
            path, schema, options,
        )?)))
    }

    pub fn open(path: impl AsRef<Path>, options: AppendOptions) -> Result<Self> {
        let path = path.as_ref();
        match detect_format(path)? {
            FormatVersion::V1 => {
                let mut reader = fwob_v1::Reader::open(path, options.v1_key_field_index)?;
                let string_table = reader.read_string_table()?;
                drop(reader);
                Ok(Self::V1 {
                    writer: Box::new(fwob_v1::Writer::open_append(
                        path,
                        options.v1_key_field_index,
                    )?),
                    string_table,
                })
            }
            FormatVersion::V2 => Ok(Self::V2(Box::new(fwob_v2::Writer::open_append(
                path, options.v2,
            )?))),
        }
    }

    pub fn finish(self) -> Result<()> {
        Box::new(self).finish()
    }
}

impl FwobFile for AnyAppender {
    fn format_version(&self) -> FormatVersion {
        match self {
            Self::V1 { .. } => FormatVersion::V1,
            Self::V2(_) => FormatVersion::V2,
        }
    }

    fn schema(&self) -> &Schema {
        match self {
            Self::V1 { writer, .. } => writer.schema(),
            Self::V2(writer) => writer.schema(),
        }
    }

    fn title(&self) -> &str {
        match self {
            Self::V1 { writer, .. } => &writer.header().title,
            Self::V2(writer) => &writer.header().title,
        }
    }

    fn frame_count(&self) -> u64 {
        match self {
            Self::V1 { writer, .. } => writer.frame_count(),
            Self::V2(writer) => writer.frame_count(),
        }
    }

    fn string_table(&self) -> &[String] {
        match self {
            Self::V1 { string_table, .. } => string_table,
            Self::V2(writer) => &writer.header().string_table,
        }
    }
}

impl FwobAppender for AnyAppender {
    fn append_frame(&mut self, frame: &[u8]) -> Result<()> {
        match self {
            Self::V1 { writer, .. } => Ok(writer.append_frame(frame)?),
            Self::V2(writer) => Ok(writer.append_frame(frame)?),
        }
    }

    fn append_presorted_frames(&mut self, frames: &[u8]) -> Result<()> {
        match self {
            Self::V1 { writer, .. } => Ok(writer.append_presorted_raw_frames(frames)?),
            Self::V2(writer) => Ok(writer.append_presorted_raw_frames(frames)?),
        }
    }

    fn finish(self: Box<Self>) -> Result<()> {
        match *self {
            Self::V1 { .. } => Ok(()),
            Self::V2(writer) => Ok((*writer).finish()?),
        }
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

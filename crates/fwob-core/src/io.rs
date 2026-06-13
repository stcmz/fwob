use std::{
    collections::HashMap,
    ops::{Range, RangeInclusive},
    path::Path,
    sync::OnceLock,
};

use crate::{FwobError, Key, OwnedFrame, Result, Schema};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FormatVersion {
    V1,
    V2,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct ReaderOptions {
    pub v1_key_field_index: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerificationReport {
    pub format_version: FormatVersion,
    pub frame_count: u64,
    pub string_count: u32,
    pub file_length: u64,
}

/// Immutable metadata common to every FWOB format version.
pub trait FileInfo {
    fn format_version(&self) -> FormatVersion;
    fn schema(&self) -> &Schema;
    fn title(&self) -> &str;
    fn frame_count(&self) -> u64;
    fn string_table(&self) -> &[String];
}

/// Format-specific implementation behind [`Reader`].
///
/// Implementors expose logical frames only. Physical pages and other storage
/// details remain private to the format crate.
pub trait ReaderBackend: FileInfo + Send {
    fn read_frame(&mut self, index: u64) -> Result<Option<OwnedFrame>>;
    fn read_key(&mut self, index: u64) -> Result<Option<Key>>;
    fn lower_bound(&mut self, key: Key) -> Result<u64>;
    fn upper_bound(&mut self, key: Key) -> Result<u64>;
    fn equal_range(&mut self, key: Key) -> Result<Range<u64>>;
}

/// Creates writers that preserve a source file's format-specific organization.
pub trait WriterFactory: Send {
    fn create(&mut self, path: &Path, title: &str, string_table: &[String]) -> Result<Writer>;
}

/// Version-neutral logical-frame reader.
///
/// Indexed reads are `O(1)` for v1 and `O(log P + D)` for v2, where `P` is
/// the number of physical storage units and `D` is one-unit decode cost.
/// Streams retain only the format backend's bounded read buffer.
pub struct Reader {
    inner: Box<dyn ReaderBackend>,
    writer_factory: Box<dyn WriterFactory>,
    string_indexes: OnceLock<HashMap<String, u32>>,
}

impl Reader {
    pub fn from_parts(
        inner: impl ReaderBackend + 'static,
        writer_factory: impl WriterFactory + 'static,
    ) -> Self {
        Self {
            inner: Box::new(inner),
            writer_factory: Box::new(writer_factory),
            string_indexes: OnceLock::new(),
        }
    }

    pub fn first_frame(&mut self) -> Result<Option<OwnedFrame>> {
        self.read_frame(0)
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

    pub fn string_at(&self, index: u32) -> Option<&str> {
        self.string_table().get(index as usize).map(String::as_str)
    }

    /// Returns the last index associated with `value`.
    ///
    /// The reverse index is built lazily in `O(S)` time and space, where `S`
    /// is the total string-table size. Subsequent lookups are `O(1)` average.
    pub fn string_index(&self, value: &str) -> Option<u32> {
        self.string_indexes
            .get_or_init(|| {
                self.string_table()
                    .iter()
                    .enumerate()
                    .map(|(index, value)| (value.clone(), index as u32))
                    .collect()
            })
            .get(value)
            .copied()
    }

    pub fn contains_string(&self, value: &str) -> bool {
        self.string_index(value).is_some()
    }

    pub fn last_frame(&mut self) -> Result<Option<OwnedFrame>> {
        match self.frame_count().checked_sub(1) {
            Some(index) => self.read_frame(index),
            None => Ok(None),
        }
    }

    pub fn first_key(&mut self) -> Result<Option<Key>> {
        self.read_key(0)
    }

    pub fn last_key(&mut self) -> Result<Option<Key>> {
        match self.frame_count().checked_sub(1) {
            Some(index) => self.read_key(index),
            None => Ok(None),
        }
    }

    pub fn read_frame(&mut self, index: u64) -> Result<Option<OwnedFrame>> {
        self.inner.read_frame(index)
    }

    pub fn read_key(&mut self, index: u64) -> Result<Option<Key>> {
        self.inner.read_key(index)
    }

    pub fn lower_bound(&mut self, key: Key) -> Result<u64> {
        self.inner.lower_bound(key)
    }

    pub fn upper_bound(&mut self, key: Key) -> Result<u64> {
        self.inner.upper_bound(key)
    }

    pub fn equal_range(&mut self, key: Key) -> Result<Range<u64>> {
        self.inner.equal_range(key)
    }

    pub fn frames(&mut self, range: Range<u64>) -> Result<FrameIter<'_>> {
        if range.start > range.end || range.end > self.frame_count() {
            return Err(FwobError::InvalidFrameRange {
                start: range.start,
                end: range.end,
                frame_count: self.frame_count(),
            });
        }
        Ok(FrameIter {
            reader: self,
            next: range.start,
            end: range.end,
        })
    }

    pub fn frames_by_key(&mut self, range: RangeInclusive<Key>) -> Result<FrameIter<'_>> {
        if range.start() > range.end() {
            return self.frames(0..0);
        }
        let start = self.lower_bound(*range.start())?;
        let end = self.upper_bound(*range.end())?;
        self.frames(start..end)
    }

    pub fn frames_before(&mut self, last_key: Key) -> Result<FrameIter<'_>> {
        let end = self.upper_bound(last_key)?;
        self.frames(0..end)
    }

    pub fn frames_after(&mut self, first_key: Key) -> Result<FrameIter<'_>> {
        let start = self.lower_bound(first_key)?;
        self.frames(start..self.frame_count())
    }

    pub fn frames_by_keys(&mut self, keys: &[Key]) -> Result<MultiRangeFrameIter<'_>> {
        if keys.windows(2).any(|pair| pair[0] > pair[1]) {
            return Err(FwobError::UnsortedKeys);
        }
        let mut ranges = Vec::with_capacity(keys.len());
        let mut minimum = 0;
        for key in keys {
            let mut range = self.equal_range(*key)?;
            range.start = range.start.max(minimum);
            if range.start < range.end {
                minimum = range.end;
                ranges.push(range);
            }
        }
        Ok(MultiRangeFrameIter {
            reader: self,
            ranges,
            range_index: 0,
            next: 0,
        })
    }

    pub fn read_all_frames(&mut self) -> Result<Vec<OwnedFrame>> {
        self.frames(0..self.frame_count())?.collect()
    }

    pub fn create_rewrite_writer(
        &mut self,
        path: &Path,
        title: &str,
        string_table: &[String],
    ) -> Result<Writer> {
        self.writer_factory.create(path, title, string_table)
    }
}

impl FileInfo for Reader {
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

pub struct FrameIter<'a> {
    reader: &'a mut Reader,
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
            Ok(None) => Some(Err(FwobError::InvalidFrameRange {
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

pub struct MultiRangeFrameIter<'a> {
    reader: &'a mut Reader,
    ranges: Vec<Range<u64>>,
    range_index: usize,
    next: u64,
}

impl Iterator for MultiRangeFrameIter<'_> {
    type Item = Result<OwnedFrame>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            let range = self.ranges.get(self.range_index)?;
            if self.next < range.start {
                self.next = range.start;
            }
            if self.next >= range.end {
                self.range_index += 1;
                continue;
            }
            let index = self.next;
            self.next += 1;
            return Some(match self.reader.read_frame(index) {
                Ok(Some(frame)) => Ok(frame),
                Ok(None) => Err(FwobError::InvalidFrameRange {
                    start: index,
                    end: index + 1,
                    frame_count: self.reader.frame_count(),
                }),
                Err(error) => Err(error),
            });
        }
    }
}

/// Format-specific implementation behind [`Writer`].
pub trait WriterBackend: FileInfo + Send {
    fn append_frame(&mut self, frame: &[u8]) -> Result<()>;
    fn append_presorted_frames(&mut self, frames: &[u8]) -> Result<()>;
    fn append_frames_transactional(&mut self, frames: &[u8]) -> Result<()>;
    fn finish(self: Box<Self>) -> Result<()>;
}

/// Version-neutral ordered append writer.
pub struct Writer {
    inner: Box<dyn WriterBackend>,
}

impl Writer {
    pub fn from_backend(inner: impl WriterBackend + 'static) -> Self {
        Self {
            inner: Box::new(inner),
        }
    }

    pub fn append_frame(&mut self, frame: &[u8]) -> Result<()> {
        self.inner.append_frame(frame)
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

    pub fn append_presorted_frames(&mut self, frames: &[u8]) -> Result<()> {
        self.inner.append_presorted_frames(frames)
    }

    pub fn append_frames_transactional(&mut self, frames: &[u8]) -> Result<()> {
        self.inner.append_frames_transactional(frames)
    }

    pub fn finish(self) -> Result<()> {
        self.inner.finish()
    }
}

impl FileInfo for Writer {
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

/// Physical validation and interrupted-write recovery for one format version.
pub trait Maintenance: Send + Sync {
    fn format_version(&self) -> FormatVersion;
    fn light_verify(&self, path: &Path, options: ReaderOptions) -> Result<VerificationReport>;
    fn verify(&self, path: &Path, options: ReaderOptions) -> Result<VerificationReport>;
    fn repair(&self, path: &Path, options: ReaderOptions) -> Result<VerificationReport>;
}

/// Copy-on-write mutation contract. Implementations must use bounded memory.
pub trait Editor: FileInfo {
    fn delete_frame(&mut self, index: u64) -> Result<bool>;
    fn delete_frames(&mut self, range: Range<u64>) -> Result<u64>;
    fn delete_key(&mut self, key: Key) -> Result<u64>;
    fn delete_keys(&mut self, keys: &[Key]) -> Result<u64>;
    fn delete_key_range(&mut self, range: RangeInclusive<Key>) -> Result<u64>;
    fn delete_before(&mut self, last_key: Key) -> Result<u64>;
    fn delete_after(&mut self, first_key: Key) -> Result<u64>;
    fn delete_all_frames(&mut self) -> Result<u64>;
    fn set_title(&mut self, title: &str) -> Result<()>;
    fn append_string(&mut self, value: &str) -> Result<u32>;
    fn replace_string_table(&mut self, strings: &[String]) -> Result<()>;

    fn clear_string_table(&mut self) -> Result<()> {
        self.replace_string_table(&[])
    }
}

/// Logical file organization operations. Implementations must not expose
/// physical pages or require complete files to be loaded into memory.
pub trait Organizer {
    type Error;

    fn split_by_keys(
        &self,
        source: &Path,
        output_dir: &Path,
        first_keys: &[Key],
    ) -> std::result::Result<Vec<std::path::PathBuf>, Self::Error>;

    fn concat(
        &self,
        destination: &Path,
        sources: &[std::path::PathBuf],
    ) -> std::result::Result<u64, Self::Error>;
}

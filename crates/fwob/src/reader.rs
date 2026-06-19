use std::{
    ops::{Range, RangeInclusive},
    path::Path,
};

use fwob_core::{FormatVersion, Key, OwnedFrame, ReaderOptions, Schema};

use crate::{detect_format, Result};

pub struct Reader {
    inner: fwob_core::Reader,
}

pub struct FrameIter<'a> {
    inner: fwob_core::FrameIter<'a>,
}

impl Iterator for FrameIter<'_> {
    type Item = Result<OwnedFrame>;

    fn next(&mut self) -> Option<Self::Item> {
        self.inner.next().map(|frame| frame.map_err(Into::into))
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.inner.size_hint()
    }
}

impl ExactSizeIterator for FrameIter<'_> {}

pub struct MultiRangeFrameIter<'a> {
    inner: fwob_core::MultiRangeFrameIter<'a>,
}

impl Iterator for MultiRangeFrameIter<'_> {
    type Item = Result<OwnedFrame>;

    fn next(&mut self) -> Option<Self::Item> {
        self.inner.next().map(|frame| frame.map_err(Into::into))
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.inner.size_hint()
    }
}

impl ExactSizeIterator for MultiRangeFrameIter<'_> {}

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

    pub fn string_at(&self, index: u32) -> Option<&str> {
        self.inner.string_at(index)
    }

    pub fn string_at_u64(&self, index: u64) -> Option<&str> {
        self.inner.string_at_u64(index)
    }

    pub fn string_index(&self, value: &str) -> Option<u32> {
        self.inner.string_index(value)
    }

    pub fn string_index_u64(&self, value: &str) -> Option<u64> {
        self.inner.string_index_u64(value)
    }

    pub fn contains_string(&self, value: &str) -> bool {
        self.inner.contains_string(value)
    }

    pub fn read_frame(&mut self, index: u64) -> Result<Option<OwnedFrame>> {
        Ok(self.inner.read_frame(index)?)
    }

    pub fn read_raw_frames_chunk(&mut self, start: u64, max_frames: usize) -> Result<Vec<u8>> {
        Ok(self.inner.read_raw_frames_chunk(start, max_frames)?)
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

    pub fn frames(&mut self, range: Range<u64>) -> Result<FrameIter<'_>> {
        Ok(FrameIter {
            inner: self.inner.frames(range)?,
        })
    }

    pub fn frames_by_key(&mut self, range: RangeInclusive<Key>) -> Result<FrameIter<'_>> {
        Ok(FrameIter {
            inner: self.inner.frames_by_key(range)?,
        })
    }

    pub fn frames_before(&mut self, last_key: Key) -> Result<FrameIter<'_>> {
        Ok(FrameIter {
            inner: self.inner.frames_before(last_key)?,
        })
    }

    pub fn frames_after(&mut self, first_key: Key) -> Result<FrameIter<'_>> {
        Ok(FrameIter {
            inner: self.inner.frames_after(first_key)?,
        })
    }

    pub fn frames_by_keys(&mut self, keys: &[Key]) -> Result<MultiRangeFrameIter<'_>> {
        Ok(MultiRangeFrameIter {
            inner: self.inner.frames_by_keys(keys)?,
        })
    }

    pub fn read_all_frames(&mut self) -> Result<Vec<OwnedFrame>> {
        Ok(self.inner.read_all_frames()?)
    }
}

use std::{
    marker::PhantomData,
    ops::{Range, RangeInclusive},
    path::Path,
};

use fwob_core::{FwobFrame, FwobKey, OwnedFrame};

use crate::{Editor, OperationOptions, Reader, Result, Writer};

pub struct TypedReader<F> {
    inner: Reader,
    frame: PhantomData<F>,
}

impl<F: FwobFrame> TypedReader<F> {
    pub fn new(inner: Reader) -> Result<Self> {
        ensure_schema::<F>(inner.schema())?;
        Ok(Self {
            inner,
            frame: PhantomData,
        })
    }

    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        Self::new(Reader::open(path)?)
    }

    pub fn open_with_options(
        path: impl AsRef<Path>,
        options: crate::ReaderOptions,
    ) -> Result<Self> {
        Self::new(Reader::open_with_options(path, options)?)
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

    pub fn string_index(&self, value: &str) -> Option<u32> {
        self.inner.string_index(value)
    }

    pub fn contains_string(&self, value: &str) -> bool {
        self.inner.contains_string(value)
    }

    pub fn read_frame(&mut self, index: u64) -> Result<Option<F>> {
        decode_optional(self.inner.read_frame(index)?)
    }

    pub fn first_frame(&mut self) -> Result<Option<F>> {
        decode_optional(self.inner.first_frame()?)
    }

    pub fn last_frame(&mut self) -> Result<Option<F>> {
        decode_optional(self.inner.last_frame()?)
    }

    pub fn first_key(&mut self) -> Result<Option<F::Key>> {
        self.inner
            .first_key()?
            .map(F::Key::from_key)
            .transpose()
            .map_err(Into::into)
    }

    pub fn last_key(&mut self) -> Result<Option<F::Key>> {
        self.inner
            .last_key()?
            .map(F::Key::from_key)
            .transpose()
            .map_err(Into::into)
    }

    pub fn lower_bound(&mut self, key: F::Key) -> Result<u64> {
        self.inner.lower_bound(key.into_key())
    }

    pub fn upper_bound(&mut self, key: F::Key) -> Result<u64> {
        self.inner.upper_bound(key.into_key())
    }

    pub fn equal_range(&mut self, key: F::Key) -> Result<Range<u64>> {
        self.inner.equal_range(key.into_key())
    }

    pub fn frames(
        &mut self,
        range: Range<u64>,
    ) -> Result<Box<dyn Iterator<Item = Result<F>> + '_>> {
        Ok(Box::new(
            self.inner
                .frames(range)?
                .map(|frame| frame.and_then(decode_frame::<F>)),
        ))
    }

    pub fn frames_by_key(
        &mut self,
        range: RangeInclusive<F::Key>,
    ) -> Result<Box<dyn Iterator<Item = Result<F>> + '_>> {
        let raw_range = range.start().into_key()..=range.end().into_key();
        Ok(Box::new(
            self.inner
                .frames_by_key(raw_range)?
                .map(|frame| frame.and_then(decode_frame::<F>)),
        ))
    }

    pub fn frames_before(
        &mut self,
        last_key: F::Key,
    ) -> Result<Box<dyn Iterator<Item = Result<F>> + '_>> {
        Ok(Box::new(
            self.inner
                .frames_before(last_key.into_key())?
                .map(|frame| frame.and_then(decode_frame::<F>)),
        ))
    }

    pub fn frames_after(
        &mut self,
        first_key: F::Key,
    ) -> Result<Box<dyn Iterator<Item = Result<F>> + '_>> {
        Ok(Box::new(
            self.inner
                .frames_after(first_key.into_key())?
                .map(|frame| frame.and_then(decode_frame::<F>)),
        ))
    }

    pub fn frames_by_keys(
        &mut self,
        keys: &[F::Key],
    ) -> Result<Box<dyn Iterator<Item = Result<F>> + '_>> {
        let raw_keys = keys
            .iter()
            .copied()
            .map(FwobKey::into_key)
            .collect::<Vec<_>>();
        Ok(Box::new(
            self.inner
                .frames_by_keys(&raw_keys)?
                .map(|frame| frame.and_then(decode_frame::<F>)),
        ))
    }

    pub fn into_inner(self) -> Reader {
        self.inner
    }
}

pub struct TypedWriter<F> {
    inner: Writer,
    buffer: Vec<u8>,
    frame: PhantomData<F>,
}

impl<F: FwobFrame> TypedWriter<F> {
    pub fn new(inner: Writer) -> Result<Self> {
        ensure_schema::<F>(inner.schema())?;
        Ok(Self {
            inner,
            buffer: Vec::with_capacity(F::schema().frame_len as usize),
            frame: PhantomData,
        })
    }

    pub fn open(path: impl AsRef<Path>, options: OperationOptions) -> Result<Self> {
        Self::new(Writer::open(path, options)?)
    }

    pub fn create_v1(
        path: impl AsRef<Path>,
        options: fwob_v1::WriterOptions,
        strings: &[String],
    ) -> Result<Self> {
        Self::new(Writer::create_v1(path, F::schema(), options, strings)?)
    }

    pub fn create_v2(path: impl AsRef<Path>, options: fwob_v2::WriterOptions) -> Result<Self> {
        Self::new(Writer::create_v2(path, F::schema(), options)?)
    }

    pub fn frame_count(&self) -> u64 {
        self.inner.frame_count()
    }

    pub fn append(&mut self, frame: &F) -> Result<()> {
        frame.encode(&mut self.buffer);
        self.inner.append_frame(&self.buffer)
    }

    pub fn append_all<I>(&mut self, frames: I) -> Result<u64>
    where
        I: IntoIterator<Item = F>,
    {
        let mut count = 0;
        for frame in frames {
            self.append(&frame)?;
            count += 1;
        }
        Ok(count)
    }

    pub fn append_all_transactional<I>(&mut self, frames: I) -> Result<u64>
    where
        I: IntoIterator<Item = F>,
    {
        let frame_len = F::schema().frame_len as usize;
        let mut bytes = Vec::new();
        let mut count = 0u64;
        for frame in frames {
            frame.encode(&mut self.buffer);
            bytes.extend_from_slice(&self.buffer);
            count += 1;
        }
        debug_assert_eq!(bytes.len(), count as usize * frame_len);
        self.inner.append_frames_transactional(&bytes)?;
        Ok(count)
    }

    pub fn finish(self) -> Result<()> {
        self.inner.finish()
    }
}

pub struct TypedEditor<F> {
    inner: Editor,
    frame: PhantomData<F>,
}

impl<F: FwobFrame> TypedEditor<F> {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let inner = Editor::open(path)?;
        ensure_schema::<F>(inner.schema())?;
        Ok(Self {
            inner,
            frame: PhantomData,
        })
    }

    pub fn open_with_operation_options(
        path: impl AsRef<Path>,
        options: OperationOptions,
    ) -> Result<Self> {
        let inner = Editor::open_with_operation_options(path, options)?;
        ensure_schema::<F>(inner.schema())?;
        Ok(Self {
            inner,
            frame: PhantomData,
        })
    }

    pub fn frame_count(&self) -> u64 {
        self.inner.frame_count()
    }

    pub fn delete_frame(&mut self, index: u64) -> Result<bool> {
        self.inner.delete_frame(index)
    }

    pub fn delete_frames(&mut self, range: Range<u64>) -> Result<u64> {
        self.inner.delete_frames(range)
    }

    pub fn delete_indices(&mut self, indices: &[u64]) -> Result<u64> {
        self.inner.delete_indices(indices)
    }

    pub fn delete_ranges(&mut self, ranges: &[Range<u64>]) -> Result<u64> {
        self.inner.delete_ranges(ranges)
    }

    pub fn delete_key(&mut self, key: F::Key) -> Result<u64> {
        self.inner.delete_key(key.into_key())
    }

    pub fn delete_keys(&mut self, keys: &[F::Key]) -> Result<u64> {
        let raw_keys = keys
            .iter()
            .copied()
            .map(FwobKey::into_key)
            .collect::<Vec<_>>();
        self.inner.delete_keys(&raw_keys)
    }

    pub fn delete_key_range(&mut self, range: RangeInclusive<F::Key>) -> Result<u64> {
        self.inner
            .delete_key_range(range.start().into_key()..=range.end().into_key())
    }

    pub fn delete_before(&mut self, last_key: F::Key) -> Result<u64> {
        self.inner.delete_before(last_key.into_key())
    }

    pub fn delete_after(&mut self, first_key: F::Key) -> Result<u64> {
        self.inner.delete_after(first_key.into_key())
    }

    pub fn delete_all_frames(&mut self) -> Result<u64> {
        self.inner.delete_all_frames()
    }

    pub fn set_title(&mut self, title: &str) -> Result<()> {
        self.inner.set_title(title)
    }

    pub fn append_string(&mut self, value: &str) -> Result<u32> {
        self.inner.append_string(value)
    }

    pub fn replace_string_table(&mut self, strings: &[String]) -> Result<()> {
        self.inner.replace_string_table(strings)
    }

    pub fn clear_string_table(&mut self) -> Result<()> {
        self.inner.clear_string_table()
    }
}

fn ensure_schema<F: FwobFrame>(schema: &fwob_core::Schema) -> Result<()> {
    if schema == &F::schema() {
        Ok(())
    } else {
        Err(crate::Error::SchemaMismatch)
    }
}

fn decode_optional<F: FwobFrame>(frame: Option<OwnedFrame>) -> Result<Option<F>> {
    frame.map(decode_frame::<F>).transpose()
}

fn decode_frame<F: FwobFrame>(frame: OwnedFrame) -> Result<F> {
    Ok(F::decode(frame.bytes())?)
}

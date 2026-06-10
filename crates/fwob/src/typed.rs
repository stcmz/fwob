use std::{
    marker::PhantomData,
    ops::{Range, RangeInclusive},
    path::Path,
};

use fwob_core::{FwobFrame, FwobKey, OwnedFrame};

use crate::{
    AnyAppender, AnyEditor, AnyReader, AppendOptions, FwobAppender, FwobEditor, FwobFile,
    FwobReader, Result,
};

pub struct TypedReader<R, F> {
    inner: R,
    frame: PhantomData<F>,
}

impl<R, F> TypedReader<R, F>
where
    R: FwobReader,
    F: FwobFrame,
{
    pub fn new(inner: R) -> Result<Self> {
        ensure_schema::<F>(&inner)?;
        Ok(Self {
            inner,
            frame: PhantomData,
        })
    }

    pub fn frame_count(&self) -> u64 {
        self.inner.frame_count()
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

    pub fn into_inner(self) -> R {
        self.inner
    }
}

impl<F: FwobFrame> TypedReader<AnyReader, F> {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        Self::new(AnyReader::open(path)?)
    }

    pub fn open_with_v1_key(path: impl AsRef<Path>, key_field_index: usize) -> Result<Self> {
        Self::new(AnyReader::open_with_v1_key(path, key_field_index)?)
    }
}

pub struct TypedAppender<W, F> {
    inner: W,
    buffer: Vec<u8>,
    frame: PhantomData<F>,
}

impl<W, F> TypedAppender<W, F>
where
    W: FwobAppender,
    F: FwobFrame,
{
    pub fn new(inner: W) -> Result<Self> {
        ensure_schema::<F>(&inner)?;
        Ok(Self {
            inner,
            buffer: Vec::with_capacity(F::schema().frame_len as usize),
            frame: PhantomData,
        })
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

    pub fn finish(self) -> Result<()> {
        Box::new(self.inner).finish()
    }
}

impl<F: FwobFrame> TypedAppender<AnyAppender, F> {
    pub fn open(path: impl AsRef<Path>, options: AppendOptions) -> Result<Self> {
        Self::new(AnyAppender::open(path, options)?)
    }

    pub fn create_v1(
        path: impl AsRef<Path>,
        options: fwob_v1::WriterOptions,
        strings: &[String],
    ) -> Result<Self> {
        Self::new(AnyAppender::create_v1(path, F::schema(), options, strings)?)
    }

    pub fn create_v2(path: impl AsRef<Path>, options: fwob_v2::WriterOptions) -> Result<Self> {
        Self::new(AnyAppender::create_v2(path, F::schema(), options)?)
    }
}

pub struct TypedEditor<F> {
    inner: AnyEditor,
    frame: PhantomData<F>,
}

impl<F: FwobFrame> TypedEditor<F> {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let inner = AnyEditor::open(path)?;
        ensure_schema::<F>(&inner)?;
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

    pub fn delete_key(&mut self, key: F::Key) -> Result<u64> {
        self.inner.delete_key(key.into_key())
    }

    pub fn delete_key_range(&mut self, range: RangeInclusive<F::Key>) -> Result<u64> {
        self.inner
            .delete_key_range(range.start().into_key()..=range.end().into_key())
    }

    pub fn delete_all_frames(&mut self) -> Result<u64> {
        self.inner.delete_all_frames()
    }
}

fn ensure_schema<F: FwobFrame>(file: &impl FwobFile) -> Result<()> {
    if file.schema() == &F::schema() {
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

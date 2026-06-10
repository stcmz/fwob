use std::{
    fs::File,
    io::{Read, Seek, SeekFrom},
    path::Path,
};

use fwob_core::{FrameRef, Key, KeyType, OwnedFrame, Schema};

use crate::{
    header::{read_header, Header},
    Result, V1Error,
};

pub struct Reader<R> {
    inner: R,
    header: Header,
    schema: Schema,
    key_type: KeyType,
}

impl Reader<File> {
    pub fn open(path: impl AsRef<Path>, key_field_index: usize) -> Result<Self> {
        let file = File::open(path.as_ref())?;
        let len = file.metadata()?.len();
        let reader = Self::new(file, key_field_index)?;
        if reader.header.file_length() != len {
            return Err(V1Error::CorruptedFileLength {
                expected: reader.header.file_length(),
                actual: len,
            });
        }
        Ok(reader)
    }
}

impl<R: Read + Seek> Reader<R> {
    pub fn new(mut inner: R, key_field_index: usize) -> Result<Self> {
        inner.seek(SeekFrom::Start(0))?;
        let header = read_header(&mut inner)?;
        let schema = header.schema(key_field_index)?;
        let key_type = KeyType::from_field(schema.key_field())?;
        Ok(Self {
            inner,
            header,
            schema,
            key_type,
        })
    }

    pub fn header(&self) -> &Header {
        &self.header
    }

    pub fn schema(&self) -> &Schema {
        &self.schema
    }

    pub fn key_type(&self) -> KeyType {
        self.key_type
    }

    pub fn frame_count(&self) -> u64 {
        self.header.frame_count
    }

    pub fn read_string_table(&mut self) -> Result<Vec<String>> {
        self.inner
            .seek(SeekFrom::Start(self.header.string_table_position()))?;
        let mut strings = Vec::with_capacity(self.header.string_count as usize);
        for _ in 0..self.header.string_count {
            strings.push(read_dotnet_string(&mut self.inner)?);
        }
        let pos = self.inner.stream_position()?;
        if pos != self.header.string_table_ending() {
            return Err(V1Error::CorruptedStringTableLength {
                expected: self.header.string_table_length,
                actual: pos - self.header.string_table_position(),
            });
        }
        Ok(strings)
    }

    pub fn read_frame_at(&mut self, index: u64) -> Result<Option<OwnedFrame>> {
        if index >= self.header.frame_count {
            return Ok(None);
        }
        self.inner.seek(SeekFrom::Start(self.frame_offset(index)))?;
        let mut bytes = vec![0u8; self.header.frame_length as usize];
        self.inner.read_exact(&mut bytes)?;
        Ok(Some(OwnedFrame::new(&self.schema, bytes)?))
    }

    pub fn read_key_at(&mut self, index: u64) -> Result<Option<Key>> {
        if index >= self.header.frame_count {
            return Ok(None);
        }
        let key_field = self.schema.key_field();
        self.inner.seek(SeekFrom::Start(
            self.frame_offset(index) + u64::from(key_field.offset),
        ))?;
        let mut bytes = vec![0u8; key_field.length as usize];
        self.inner.read_exact(&mut bytes)?;
        Ok(Some(Key::decode(self.key_type, &bytes)?))
    }

    pub fn lower_bound(&mut self, key: Key) -> Result<u64> {
        let mut lo = 0;
        let mut hi = self.header.frame_count;
        while lo < hi {
            let mid = lo + ((hi - lo) >> 1);
            let mid_key = self.read_key_at(mid)?.expect("mid is in range");
            if mid_key < key {
                lo = mid + 1;
            } else {
                hi = mid;
            }
        }
        Ok(lo)
    }

    pub fn upper_bound(&mut self, key: Key) -> Result<u64> {
        let mut lo = 0;
        let mut hi = self.header.frame_count;
        while lo < hi {
            let mid = lo + ((hi - lo) >> 1);
            let mid_key = self.read_key_at(mid)?.expect("mid is in range");
            if mid_key <= key {
                lo = mid + 1;
            } else {
                hi = mid;
            }
        }
        Ok(lo)
    }

    pub fn equal_range(&mut self, key: Key) -> Result<(u64, u64)> {
        let mut lo = 0;
        let mut hi = self.header.frame_count;
        let mut upper_hi = hi;
        while lo < hi {
            let mid = lo + ((hi - lo) >> 1);
            let mid_key = self.read_key_at(mid)?.expect("mid is in range");
            if mid_key < key {
                lo = mid + 1;
            } else if mid_key > key {
                hi = mid;
                upper_hi = mid;
            } else {
                hi = mid;
            }
        }

        let lower = lo;
        hi = upper_hi;
        while lo < hi {
            let mid = lo + ((hi - lo) >> 1);
            let mid_key = self.read_key_at(mid)?.expect("mid is in range");
            if mid_key <= key {
                lo = mid + 1;
            } else {
                hi = mid;
            }
        }
        Ok((lower, hi))
    }

    pub fn frames_between(&mut self, first: Key, last: Key) -> Result<Vec<OwnedFrame>> {
        if first > last {
            return Ok(Vec::new());
        }
        let lb = self.lower_bound(first)?;
        let ub = self.upper_bound(last)?;
        self.read_frame_range(lb, ub)
    }

    pub fn read_all_frames(&mut self) -> Result<Vec<OwnedFrame>> {
        self.read_frame_range(0, self.header.frame_count)
    }

    pub fn read_raw_frames_chunk(&mut self, start: u64, max_frames: usize) -> Result<Vec<u8>> {
        if start >= self.header.frame_count || max_frames == 0 {
            return Ok(Vec::new());
        }
        let count = max_frames.min((self.header.frame_count - start) as usize);
        let mut bytes = vec![0u8; count * self.header.frame_length as usize];
        self.inner.seek(SeekFrom::Start(self.frame_offset(start)))?;
        self.inner.read_exact(&mut bytes)?;
        Ok(bytes)
    }

    pub fn verify_key_order(&mut self) -> Result<()> {
        if self.header.frame_count <= 1 {
            return Ok(());
        }
        let mut last = self.read_key_at(0)?.expect("frame exists");
        for index in 1..self.header.frame_count {
            let key = self.read_key_at(index)?.expect("frame exists");
            if key < last {
                return Err(V1Error::KeyOrderViolation { index });
            }
            last = key;
        }
        Ok(())
    }

    pub fn frame_key(&self, frame: FrameRef<'_>) -> Result<Key> {
        Ok(frame.key(&self.schema, self.key_type)?)
    }

    fn read_frame_range(&mut self, begin: u64, end: u64) -> Result<Vec<OwnedFrame>> {
        let mut frames = Vec::with_capacity((end - begin) as usize);
        self.inner.seek(SeekFrom::Start(self.frame_offset(begin)))?;
        for _ in begin..end {
            let mut bytes = vec![0u8; self.header.frame_length as usize];
            self.inner.read_exact(&mut bytes)?;
            frames.push(OwnedFrame::new(&self.schema, bytes)?);
        }
        Ok(frames)
    }

    fn frame_offset(&self, index: u64) -> u64 {
        self.header.first_frame_position() + u64::from(self.header.frame_length) * index
    }
}

pub(crate) fn read_dotnet_string<R: Read>(reader: &mut R) -> Result<String> {
    let len = read_7bit_encoded_int(reader)?;
    let mut bytes = vec![0u8; len as usize];
    reader.read_exact(&mut bytes)?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

pub(crate) fn read_7bit_encoded_int<R: Read>(reader: &mut R) -> Result<u32> {
    let mut count = 0u32;
    let mut shift = 0;
    loop {
        let mut b = [0u8; 1];
        reader.read_exact(&mut b)?;
        count |= u32::from(b[0] & 0x7f) << shift;
        if b[0] & 0x80 == 0 {
            return Ok(count);
        }
        shift += 7;
        if shift >= 35 {
            return Err(V1Error::CorruptedHeader);
        }
    }
}

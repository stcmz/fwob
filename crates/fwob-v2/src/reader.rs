use std::{
    fs::File,
    io::{Read, Seek, SeekFrom},
    path::Path,
};

use fwob_core::{Key, KeyType, OwnedFrame};

use crate::{
    encoding::decode_page_payload,
    file_header::{read_file_header, FileHeader},
    page::PageHeader,
    Result, V2Error,
};

pub struct Reader<R> {
    inner: R,
    header: FileHeader,
    key_type: KeyType,
    cached_page: Option<CachedPage>,
}

struct CachedPage {
    index: u64,
    header: PageHeader,
    raw: Vec<u8>,
}

impl Reader<File> {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let file = File::open(path)?;
        Self::new(file)
    }
}

impl<R: Read + Seek> Reader<R> {
    pub fn new(mut inner: R) -> Result<Self> {
        let header = read_file_header(&mut inner)?;
        let key_type = KeyType::from_field(header.schema.key_field())?;
        Ok(Self {
            inner,
            header,
            key_type,
            cached_page: None,
        })
    }

    pub fn header(&self) -> &FileHeader {
        &self.header
    }

    pub fn read_page_header(&mut self, page_index: u64) -> Result<PageHeader> {
        if page_index >= self.header.page_count {
            return Err(V2Error::InvalidPageHeader(page_index));
        }
        self.inner
            .seek(SeekFrom::Start(self.header.page_offset(page_index)))?;
        PageHeader::read(&mut self.inner, page_index)
    }

    pub fn read_page_frames(&mut self, page_index: u64) -> Result<Vec<OwnedFrame>> {
        self.load_page(page_index)?;
        let raw = &self.cached_page.as_ref().expect("page loaded").raw;
        let frame_len = self.header.schema.frame_len as usize;
        let mut frames = Vec::with_capacity(raw.len() / frame_len);
        for chunk in raw.chunks_exact(frame_len) {
            frames.push(OwnedFrame::new(&self.header.schema, chunk.to_vec())?);
        }
        Ok(frames)
    }

    pub fn read_page_raw_frames(&mut self, page_index: u64) -> Result<Vec<u8>> {
        self.load_page(page_index)?;
        Ok(self.cached_page.as_ref().expect("page loaded").raw.clone())
    }

    pub fn read_frame_at(&mut self, index: u64) -> Result<Option<OwnedFrame>> {
        let Some(page_index) = self.find_page_for_index(index)? else {
            return Ok(None);
        };
        self.load_page(page_index)?;
        let cached = self.cached_page.as_ref().expect("page loaded");
        let local_index = (index - cached.header.first_frame_index) as usize;
        let frame_len = self.header.schema.frame_len as usize;
        let offset = local_index * frame_len;
        Ok(Some(OwnedFrame::new(
            &self.header.schema,
            cached.raw[offset..offset + frame_len].to_vec(),
        )?))
    }

    pub fn read_key_at(&mut self, index: u64) -> Result<Option<Key>> {
        let Some(page_index) = self.find_page_for_index(index)? else {
            return Ok(None);
        };
        self.load_page(page_index)?;
        let cached = self.cached_page.as_ref().expect("page loaded");
        let local_index = index - cached.header.first_frame_index;
        Ok(Some(self.cached_key(local_index as usize)?))
    }

    pub fn first_key(&mut self) -> Result<Option<Key>> {
        if self.header.page_count == 0 {
            Ok(None)
        } else {
            Ok(Some(self.read_page_header(0)?.first_key))
        }
    }

    pub fn last_key(&mut self) -> Result<Option<Key>> {
        if self.header.page_count == 0 {
            Ok(None)
        } else {
            Ok(Some(
                self.read_page_header(self.header.page_count - 1)?.last_key,
            ))
        }
    }

    pub fn find_page_for_index(&mut self, index: u64) -> Result<Option<u64>> {
        if index >= self.header.frame_count || self.header.page_count == 0 {
            return Ok(None);
        }
        let mut lo = 0;
        let mut hi = self.header.page_count;
        while lo < hi {
            let mid = lo + ((hi - lo) >> 1);
            let page = self.read_page_header(mid)?;
            if page.first_frame_index <= index {
                lo = mid + 1;
            } else {
                hi = mid;
            }
        }
        let page_index = lo.saturating_sub(1);
        let page = self.read_page_header(page_index)?;
        if index < page.first_frame_index + u64::from(page.frame_count) {
            Ok(Some(page_index))
        } else {
            Err(V2Error::InvalidPageHeader(page_index))
        }
    }

    pub fn lower_bound(&mut self, key: Key) -> Result<u64> {
        let mut lo = 0;
        let mut hi = self.header.page_count;
        while lo < hi {
            let mid = lo + ((hi - lo) >> 1);
            if self.read_page_header(mid)?.last_key < key {
                lo = mid + 1;
            } else {
                hi = mid;
            }
        }
        if lo == self.header.page_count {
            return Ok(self.header.frame_count);
        }
        self.load_page(lo)?;
        let cached = self.cached_page.as_ref().expect("page loaded");
        let mut frame_lo = 0usize;
        let mut frame_hi = cached.header.frame_count as usize;
        while frame_lo < frame_hi {
            let mid = frame_lo + ((frame_hi - frame_lo) >> 1);
            if self.cached_key(mid)? < key {
                frame_lo = mid + 1;
            } else {
                frame_hi = mid;
            }
        }
        Ok(cached.header.first_frame_index + frame_lo as u64)
    }

    pub fn upper_bound(&mut self, key: Key) -> Result<u64> {
        let mut lo = 0;
        let mut hi = self.header.page_count;
        while lo < hi {
            let mid = lo + ((hi - lo) >> 1);
            if self.read_page_header(mid)?.last_key <= key {
                lo = mid + 1;
            } else {
                hi = mid;
            }
        }
        if lo == self.header.page_count {
            return Ok(self.header.frame_count);
        }
        self.load_page(lo)?;
        let cached = self.cached_page.as_ref().expect("page loaded");
        let mut frame_lo = 0usize;
        let mut frame_hi = cached.header.frame_count as usize;
        while frame_lo < frame_hi {
            let mid = frame_lo + ((frame_hi - frame_lo) >> 1);
            if self.cached_key(mid)? <= key {
                frame_lo = mid + 1;
            } else {
                frame_hi = mid;
            }
        }
        Ok(cached.header.first_frame_index + frame_lo as u64)
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
            if self.read_key_at(mid)?.expect("mid is in range") <= key {
                lo = mid + 1;
            } else {
                hi = mid;
            }
        }
        Ok((lower, hi))
    }

    fn load_page(&mut self, page_index: u64) -> Result<()> {
        if self
            .cached_page
            .as_ref()
            .is_some_and(|cached| cached.index == page_index)
        {
            return Ok(());
        }
        let page_header = self.read_page_header(page_index)?;
        let mut compressed = vec![0u8; page_header.compressed_len as usize];
        self.inner.read_exact(&mut compressed)?;
        page_header.validate_payload(&compressed)?;
        let encoded = page_header
            .codec
            .decompress(&compressed, page_header.uncompressed_len as usize)?;
        let raw = decode_page_payload(
            &self.header.schema,
            page_header.encoding,
            &encoded,
            page_header.frame_count as usize,
        )?;
        self.cached_page = Some(CachedPage {
            index: page_index,
            header: page_header,
            raw,
        });
        Ok(())
    }

    fn cached_key(&self, local_index: usize) -> Result<Key> {
        let cached = self.cached_page.as_ref().expect("page loaded");
        let frame_len = self.header.schema.frame_len as usize;
        let key_field = self.header.schema.key_field();
        let offset = local_index * frame_len + key_field.offset as usize;
        let end = offset + key_field.length as usize;
        Ok(Key::decode(self.key_type, &cached.raw[offset..end])?)
    }

    pub fn find_page_for_key(&mut self, key: Key) -> Result<Option<u64>> {
        if self.header.page_count == 0 {
            return Ok(None);
        }
        let mut lo = 0;
        let mut hi = self.header.page_count;
        while lo < hi {
            let mid = lo + ((hi - lo) >> 1);
            let page = self.read_page_header(mid)?;
            if page.last_key < key {
                lo = mid + 1;
            } else {
                hi = mid;
            }
        }
        if lo >= self.header.page_count {
            return Ok(None);
        }
        let page = self.read_page_header(lo)?;
        if key < page.first_key {
            Ok(None)
        } else {
            Ok(Some(lo))
        }
    }

    pub fn frames_between(&mut self, first: Key, last: Key) -> Result<Vec<OwnedFrame>> {
        if first > last {
            return Ok(Vec::new());
        }
        let start = self.lower_bound(first)?;
        let end = self.upper_bound(last)?;
        let mut out = Vec::with_capacity((end - start) as usize);
        for index in start..end {
            out.push(self.read_frame_at(index)?.expect("index is in range"));
        }
        Ok(out)
    }

    pub fn read_all_frames(&mut self) -> Result<Vec<OwnedFrame>> {
        let mut out = Vec::with_capacity(self.header.frame_count as usize);
        for page_index in 0..self.header.page_count {
            out.extend(self.read_page_frames(page_index)?);
        }
        Ok(out)
    }

    pub fn verify(&mut self) -> Result<()> {
        let mut last_key = None;
        let mut count = 0u64;
        for page_index in 0..self.header.page_count {
            let page = self.read_page_header(page_index)?;
            if page.first_frame_index != count {
                return Err(V2Error::InvalidPageHeader(page_index));
            }
            let frames = self.read_page_frames(page_index)?;
            if frames.len() != page.frame_count as usize {
                return Err(V2Error::InvalidPageHeader(page_index));
            }
            for frame in &frames {
                let key = frame.as_ref().key(&self.header.schema, self.key_type)?;
                if let Some(last) = last_key {
                    if key < last {
                        return Err(V2Error::KeyOrderViolation);
                    }
                }
                last_key = Some(key);
            }
            count += frames.len() as u64;
        }
        if count != self.header.frame_count {
            return Err(V2Error::InvalidFileHeader);
        }
        Ok(())
    }
}

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
        let raw = self.read_page_raw_frames(page_index)?;
        let frame_len = self.header.schema.frame_len as usize;
        let mut frames = Vec::with_capacity(raw.len() / frame_len);
        for chunk in raw.chunks_exact(frame_len) {
            frames.push(OwnedFrame::new(&self.header.schema, chunk.to_vec())?);
        }
        Ok(frames)
    }

    pub fn read_page_raw_frames(&mut self, page_index: u64) -> Result<Vec<u8>> {
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
        Ok(raw)
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
        let mut out = Vec::new();
        let Some(mut page_index) = self.find_page_for_key(first)? else {
            return Ok(out);
        };
        while page_index < self.header.page_count {
            let page = self.read_page_header(page_index)?;
            if page.first_key > last {
                break;
            }
            let frames = self.read_page_frames(page_index)?;
            for frame in frames {
                let key = frame.as_ref().key(&self.header.schema, self.key_type)?;
                if key >= first && key <= last {
                    out.push(frame);
                }
            }
            page_index += 1;
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

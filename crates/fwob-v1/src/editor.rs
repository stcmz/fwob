use std::{fs::File, path::Path};

use fwob_core::{Key, KeyType, OwnedFrame, Schema};

use crate::{Reader, Result, V1Error, Writer, WriterOptions};

/// Mutable v1 file facade implemented by loading frames and rewriting the file on save.
///
/// FWOB v1 stores frames as a flat sorted array. Deletes require compaction in the
/// original C# implementation; this editor performs the same logical operation
/// by rewriting the file. That is acceptable for compatibility and for the rare
/// bulk-deletion workflow that v2 is designed around.
pub struct InMemoryEditor {
    schema: Schema,
    title: String,
    string_table: Vec<String>,
    frames: Vec<OwnedFrame>,
    key_type: KeyType,
}

impl InMemoryEditor {
    pub fn open(path: impl AsRef<Path>, key_field_index: usize) -> Result<Self> {
        let mut reader = Reader::open(path, key_field_index)?;
        reader.verify_key_order()?;
        let schema = reader.schema().clone();
        let key_type = reader.key_type();
        let title = reader.header().title.clone();
        let string_table = reader.read_string_table()?;
        let frames = reader.read_all_frames()?;
        Ok(Self {
            schema,
            title,
            string_table,
            frames,
            key_type,
        })
    }

    pub fn new(schema: Schema, title: impl Into<String>) -> Result<Self> {
        let key_type = KeyType::from_field(schema.key_field())?;
        Ok(Self {
            schema,
            title: title.into(),
            string_table: Vec::new(),
            frames: Vec::new(),
            key_type,
        })
    }

    pub fn schema(&self) -> &Schema {
        &self.schema
    }

    pub fn title(&self) -> &str {
        &self.title
    }

    pub fn frame_count(&self) -> u64 {
        self.frames.len() as u64
    }

    pub fn string_table(&self) -> &[String] {
        &self.string_table
    }

    pub fn frames(&self) -> &[OwnedFrame] {
        &self.frames
    }

    pub fn append_string(&mut self, value: impl Into<String>) -> u32 {
        let index = self.string_table.len() as u32;
        self.string_table.push(value.into());
        index
    }

    pub fn append_frame(&mut self, bytes: &[u8]) -> Result<()> {
        let frame = OwnedFrame::new(&self.schema, bytes.to_vec())?;
        let key = frame.as_ref().key(&self.schema, self.key_type)?;
        if let Some(last) = self.last_key()? {
            if key < last {
                return Err(V1Error::KeyOrderViolation {
                    index: self.frames.len() as u64,
                });
            }
        }
        self.frames.push(frame);
        Ok(())
    }

    pub fn append_frames<I, B>(&mut self, frames: I) -> Result<()>
    where
        I: IntoIterator<Item = B>,
        B: AsRef<[u8]>,
    {
        for frame in frames {
            self.append_frame(frame.as_ref())?;
        }
        Ok(())
    }

    pub fn delete_all_frames(&mut self) -> u64 {
        let removed = self.frames.len() as u64;
        self.frames.clear();
        removed
    }

    pub fn delete_frames_before(&mut self, last_key: Key) -> Result<u64> {
        let end = self.upper_bound(last_key)?;
        self.frames.drain(0..end);
        Ok(end as u64)
    }

    pub fn delete_frames_after(&mut self, first_key: Key) -> Result<u64> {
        let begin = self.lower_bound(first_key)?;
        let removed = self.frames.len() - begin;
        self.frames.truncate(begin);
        Ok(removed as u64)
    }

    pub fn delete_frames_between(&mut self, first_key: Key, last_key: Key) -> Result<u64> {
        if first_key > last_key {
            return Ok(0);
        }
        let begin = self.lower_bound(first_key)?;
        let end = self.upper_bound(last_key)?;
        let removed = end - begin;
        self.frames.drain(begin..end);
        Ok(removed as u64)
    }

    pub fn delete_frames<I>(&mut self, keys: I) -> Result<u64>
    where
        I: IntoIterator<Item = Key>,
    {
        let mut removed = 0u64;
        for key in keys {
            removed += self.delete_frames_between(key, key)?;
        }
        Ok(removed)
    }

    pub fn save_as(&self, path: impl AsRef<Path>) -> Result<()> {
        let mut options = WriterOptions::new(self.title.clone());
        let estimated_string_bytes: usize = self.string_table.iter().map(|s| s.len() + 5).sum();
        options.string_table_preserved_length = estimated_string_bytes.max(1834) as u32;
        let file = File::create(path)?;
        let mut writer = Writer::new(file, self.schema.clone(), options)?;
        for value in &self.string_table {
            writer.append_string(value)?;
        }
        for frame in &self.frames {
            writer.append_frame(frame.bytes())?;
        }
        Ok(())
    }

    fn last_key(&self) -> Result<Option<Key>> {
        Ok(self
            .frames
            .last()
            .map(|frame| frame.as_ref().key(&self.schema, self.key_type))
            .transpose()?)
    }

    fn lower_bound(&self, key: Key) -> Result<usize> {
        let mut lo = 0usize;
        let mut hi = self.frames.len();
        while lo < hi {
            let mid = lo + ((hi - lo) >> 1);
            let mid_key = self.frames[mid].as_ref().key(&self.schema, self.key_type)?;
            if mid_key < key {
                lo = mid + 1;
            } else {
                hi = mid;
            }
        }
        Ok(lo)
    }

    fn upper_bound(&self, key: Key) -> Result<usize> {
        let mut lo = 0usize;
        let mut hi = self.frames.len();
        while lo < hi {
            let mid = lo + ((hi - lo) >> 1);
            let mid_key = self.frames[mid].as_ref().key(&self.schema, self.key_type)?;
            if mid_key <= key {
                lo = mid + 1;
            } else {
                hi = mid;
            }
        }
        Ok(lo)
    }
}

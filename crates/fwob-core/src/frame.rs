use crate::{
    error::Result,
    key::{Key, KeyType},
    schema::Schema,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OwnedFrame {
    bytes: Vec<u8>,
}

impl OwnedFrame {
    pub fn new(schema: &Schema, bytes: Vec<u8>) -> Result<Self> {
        schema.validate_frame_len(bytes.len())?;
        Ok(Self { bytes })
    }

    pub fn as_ref(&self) -> FrameRef<'_> {
        FrameRef { bytes: &self.bytes }
    }

    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameRef<'a> {
    bytes: &'a [u8],
}

impl<'a> FrameRef<'a> {
    pub fn new(schema: &Schema, bytes: &'a [u8]) -> Result<Self> {
        schema.validate_frame_len(bytes.len())?;
        Ok(Self { bytes })
    }

    pub fn bytes(self) -> &'a [u8] {
        self.bytes
    }

    pub fn key(self, schema: &Schema, key_type: KeyType) -> Result<Key> {
        let key_field = schema.key_field();
        let start = key_field.offset as usize;
        let end = start + key_field.length as usize;
        Key::decode(key_type, &self.bytes[start..end])
    }
}

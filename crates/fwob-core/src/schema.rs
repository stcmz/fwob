use crate::error::{FwobError, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum FieldType {
    SignedInteger = 0,
    UnsignedInteger = 1,
    FloatingPoint = 2,
    Utf8String = 3,
    StringTableIndex = 4,
}

impl FieldType {
    pub fn from_v1_id(id: u8) -> Result<Self> {
        match id {
            0 => Ok(Self::SignedInteger),
            1 => Ok(Self::UnsignedInteger),
            2 => Ok(Self::FloatingPoint),
            3 => Ok(Self::Utf8String),
            4 => Ok(Self::StringTableIndex),
            _ => Err(FwobError::UnsupportedFieldType(id)),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Field {
    pub name: String,
    pub field_type: FieldType,
    pub length: u16,
    pub offset: u32,
}

impl Field {
    pub fn new(name: impl Into<String>, field_type: FieldType, length: u16, offset: u32) -> Self {
        Self {
            name: name.into(),
            field_type,
            length,
            offset,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Schema {
    pub frame_type: String,
    pub fields: Vec<Field>,
    pub key_field_index: usize,
    pub frame_len: u32,
}

impl Schema {
    pub fn new(
        frame_type: impl Into<String>,
        fields: Vec<Field>,
        key_field_index: usize,
    ) -> Result<Self> {
        if fields.is_empty() {
            return Err(FwobError::InvalidSchema(
                "schema must contain fields".into(),
            ));
        }
        if key_field_index >= fields.len() {
            return Err(FwobError::InvalidSchema(
                "key field index is out of range".into(),
            ));
        }

        let mut expected_offset = 0u32;
        for field in &fields {
            if field.offset != expected_offset {
                return Err(FwobError::InvalidSchema(format!(
                    "field '{}' has offset {}, expected {}",
                    field.name, field.offset, expected_offset
                )));
            }
            expected_offset += u32::from(field.length);
        }

        Ok(Self {
            frame_type: frame_type.into(),
            fields,
            key_field_index,
            frame_len: expected_offset,
        })
    }

    pub fn key_field(&self) -> &Field {
        &self.fields[self.key_field_index]
    }

    pub fn validate_frame_len(&self, len: usize) -> Result<()> {
        if len == self.frame_len as usize {
            Ok(())
        } else {
            Err(FwobError::InvalidFrameLength {
                expected: self.frame_len as usize,
                actual: len,
            })
        }
    }
}

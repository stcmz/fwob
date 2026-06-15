use crate::error::{FwobError, Result};
use std::collections::HashSet;

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimestampUnit {
    Seconds,
    Milliseconds,
    Microseconds,
    Nanoseconds,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FieldSemantic {
    None,
    UnixTimestamp(TimestampUnit),
}

impl FieldSemantic {
    pub fn id(self) -> u8 {
        match self {
            Self::None => 0,
            Self::UnixTimestamp(TimestampUnit::Seconds) => 1,
            Self::UnixTimestamp(TimestampUnit::Milliseconds) => 2,
            Self::UnixTimestamp(TimestampUnit::Microseconds) => 3,
            Self::UnixTimestamp(TimestampUnit::Nanoseconds) => 4,
        }
    }

    pub fn from_id(id: u8) -> Result<Self> {
        match id {
            0 => Ok(Self::None),
            1 => Ok(Self::UnixTimestamp(TimestampUnit::Seconds)),
            2 => Ok(Self::UnixTimestamp(TimestampUnit::Milliseconds)),
            3 => Ok(Self::UnixTimestamp(TimestampUnit::Microseconds)),
            4 => Ok(Self::UnixTimestamp(TimestampUnit::Nanoseconds)),
            _ => Err(FwobError::InvalidSchema(format!(
                "unsupported field semantic id {id}"
            ))),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Field {
    pub name: String,
    pub field_type: FieldType,
    pub length: u16,
    pub offset: u32,
    pub semantic: FieldSemantic,
}

impl Field {
    pub fn new(name: impl Into<String>, field_type: FieldType, length: u16, offset: u32) -> Self {
        Self {
            name: name.into(),
            field_type,
            length,
            offset,
            semantic: FieldSemantic::None,
        }
    }

    pub fn with_semantic(mut self, semantic: FieldSemantic) -> Self {
        self.semantic = semantic;
        self
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
        let frame_type = frame_type.into();
        if frame_type.is_empty() {
            return Err(FwobError::InvalidSchema(
                "frame type must not be empty".into(),
            ));
        }
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
        let mut field_names = HashSet::with_capacity(fields.len());
        for field in &fields {
            if field.name.is_empty() {
                return Err(FwobError::InvalidSchema(
                    "field name must not be empty".into(),
                ));
            }
            if !field_names.insert(field.name.as_str()) {
                return Err(FwobError::InvalidSchema(format!(
                    "duplicate field name '{}'",
                    field.name
                )));
            }
            validate_field_length(field)?;
            if !matches!(field.semantic, FieldSemantic::None)
                && !matches!(
                    field.field_type,
                    FieldType::SignedInteger | FieldType::UnsignedInteger
                )
            {
                return Err(FwobError::InvalidSchema(format!(
                    "field '{}' uses timestamp semantics but is not an integer",
                    field.name
                )));
            }
            if field.offset != expected_offset {
                return Err(FwobError::InvalidSchema(format!(
                    "field '{}' has offset {}, expected {}",
                    field.name, field.offset, expected_offset
                )));
            }
            expected_offset = expected_offset
                .checked_add(u32::from(field.length))
                .ok_or_else(|| FwobError::InvalidSchema("frame length overflows u32".into()))?;
        }
        crate::KeyType::from_field(&fields[key_field_index])?;

        Ok(Self {
            frame_type,
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

fn validate_field_length(field: &Field) -> Result<()> {
    let valid = match field.field_type {
        FieldType::SignedInteger | FieldType::UnsignedInteger | FieldType::StringTableIndex => {
            matches!(field.length, 1 | 2 | 4 | 8)
        }
        // Decimal fields written by the original C# implementation use 16 bytes.
        FieldType::FloatingPoint => matches!(field.length, 4 | 8 | 16),
        FieldType::Utf8String => field.length > 0,
    };
    if valid {
        Ok(())
    } else {
        Err(FwobError::InvalidSchema(format!(
            "field '{}' has invalid length {} for {:?}",
            field.name, field.length, field.field_type
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn field(name: &str, field_type: FieldType, length: u16, offset: u32) -> Field {
        Field::new(name, field_type, length, offset)
    }

    #[test]
    fn schema_rejects_empty_duplicate_and_non_contiguous_fields() {
        assert!(Schema::new("", vec![field("Key", FieldType::SignedInteger, 4, 0)], 0).is_err());
        assert!(Schema::new(
            "Tick",
            vec![
                field("Key", FieldType::SignedInteger, 4, 0),
                field("Key", FieldType::UnsignedInteger, 4, 4),
            ],
            0,
        )
        .is_err());
        assert!(Schema::new(
            "Tick",
            vec![
                field("Key", FieldType::SignedInteger, 4, 0),
                field("Value", FieldType::UnsignedInteger, 4, 5),
            ],
            0,
        )
        .is_err());
    }

    #[test]
    fn schema_validates_field_widths_and_orderable_key_type() {
        assert!(Schema::new(
            "Tick",
            vec![field("Key", FieldType::SignedInteger, 3, 0)],
            0,
        )
        .is_err());
        assert!(Schema::new(
            "Tick",
            vec![field("Key", FieldType::FloatingPoint, 8, 0)],
            0,
        )
        .is_ok());
        assert!(Schema::new(
            "Tick",
            vec![
                field("Key", FieldType::SignedInteger, 4, 0),
                field("Decimal", FieldType::FloatingPoint, 16, 4),
            ],
            0,
        )
        .is_ok());
    }
}

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

/// Largest supported decimal-point count for the fixed-point and percentage semantics.
pub const MAX_DECIMAL_POINTS: u8 = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FieldSemantic {
    None,
    UnixTimestamp(TimestampUnit),
    /// Render an integer as `value / 10^points` with `points` fractional digits, comma-grouped.
    /// `points == 0` formats the integer with commas and no decimal point.
    FixedPoint(u8),
    /// Like [`FieldSemantic::FixedPoint`] but with a trailing `%`.
    Percentage(u8),
}

impl FieldSemantic {
    // On-disk ids are a flat enumeration. 0..=4 are grandfathered; a slot is reserved after
    // each category (5, 15, 25) so future kinds can extend without renumbering. FixedPoint and
    // Percentage carry 0..=8 decimal points (ids 6..=14 and 16..=24).
    pub fn id(self) -> u8 {
        match self {
            Self::None => 0,
            Self::UnixTimestamp(TimestampUnit::Seconds) => 1,
            Self::UnixTimestamp(TimestampUnit::Milliseconds) => 2,
            Self::UnixTimestamp(TimestampUnit::Microseconds) => 3,
            Self::UnixTimestamp(TimestampUnit::Nanoseconds) => 4,
            // reserved: 5
            Self::FixedPoint(points) => 6 + points,
            // reserved: 15
            Self::Percentage(points) => 16 + points,
        }
    }

    pub fn from_id(id: u8) -> Result<Self> {
        match id {
            0 => Ok(Self::None),
            1 => Ok(Self::UnixTimestamp(TimestampUnit::Seconds)),
            2 => Ok(Self::UnixTimestamp(TimestampUnit::Milliseconds)),
            3 => Ok(Self::UnixTimestamp(TimestampUnit::Microseconds)),
            4 => Ok(Self::UnixTimestamp(TimestampUnit::Nanoseconds)),
            6..=14 => Ok(Self::FixedPoint(id - 6)),
            16..=24 => Ok(Self::Percentage(id - 16)),
            _ => Err(FwobError::InvalidSchema(format!(
                "unsupported field semantic id {id}"
            ))),
        }
    }

    /// The decimal-point count for fixed-point / percentage semantics, if applicable.
    pub fn decimal_points(self) -> Option<u8> {
        match self {
            Self::FixedPoint(points) | Self::Percentage(points) => Some(points),
            _ => None,
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
                    "field '{}' uses a numeric semantic but is not an integer",
                    field.name
                )));
            }
            if let Some(points) = field.semantic.decimal_points() {
                if points > MAX_DECIMAL_POINTS {
                    return Err(FwobError::InvalidSchema(format!(
                        "field '{}' uses {points} decimal points but at most {MAX_DECIMAL_POINTS} are supported",
                        field.name
                    )));
                }
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

    /// Structural compatibility that ignores per-field `semantic`.
    ///
    /// FWOB v1 has no on-disk slot for field semantics, so a v1 schema always reads back with
    /// `FieldSemantic::None`. When an operation bridges v1 and v2 (e.g. appending a v1 file into a
    /// v2 target, or opening a v1 file with a semantic-bearing typed schema), the schemas must be
    /// treated as compatible even though their semantics differ. Callers that compare two v2
    /// schemas should keep using `==` so that semantic differences are still rejected.
    pub fn is_compatible(&self, other: &Schema) -> bool {
        self.frame_type == other.frame_type
            && self.key_field_index == other.key_field_index
            && self.frame_len == other.frame_len
            && self.fields.len() == other.fields.len()
            && self.fields.iter().zip(&other.fields).all(|(a, b)| {
                a.name == b.name
                    && a.field_type == b.field_type
                    && a.length == b.length
                    && a.offset == b.offset
            })
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
    fn semantic_ids_round_trip_through_fixed8_and_percent8() {
        // Grandfathered + extended ids round-trip; reserved slots (5, 15, 25) reject.
        for points in 0..=MAX_DECIMAL_POINTS {
            let fixed = FieldSemantic::FixedPoint(points);
            assert_eq!(FieldSemantic::from_id(fixed.id()).unwrap(), fixed);
            assert_eq!(fixed.id(), 6 + points);
            let percent = FieldSemantic::Percentage(points);
            assert_eq!(FieldSemantic::from_id(percent.id()).unwrap(), percent);
            assert_eq!(percent.id(), 16 + points);
        }
        assert_eq!(FieldSemantic::FixedPoint(8).id(), 14);
        assert_eq!(FieldSemantic::Percentage(8).id(), 24);
        for reserved in [5u8, 15, 25] {
            assert!(FieldSemantic::from_id(reserved).is_err());
        }
        assert!(FieldSemantic::from_id(255).is_err());
    }

    #[test]
    fn schema_accepts_fixed8_but_rejects_nine_decimals() {
        let ok = Schema::new(
            "Row",
            vec![field("v", FieldType::SignedInteger, 4, 0)
                .with_semantic(FieldSemantic::FixedPoint(8))],
            0,
        );
        assert!(ok.is_ok());
        let too_many = Schema::new(
            "Row",
            vec![field("v", FieldType::SignedInteger, 4, 0)
                .with_semantic(FieldSemantic::FixedPoint(9))],
            0,
        );
        assert!(too_many.is_err());
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
    fn is_compatible_ignores_semantics_but_not_structure() {
        let v1 = Schema::new(
            "Tick",
            vec![
                field("Time", FieldType::UnsignedInteger, 4, 0),
                field("Price", FieldType::UnsignedInteger, 4, 4),
            ],
            0,
        )
        .unwrap();
        let v2 = Schema::new(
            "Tick",
            vec![
                field("Time", FieldType::UnsignedInteger, 4, 0)
                    .with_semantic(FieldSemantic::UnixTimestamp(TimestampUnit::Seconds)),
                field("Price", FieldType::UnsignedInteger, 4, 4),
            ],
            0,
        )
        .unwrap();

        // Semantics differ, so `==` rejects but `is_compatible` accepts.
        assert_ne!(v1, v2);
        assert!(v1.is_compatible(&v2));
        assert!(v2.is_compatible(&v1));

        // Structural differences are still incompatible.
        let renamed = Schema::new(
            "Tick",
            vec![
                field("Stamp", FieldType::UnsignedInteger, 4, 0),
                field("Price", FieldType::UnsignedInteger, 4, 4),
            ],
            0,
        )
        .unwrap();
        let different_key = Schema::new(
            "Tick",
            vec![
                field("Time", FieldType::UnsignedInteger, 4, 0),
                field("Price", FieldType::UnsignedInteger, 4, 4),
            ],
            1,
        )
        .unwrap();
        assert!(!v1.is_compatible(&renamed));
        assert!(!v1.is_compatible(&different_key));
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

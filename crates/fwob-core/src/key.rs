use std::cmp::Ordering;
use std::fmt;

use crate::{
    error::{FwobError, Result},
    schema::{Field, FieldType},
    typed::{decode_decimal, encode_decimal},
    Decimal,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyType {
    I8,
    I16,
    I32,
    I64,
    U8,
    U16,
    U32,
    U64,
    F32,
    F64,
    Decimal,
}

impl KeyType {
    pub fn from_field(field: &Field) -> Result<Self> {
        match (field.field_type, field.length) {
            (FieldType::SignedInteger, 1) => Ok(Self::I8),
            (FieldType::SignedInteger, 2) => Ok(Self::I16),
            (FieldType::SignedInteger, 4) => Ok(Self::I32),
            (FieldType::SignedInteger, 8) => Ok(Self::I64),
            (FieldType::UnsignedInteger | FieldType::StringTableIndex, 1) => Ok(Self::U8),
            (FieldType::UnsignedInteger | FieldType::StringTableIndex, 2) => Ok(Self::U16),
            (FieldType::UnsignedInteger | FieldType::StringTableIndex, 4) => Ok(Self::U32),
            (FieldType::UnsignedInteger | FieldType::StringTableIndex, 8) => Ok(Self::U64),
            (FieldType::FloatingPoint, 4) => Ok(Self::F32),
            (FieldType::FloatingPoint, 8) => Ok(Self::F64),
            (FieldType::FloatingPoint, 16) => Ok(Self::Decimal),
            _ => Err(FwobError::UnsupportedKeyType {
                field_type: field.field_type,
                length: field.length,
            }),
        }
    }

    pub fn byte_len(self) -> usize {
        match self {
            Self::I8 | Self::U8 => 1,
            Self::I16 | Self::U16 => 2,
            Self::I32 | Self::U32 => 4,
            Self::I64 | Self::U64 | Self::F64 => 8,
            Self::F32 => 4,
            Self::Decimal => 16,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub enum Key {
    I8(i8),
    I16(i16),
    I32(i32),
    I64(i64),
    U8(u8),
    U16(u16),
    U32(u32),
    U64(u64),
    F32(f32),
    F64(f64),
    Decimal(Decimal),
}

impl Key {
    pub fn parse(key_type: KeyType, value: &str) -> Result<Self> {
        macro_rules! parsed {
            ($ty:ty, $variant:ident) => {
                value
                    .parse::<$ty>()
                    .map(Self::$variant)
                    .map_err(|_| FwobError::InvalidKeyValue {
                        key_type,
                        value: value.to_owned(),
                    })
            };
        }
        match key_type {
            KeyType::I8 => parsed!(i8, I8),
            KeyType::I16 => parsed!(i16, I16),
            KeyType::I32 => parsed!(i32, I32),
            KeyType::I64 => parsed!(i64, I64),
            KeyType::U8 => parsed!(u8, U8),
            KeyType::U16 => parsed!(u16, U16),
            KeyType::U32 => parsed!(u32, U32),
            KeyType::U64 => parsed!(u64, U64),
            KeyType::F32 => parsed!(f32, F32),
            KeyType::F64 => parsed!(f64, F64),
            KeyType::Decimal => parsed!(Decimal, Decimal),
        }
    }

    pub fn decode(key_type: KeyType, bytes: &[u8]) -> Result<Self> {
        if bytes.len() != key_type.byte_len() {
            return Err(FwobError::InvalidKeyLength {
                expected: key_type.byte_len(),
                actual: bytes.len(),
            });
        }

        Ok(match key_type {
            KeyType::I8 => Self::I8(bytes[0] as i8),
            KeyType::I16 => Self::I16(i16::from_le_bytes(bytes.try_into().unwrap())),
            KeyType::I32 => Self::I32(i32::from_le_bytes(bytes.try_into().unwrap())),
            KeyType::I64 => Self::I64(i64::from_le_bytes(bytes.try_into().unwrap())),
            KeyType::U8 => Self::U8(bytes[0]),
            KeyType::U16 => Self::U16(u16::from_le_bytes(bytes.try_into().unwrap())),
            KeyType::U32 => Self::U32(u32::from_le_bytes(bytes.try_into().unwrap())),
            KeyType::U64 => Self::U64(u64::from_le_bytes(bytes.try_into().unwrap())),
            KeyType::F32 => Self::F32(f32::from_le_bytes(bytes.try_into().unwrap())),
            KeyType::F64 => Self::F64(f64::from_le_bytes(bytes.try_into().unwrap())),
            KeyType::Decimal => Self::Decimal(decode_decimal(bytes)?),
        })
    }

    pub fn encode(self, out: &mut Vec<u8>) {
        match self {
            Self::I8(v) => out.push(v as u8),
            Self::I16(v) => out.extend_from_slice(&v.to_le_bytes()),
            Self::I32(v) => out.extend_from_slice(&v.to_le_bytes()),
            Self::I64(v) => out.extend_from_slice(&v.to_le_bytes()),
            Self::U8(v) => out.push(v),
            Self::U16(v) => out.extend_from_slice(&v.to_le_bytes()),
            Self::U32(v) => out.extend_from_slice(&v.to_le_bytes()),
            Self::U64(v) => out.extend_from_slice(&v.to_le_bytes()),
            Self::F32(v) => out.extend_from_slice(&v.to_le_bytes()),
            Self::F64(v) => out.extend_from_slice(&v.to_le_bytes()),
            Self::Decimal(v) => encode_decimal(v, out),
        }
    }
}

impl PartialEq for Key {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}

impl Eq for Key {}

impl PartialOrd for Key {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Key {
    fn cmp(&self, other: &Self) -> Ordering {
        match (*self, *other) {
            (Self::I8(a), Self::I8(b)) => a.cmp(&b),
            (Self::I16(a), Self::I16(b)) => a.cmp(&b),
            (Self::I32(a), Self::I32(b)) => a.cmp(&b),
            (Self::I64(a), Self::I64(b)) => a.cmp(&b),
            (Self::U8(a), Self::U8(b)) => a.cmp(&b),
            (Self::U16(a), Self::U16(b)) => a.cmp(&b),
            (Self::U32(a), Self::U32(b)) => a.cmp(&b),
            (Self::U64(a), Self::U64(b)) => a.cmp(&b),
            (Self::F32(a), Self::F32(b)) => a.total_cmp(&b),
            (Self::F64(a), Self::F64(b)) => a.total_cmp(&b),
            (Self::Decimal(a), Self::Decimal(b)) => a.cmp(&b),
            _ => self.variant_rank().cmp(&other.variant_rank()),
        }
    }
}

impl fmt::Display for Key {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::I8(v) => write!(f, "{v}"),
            Self::I16(v) => write!(f, "{v}"),
            Self::I32(v) => write!(f, "{v}"),
            Self::I64(v) => write!(f, "{v}"),
            Self::U8(v) => write!(f, "{v}"),
            Self::U16(v) => write!(f, "{v}"),
            Self::U32(v) => write!(f, "{v}"),
            Self::U64(v) => write!(f, "{v}"),
            Self::F32(v) => write!(f, "{v}"),
            Self::F64(v) => write!(f, "{v}"),
            Self::Decimal(v) => write!(f, "{v}"),
        }
    }
}

impl Key {
    fn variant_rank(self) -> u8 {
        match self {
            Self::I8(_) => 0,
            Self::I16(_) => 1,
            Self::I32(_) => 2,
            Self::I64(_) => 3,
            Self::U8(_) => 4,
            Self::U16(_) => 5,
            Self::U32(_) => 6,
            Self::U64(_) => 7,
            Self::F32(_) => 8,
            Self::F64(_) => 9,
            Self::Decimal(_) => 10,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn floating_keys_use_total_order_and_roundtrip() {
        let values = [
            Key::F32(f32::NEG_INFINITY),
            Key::F32(-0.0),
            Key::F32(0.0),
            Key::F32(f32::INFINITY),
            Key::F32(f32::NAN),
        ];
        assert!(values.windows(2).all(|pair| pair[0] < pair[1]));
        for value in values {
            let mut bytes = Vec::new();
            value.encode(&mut bytes);
            assert_eq!(Key::decode(KeyType::F32, &bytes).unwrap(), value);
        }
    }

    #[test]
    fn decimal_keys_roundtrip() {
        let value = Key::Decimal(Decimal::new(-12_345, 2));
        let mut bytes = Vec::new();
        value.encode(&mut bytes);
        assert_eq!(Key::decode(KeyType::Decimal, &bytes).unwrap(), value);
        assert_eq!(Key::parse(KeyType::Decimal, "-123.45").unwrap(), value);
    }
}

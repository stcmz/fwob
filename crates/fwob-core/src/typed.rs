use crate::{FwobError, Key, Result, Schema};
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct StringIndex(pub u32);

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct FixedString<const N: usize> {
    bytes: [u8; N],
}

impl<const N: usize> FixedString<N> {
    pub fn new(value: &str) -> Result<Self> {
        let value = value.as_bytes();
        if value.len() > N {
            return Err(FwobError::FixedStringTooLong {
                capacity: N,
                actual: value.len(),
            });
        }
        let mut bytes = [b' '; N];
        bytes[..value.len()].copy_from_slice(value);
        Ok(Self { bytes })
    }

    pub fn from_padded_bytes(bytes: [u8; N]) -> Result<Self> {
        std::str::from_utf8(&bytes)?;
        Ok(Self { bytes })
    }

    pub fn as_str(&self) -> &str {
        let end = self
            .bytes
            .iter()
            .rposition(|byte| *byte != b' ')
            .map_or(0, |index| index + 1);
        std::str::from_utf8(&self.bytes[..end]).expect("FixedString validates UTF-8")
    }

    pub fn padded_bytes(&self) -> &[u8; N] {
        &self.bytes
    }
}

impl<const N: usize> Default for FixedString<N> {
    fn default() -> Self {
        Self { bytes: [b' '; N] }
    }
}

impl<const N: usize> TryFrom<&str> for FixedString<N> {
    type Error = FwobError;

    fn try_from(value: &str) -> Result<Self> {
        Self::new(value)
    }
}

impl<const N: usize> AsRef<str> for FixedString<N> {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl<const N: usize> fmt::Debug for FixedString<N> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.as_str().fmt(formatter)
    }
}

impl<const N: usize> fmt::Display for FixedString<N> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

pub trait FwobKey: Copy + Ord {
    fn into_key(self) -> Key;
    fn from_key(key: Key) -> Result<Self>;
}

macro_rules! impl_key {
    ($ty:ty, $variant:ident) => {
        impl FwobKey for $ty {
            fn into_key(self) -> Key {
                Key::$variant(self)
            }

            fn from_key(key: Key) -> Result<Self> {
                match key {
                    Key::$variant(value) => Ok(value),
                    _ => Err(FwobError::InvalidSchema(
                        "typed key does not match schema".into(),
                    )),
                }
            }
        }
    };
}

impl_key!(i8, I8);
impl_key!(i16, I16);
impl_key!(i32, I32);
impl_key!(i64, I64);
impl_key!(u8, U8);
impl_key!(u16, U16);
impl_key!(u32, U32);
impl_key!(u64, U64);

pub trait FwobFrame: Sized {
    type Key: FwobKey;

    fn schema() -> Schema;
    fn key(&self) -> Self::Key;
    fn encode(&self, output: &mut Vec<u8>);
    fn decode(bytes: &[u8]) -> Result<Self>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixed_string_pads_trims_and_supports_utf8() {
        let ascii = FixedString::<5>::new("abc").unwrap();
        assert_eq!(ascii.padded_bytes(), b"abc  ");
        assert_eq!(ascii.as_str(), "abc");

        let utf8 = FixedString::<6>::new("你好").unwrap();
        assert_eq!(utf8.as_str(), "你好");
        assert!(FixedString::<5>::new("你好").is_err());
        assert!(FixedString::<2>::from_padded_bytes([0xff, 0xff]).is_err());
    }
}

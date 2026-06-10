use crate::{FwobError, Key, Result, Schema};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct StringIndex(pub u32);

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

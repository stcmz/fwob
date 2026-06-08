use crate::{Result, V2Error};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Codec {
    None = 0,
    Zstd = 1,
    Lz4 = 2,
}

impl Codec {
    pub fn from_id(id: u8) -> Result<Self> {
        match id {
            0 => Ok(Self::None),
            1 => Ok(Self::Zstd),
            2 => Ok(Self::Lz4),
            other => Err(V2Error::UnsupportedCodec(other)),
        }
    }

    pub fn compress(self, input: &[u8]) -> Result<Vec<u8>> {
        self.compress_with_zstd_level(input, 3)
    }

    pub fn compress_with_zstd_level(self, input: &[u8], zstd_level: i32) -> Result<Vec<u8>> {
        Ok(match self {
            Self::None => input.to_vec(),
            Self::Zstd => zstd::bulk::compress(input, zstd_level)?,
            Self::Lz4 => lz4_flex::compress_prepend_size(input),
        })
    }

    pub fn decompress(self, input: &[u8], expected_len: usize) -> Result<Vec<u8>> {
        let out = match self {
            Self::None => input.to_vec(),
            Self::Zstd => zstd::bulk::decompress(input, expected_len)?,
            Self::Lz4 => {
                lz4_flex::decompress_size_prepended(input).map_err(|_| V2Error::ChecksumMismatch)?
            }
        };
        if out.len() != expected_len {
            return Err(V2Error::ChecksumMismatch);
        }
        Ok(out)
    }
}

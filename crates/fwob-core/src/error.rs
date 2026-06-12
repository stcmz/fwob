use thiserror::Error;

pub type Result<T> = std::result::Result<T, FwobError>;

#[derive(Debug, Error)]
pub enum FwobError {
    #[error("unsupported field type id {0}")]
    UnsupportedFieldType(u8),

    #[error("unsupported key type for field length {length} and type {field_type:?}")]
    UnsupportedKeyType {
        field_type: crate::schema::FieldType,
        length: u16,
    },

    #[error("invalid schema: {0}")]
    InvalidSchema(String),

    #[error("invalid key bytes: expected {expected} bytes, got {actual}")]
    InvalidKeyLength { expected: usize, actual: usize },

    #[error("frame has invalid length: expected {expected} bytes, got {actual}")]
    InvalidFrameLength { expected: usize, actual: usize },

    #[error("invalid frame range {start}..{end} for {frame_count} frames")]
    InvalidFrameRange {
        start: u64,
        end: u64,
        frame_count: u64,
    },

    #[error("keys must be sorted in nondecreasing order")]
    UnsortedKeys,

    #[error("format backend error: {0}")]
    Backend(#[source] Box<dyn std::error::Error + Send + Sync>),

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}

impl FwobError {
    pub fn backend(error: impl std::error::Error + Send + Sync + 'static) -> Self {
        Self::Backend(Box::new(error))
    }
}

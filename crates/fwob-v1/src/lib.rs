mod core_api;
mod editor;
mod header;
mod reader;
mod verifier;
mod writer;

pub use core_api::{
    create_writer as create_core_writer, open_reader as open_core_reader,
    open_writer as open_core_writer, MaintenanceService,
};
pub use editor::InMemoryEditor;
pub use header::{Header, HEADER_LEN, SIGNATURE, VERSION};
pub use reader::Reader;
pub use verifier::{repair_committed_tail, verify_file, VerificationReport};
pub use writer::{Writer, WriterOptions};

use thiserror::Error;

pub type Result<T> = std::result::Result<T, V1Error>;

#[derive(Debug, Error)]
pub enum V1Error {
    #[error("corrupted FWOB v1 header")]
    CorruptedHeader,

    #[error("frame type mismatch: expected {expected}, found {actual}")]
    FrameTypeMismatch { expected: String, actual: String },

    #[error("corrupted file length: expected {expected}, actual {actual}")]
    CorruptedFileLength { expected: u64, actual: u64 },

    #[error("corrupted string table length: expected {expected}, actual {actual}")]
    CorruptedStringTableLength { expected: u32, actual: u64 },

    #[error("key order violation at frame {index}")]
    KeyOrderViolation { index: u64 },

    #[error("key field index {0} is out of range")]
    KeyFieldIndexOutOfRange(usize),

    #[error("string table out of space: required {required}, preserved {preserved}")]
    StringTableOutOfSpace { required: u32, preserved: u32 },

    #[error("FWOB core error: {0}")]
    Core(#[from] fwob_core::FwobError),

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}

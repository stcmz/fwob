mod codec;
mod core_api;
mod encoding;
mod file_header;
mod page;
mod reader;
mod repair;
mod writer;

pub use codec::Codec;
pub use core_api::{
    create_writer as create_core_writer, open_reader as open_core_reader,
    open_writer as open_core_writer, MaintenanceService,
};
pub use encoding::{decode_page_payload, encode_page_payload};
pub use file_header::{
    update_counts, update_metadata, FileHeader, FILE_HEADER_LEN, MAGIC, MAX_PAGE_SIZE,
    MIN_PAGE_SIZE, VERSION,
};
pub use page::{Encoding, PageHeader, PAGE_HEADER_LEN};
pub use reader::Reader;
pub use repair::repair_committed_tail;
pub use writer::{
    CodecSelection, EncodingSelection, PackingStats, PagePacking, Writer, WriterOptions,
    DEFAULT_CODEC, DEFAULT_ENCODING, DEFAULT_PAGE_PACKING, DEFAULT_PAGE_SIZE, DEFAULT_ZSTD_LEVEL,
};

use thiserror::Error;

pub type Result<T> = std::result::Result<T, V2Error>;

#[derive(Debug, Error)]
pub enum V2Error {
    #[error("invalid FWOB v2 file header")]
    InvalidFileHeader,

    #[error("invalid FWOB v2 page header at page {0}")]
    InvalidPageHeader(u64),

    #[error("unsupported codec {0}")]
    UnsupportedCodec(u8),

    #[error("unsupported encoding {0}")]
    UnsupportedEncoding(u8),

    #[error("page payload exceeds page capacity: compressed {compressed}, capacity {capacity}")]
    PageOverflow { compressed: usize, capacity: usize },

    #[error("frame does not fit into an empty page")]
    FrameTooLarge,

    #[error("key order violation")]
    KeyOrderViolation,

    #[error("checksum mismatch")]
    ChecksumMismatch,

    #[error("FWOB core error: {0}")]
    Core(#[from] fwob_core::FwobError),

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}

pub mod error;
pub mod frame;
pub mod io;
pub mod key;
pub mod schema;
pub mod typed;

pub use error::{FwobError, Result};
pub use frame::{FrameRef, OwnedFrame};
pub use fwob_derive::FwobFrame;
pub use io::{
    Editor, FileInfo, FormatVersion, FrameIter, Maintenance, MultiRangeFrameIter, Organizer,
    Reader, ReaderBackend, ReaderOptions, VerificationReport, Writer, WriterBackend, WriterFactory,
};
pub use key::{Key, KeyType};
pub use rust_decimal::Decimal;
pub use schema::{Field, FieldSemantic, FieldType, Schema, TimestampUnit};
pub use typed::{
    decode_decimal, encode_decimal, FixedString, FwobFrame, FwobKey, StringIndex, StringIndex16,
    StringIndex64, StringIndex8,
};

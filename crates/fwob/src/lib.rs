use std::{
    fs::File,
    io::Read,
    path::{Path, PathBuf},
};

mod editor;
mod maintenance;
mod organization;
mod reader;
mod typed;
mod writer;

pub use editor::{DeletionPacking, Editor, MutationOptions};
pub use maintenance::Maintenance;
pub use organization::Organizer;
pub use reader::Reader;
pub use typed::{TypedEditor, TypedReader, TypedWriter};
pub use writer::{Writer, WriterOpenOptions};

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("unsupported FWOB file format: {0}")]
    UnsupportedFormat(PathBuf),
    #[error("at least one source file is required")]
    EmptySources,
    #[error("at least one split key is required")]
    EmptySplitKeys,
    #[error("split keys must be sorted")]
    UnsortedSplitKeys,
    #[error("source files use different FWOB format versions")]
    IncompatibleFormat,
    #[error("source files use incompatible schemas")]
    IncompatibleSchema,
    #[error("source files use incompatible titles")]
    IncompatibleTitle,
    #[error("source files use incompatible string tables")]
    IncompatibleStringTable,
    #[error("source frame keys are not globally ordered")]
    IncompatibleKeyOrder,
    #[error("typed frame schema does not match the file schema")]
    SchemaMismatch,
    #[error(transparent)]
    Core(#[from] fwob_core::FwobError),
    #[error(transparent)]
    V1(#[from] fwob_v1::V1Error),
    #[error(transparent)]
    V2(#[from] fwob_v2::V2Error),
}

pub use fwob_core::{FormatVersion, ReaderOptions, VerificationReport};

pub fn detect_format(path: impl AsRef<Path>) -> Result<FormatVersion> {
    let path = path.as_ref();
    let mut file = File::open(path)?;
    let mut magic = [0u8; 4];
    file.read_exact(&mut magic)?;
    if &magic == fwob_v1::SIGNATURE {
        Ok(FormatVersion::V1)
    } else if &magic == fwob_v2::MAGIC {
        Ok(FormatVersion::V2)
    } else {
        Err(Error::UnsupportedFormat(path.to_path_buf()))
    }
}

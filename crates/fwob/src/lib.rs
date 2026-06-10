use std::{
    fs::File,
    io::Read,
    path::{Path, PathBuf},
};

use fwob_core::{OwnedFrame, Schema};

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("unsupported FWOB file format: {0}")]
    UnsupportedFormat(PathBuf),
    #[error(transparent)]
    V1(#[from] fwob_v1::V1Error),
    #[error(transparent)]
    V2(#[from] fwob_v2::V2Error),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FormatVersion {
    V1,
    V2,
}

pub trait FwobFile {
    fn format_version(&self) -> FormatVersion;
    fn schema(&self) -> &Schema;
    fn title(&self) -> &str;
    fn frame_count(&self) -> u64;
    fn string_table(&self) -> &[String];
}

pub trait FwobReader: FwobFile {
    fn read_all_frames(&mut self) -> Result<Vec<OwnedFrame>>;
}

pub trait FwobAppender: FwobFile {
    fn append_frame(&mut self, frame: &[u8]) -> Result<()>;
    fn append_presorted_frames(&mut self, frames: &[u8]) -> Result<()>;
    fn finish(self: Box<Self>) -> Result<()>;
}

pub enum AnyReader {
    V1 {
        reader: fwob_v1::Reader<File>,
        string_table: Vec<String>,
    },
    V2(fwob_v2::Reader<File>),
}

impl AnyReader {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        Self::open_with_v1_key(path, 0)
    }

    pub fn open_with_v1_key(path: impl AsRef<Path>, key_field_index: usize) -> Result<Self> {
        let path = path.as_ref();
        match detect_format(path)? {
            FormatVersion::V1 => {
                let mut reader = fwob_v1::Reader::open(path, key_field_index)?;
                let string_table = reader.read_string_table()?;
                Ok(Self::V1 {
                    reader,
                    string_table,
                })
            }
            FormatVersion::V2 => Ok(Self::V2(fwob_v2::Reader::open(path)?)),
        }
    }
}

impl FwobFile for AnyReader {
    fn format_version(&self) -> FormatVersion {
        match self {
            Self::V1 { .. } => FormatVersion::V1,
            Self::V2(_) => FormatVersion::V2,
        }
    }

    fn schema(&self) -> &Schema {
        match self {
            Self::V1 { reader, .. } => reader.schema(),
            Self::V2(reader) => &reader.header().schema,
        }
    }

    fn title(&self) -> &str {
        match self {
            Self::V1 { reader, .. } => &reader.header().title,
            Self::V2(reader) => &reader.header().title,
        }
    }

    fn frame_count(&self) -> u64 {
        match self {
            Self::V1 { reader, .. } => reader.frame_count(),
            Self::V2(reader) => reader.header().frame_count,
        }
    }

    fn string_table(&self) -> &[String] {
        match self {
            Self::V1 { string_table, .. } => string_table,
            Self::V2(reader) => &reader.header().string_table,
        }
    }
}

impl FwobReader for AnyReader {
    fn read_all_frames(&mut self) -> Result<Vec<OwnedFrame>> {
        match self {
            Self::V1 { reader, .. } => Ok(reader.read_all_frames()?),
            Self::V2(reader) => Ok(reader.read_all_frames()?),
        }
    }
}

pub struct AppendOptions {
    pub v1_key_field_index: usize,
    pub v2: fwob_v2::WriterOptions,
}

impl Default for AppendOptions {
    fn default() -> Self {
        Self {
            v1_key_field_index: 0,
            v2: fwob_v2::WriterOptions::new(""),
        }
    }
}

pub enum AnyAppender {
    V1 {
        writer: Box<fwob_v1::Writer<File>>,
        string_table: Vec<String>,
    },
    V2(Box<fwob_v2::Writer<File>>),
}

impl AnyAppender {
    pub fn open(path: impl AsRef<Path>, options: AppendOptions) -> Result<Self> {
        let path = path.as_ref();
        match detect_format(path)? {
            FormatVersion::V1 => {
                let mut reader = fwob_v1::Reader::open(path, options.v1_key_field_index)?;
                let string_table = reader.read_string_table()?;
                drop(reader);
                Ok(Self::V1 {
                    writer: Box::new(fwob_v1::Writer::open_append(
                        path,
                        options.v1_key_field_index,
                    )?),
                    string_table,
                })
            }
            FormatVersion::V2 => Ok(Self::V2(Box::new(fwob_v2::Writer::open_append(
                path, options.v2,
            )?))),
        }
    }

    pub fn finish(self) -> Result<()> {
        Box::new(self).finish()
    }
}

impl FwobFile for AnyAppender {
    fn format_version(&self) -> FormatVersion {
        match self {
            Self::V1 { .. } => FormatVersion::V1,
            Self::V2(_) => FormatVersion::V2,
        }
    }

    fn schema(&self) -> &Schema {
        match self {
            Self::V1 { writer, .. } => writer.schema(),
            Self::V2(writer) => writer.schema(),
        }
    }

    fn title(&self) -> &str {
        match self {
            Self::V1 { writer, .. } => &writer.header().title,
            Self::V2(writer) => &writer.header().title,
        }
    }

    fn frame_count(&self) -> u64 {
        match self {
            Self::V1 { writer, .. } => writer.frame_count(),
            Self::V2(writer) => writer.frame_count(),
        }
    }

    fn string_table(&self) -> &[String] {
        match self {
            Self::V1 { string_table, .. } => string_table,
            Self::V2(writer) => &writer.header().string_table,
        }
    }
}

impl FwobAppender for AnyAppender {
    fn append_frame(&mut self, frame: &[u8]) -> Result<()> {
        match self {
            Self::V1 { writer, .. } => Ok(writer.append_frame(frame)?),
            Self::V2(writer) => Ok(writer.append_frame(frame)?),
        }
    }

    fn append_presorted_frames(&mut self, frames: &[u8]) -> Result<()> {
        match self {
            Self::V1 { writer, .. } => Ok(writer.append_presorted_raw_frames(frames)?),
            Self::V2(writer) => Ok(writer.append_presorted_raw_frames(frames)?),
        }
    }

    fn finish(self: Box<Self>) -> Result<()> {
        match *self {
            Self::V1 { .. } => Ok(()),
            Self::V2(writer) => Ok((*writer).finish()?),
        }
    }
}

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

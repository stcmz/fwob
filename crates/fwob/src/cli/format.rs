use std::{fs::File, io::Read, path::Path};

use super::*;

#[derive(Debug, Clone, Copy)]
pub(super) enum Format {
    V1,
    V2,
}

pub(super) fn detect_format(path: &Path) -> Result<Format> {
    let mut file = File::open(path)?;
    let mut magic = [0u8; 4];
    file.read_exact(&mut magic)?;
    match &magic {
        b"FWOB" => Ok(Format::V1),
        b"FWB2" => Ok(Format::V2),
        _ => bail!("unrecognized FWOB file signature"),
    }
}

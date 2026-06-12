use std::path::Path;

use fwob_core::{FormatVersion, ReaderOptions, VerificationReport};

use crate::{detect_format, Result};

pub struct Maintenance;

impl Maintenance {
    pub fn light_verify(
        path: impl AsRef<Path>,
        options: ReaderOptions,
    ) -> Result<VerificationReport> {
        let path = path.as_ref();
        match detect_format(path)? {
            FormatVersion::V1 => Ok(fwob_core::Maintenance::light_verify(
                &fwob_v1::MaintenanceService,
                path,
                options,
            )?),
            FormatVersion::V2 => Ok(fwob_core::Maintenance::light_verify(
                &fwob_v2::MaintenanceService,
                path,
                options,
            )?),
        }
    }

    pub fn verify(path: impl AsRef<Path>, options: ReaderOptions) -> Result<VerificationReport> {
        let path = path.as_ref();
        match detect_format(path)? {
            FormatVersion::V1 => Ok(fwob_core::Maintenance::verify(
                &fwob_v1::MaintenanceService,
                path,
                options,
            )?),
            FormatVersion::V2 => Ok(fwob_core::Maintenance::verify(
                &fwob_v2::MaintenanceService,
                path,
                options,
            )?),
        }
    }

    pub fn repair(path: impl AsRef<Path>, options: ReaderOptions) -> Result<VerificationReport> {
        let path = path.as_ref();
        match detect_format(path)? {
            FormatVersion::V1 => Ok(fwob_core::Maintenance::repair(
                &fwob_v1::MaintenanceService,
                path,
                options,
            )?),
            FormatVersion::V2 => Ok(fwob_core::Maintenance::repair(
                &fwob_v2::MaintenanceService,
                path,
                options,
            )?),
        }
    }
}

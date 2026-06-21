use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
};

use anyhow::{bail, Context, Result};

/// Resolve input paths to a deduplicated, lexicographically sorted list of `*.fwob` files. A file
/// input is taken as-is; a directory input contributes its top-level `*.fwob` entries. Shared by
/// the `ls` and `edit` commands.
pub(super) fn discover_files(inputs: &[PathBuf]) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    let mut seen = HashSet::new();
    for input in inputs {
        let metadata = fs::metadata(input)
            .with_context(|| format!("failed to inspect {}", input.display()))?;
        if metadata.is_file() {
            push_unique(&mut files, &mut seen, input.clone())?;
        } else if metadata.is_dir() {
            for entry in fs::read_dir(input)
                .with_context(|| format!("failed to read directory {}", input.display()))?
            {
                let entry = entry?;
                let path = entry.path();
                if entry.file_type()?.is_file() && has_fwob_extension(&path) {
                    push_unique(&mut files, &mut seen, path)?;
                }
            }
        } else {
            bail!("{} is not a regular file or directory", input.display());
        }
    }
    files.sort_by_cached_key(|path| path.to_string_lossy().to_lowercase());
    Ok(files)
}

fn push_unique(files: &mut Vec<PathBuf>, seen: &mut HashSet<PathBuf>, path: PathBuf) -> Result<()> {
    let identity =
        fs::canonicalize(&path).with_context(|| format!("failed to resolve {}", path.display()))?;
    if seen.insert(identity) {
        files.push(path);
    }
    Ok(())
}

fn has_fwob_extension(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("fwob"))
}

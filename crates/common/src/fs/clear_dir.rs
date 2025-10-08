use std::{
    fs::{read_dir, remove_dir_all, remove_file},
    path::Path,
};

use anyhow::{Context, Result};

/// This method clears the passed directory of all of the files and directories contained within
/// without deleting the directory.
pub fn clear_directory(path: impl AsRef<Path>) -> Result<()> {
    for entry in read_dir(path.as_ref())
        .with_context(|| format!("Failed to read directory: {}", path.as_ref().display()))?
    {
        let entry = entry.with_context(|| {
            format!("Failed to read an entry in directory: {}", path.as_ref().display())
        })?;
        let entry_path = entry.path();

        if entry_path.is_file() {
            remove_file(&entry_path)
                .with_context(|| format!("Failed to remove file: {}", entry_path.display()))?
        } else {
            remove_dir_all(&entry_path)
                .with_context(|| format!("Failed to remove directory: {}", entry_path.display()))?
        }
    }
    Ok(())
}

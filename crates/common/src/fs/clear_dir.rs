use std::{
    fs::{read_dir, remove_dir_all, remove_file},
    path::Path,
};

use anyhow::{Result, bail};

/// This method clears the passed directory of all of the files and directories contained within
/// without deleting the directory.
pub fn clear_directory(path: impl AsRef<Path>) -> Result<()> {
    let path = path.as_ref();

    if !path.is_dir() {
        bail!("The provided path is not a directory: {}", path.display());
    }

    for entry in read_dir(path)? {
        let entry = entry?;
        let entry_path = entry.path();

        if entry_path.is_file() {
            remove_file(entry_path)?
        } else {
            remove_dir_all(entry_path)?
        }
    }

    Ok(())
}

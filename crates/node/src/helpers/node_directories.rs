use crate::internal_prelude::*;

// Allows for easy creation of the paths with auto-cleanup behavior when dropped.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct NodeDirectories {
    // Base directory of the node which contains the node's ID. e.g., workdir/geth/0
    base_directory: PathBuf,
    // Data directory of the node used to house the database if needed.
    data_directory: PathBuf,
    // A directory used to house the logs.
    logs_directory: PathBuf,
}

impl NodeDirectories {
    pub fn new(
        root_directory: impl AsRef<Path>,
        name: impl AsRef<str>,
        node_id: u32,
    ) -> anyhow::Result<Self> {
        let base_directory = root_directory
            .as_ref()
            .join(name.as_ref())
            .join(node_id.to_string());
        let data_directory = base_directory.join("data");
        let logs_directory = base_directory.join("logs");

        // Ignore errors since it might error out if the directories do not exist already.
        let _ = remove_dir_all(base_directory.as_path());
        let _ = remove_dir_all(data_directory.as_path());
        let _ = remove_dir_all(logs_directory.as_path());

        create_dir_all(base_directory.as_path()).context("Failed to crease base directory")?;
        create_dir_all(data_directory.as_path()).context("Failed to crease data directory")?;
        create_dir_all(logs_directory.as_path()).context("Failed to crease logs directory")?;

        Ok(Self {
            base_directory,
            data_directory,
            logs_directory,
        })
    }

    pub fn base_directory(&self) -> &Path {
        &self.base_directory
    }

    pub fn data_directory(&self) -> &Path {
        &self.data_directory
    }

    pub fn logs_directory(&self) -> &Path {
        &self.logs_directory
    }
}

impl Drop for NodeDirectories {
    fn drop(&mut self) {
        remove_dir_all(self.data_directory.as_path())
            .expect("Failed to delete the data directory on drop")
    }
}

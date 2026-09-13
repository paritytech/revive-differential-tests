use crate::internal_prelude::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RetesterHomeDirectory;

impl RetesterHomeDirectory {
    pub fn get_path() -> &'static Path {
        static PATH: OnceLock<PathBuf> = OnceLock::new();

        PATH.get_or_init(|| {
            let path = match env::var_os("RETESTER_HOME_DIR") {
                Some(path) => PathBuf::from(path),
                None => PathBuf::from(
                    env::var_os("HOME")
                        .filter(|home| !home.is_empty())
                        .expect("Cannot resolve retester home: HOME is not set"),
                )
                .join(".retester"),
            };
            absolute(path).expect("Cannot resolve retester home directory")
        })
        .as_path()
    }
}

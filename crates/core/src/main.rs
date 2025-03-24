use std::collections::BTreeSet;

use clap::Parser;
use rayon::prelude::*;

use revive_dt_config::*;
use revive_dt_core::driver::compiler::build_evm;
use revive_dt_format::corpus::Corpus;
use temp_dir::TempDir;

fn main() -> anyhow::Result<()> {
    env_logger::init();

    let mut args = Arguments::parse();
    if args.corpus.is_empty() {
        anyhow::bail!("no test corpus specified");
    }
    let temp_dir = TempDir::new()?;
    args.working_directory.get_or_insert(temp_dir.path().into());

    for path in args.corpus.iter().collect::<BTreeSet<_>>() {
        log::trace!("attempting corpus {path:?}");
        let corpus = Corpus::try_from_path(path)?;
        log::info!("found corpus: {corpus:?}");

        let tests = corpus.enumerate_tests();
        log::info!("found {} tests", tests.len());

        tests.par_iter().for_each(|metadata| {
            let _ = match build_evm(metadata) {
                Ok(build) => {
                    log::info!(
                        "metadata {} compilation success",
                        metadata.file_path.as_ref().unwrap().display()
                    );
                    build
                }
                Err(error) => {
                    log::warn!(
                        "metadata {} compilation failure: {error:?}",
                        metadata.file_path.as_ref().unwrap().display()
                    );
                    return;
                }
            };
        });
    }

    Ok(())
}

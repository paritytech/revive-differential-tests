use std::collections::BTreeSet;

use clap::Parser;
use rayon::prelude::*;

use revive_dt_config::*;
use revive_dt_core::{Geth, Kitchensink, driver::Driver};
use revive_dt_format::corpus::Corpus;
use temp_dir::TempDir;

fn main() -> anyhow::Result<()> {
    env_logger::init();

    let mut args = Arguments::parse();
    if args.corpus.is_empty() {
        anyhow::bail!("no test corpus specified");
    }
    if args.working_directory.is_none() {
        args.temp_dir = TempDir::new()?.into()
    }

    for path in args.corpus.iter().collect::<BTreeSet<_>>() {
        log::trace!("attempting corpus {path:?}");
        let corpus = Corpus::try_from_path(path)?;
        log::info!("found corpus: {corpus:?}");

        let tests = corpus.enumerate_tests();
        log::info!("found {} tests", tests.len());

        tests.par_iter().for_each(|metadata| {
            let mut driver = match (&args.leader, &args.follower) {
                (TestingPlatform::Geth, TestingPlatform::Kitchensink) => {
                    Driver::<Geth, Kitchensink>::new(metadata, &args)
                }
                _ => unimplemented!(),
            };

            match driver.execute() {
                Ok(build) => {
                    log::info!(
                        "metadata {} success",
                        metadata.directory().as_ref().unwrap().display()
                    );
                    build
                }
                Err(error) => {
                    log::warn!(
                        "metadata {} failure: {error:?}",
                        metadata.file_path.as_ref().unwrap().display()
                    );
                }
            }
        });
    }

    Ok(())
}

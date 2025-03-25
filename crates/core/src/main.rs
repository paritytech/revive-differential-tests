use std::collections::BTreeSet;

use clap::Parser;
use rayon::{ThreadPoolBuilder, prelude::*};

use revive_dt_config::*;
use revive_dt_core::{Geth, Kitchensink, driver::Driver};
use revive_dt_format::corpus::Corpus;
use revive_dt_node::{Node, geth};
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

    ThreadPoolBuilder::new()
        .num_threads(args.workers)
        .build_global()
        .unwrap();

    for path in args.corpus.iter().collect::<BTreeSet<_>>() {
        log::trace!("attempting corpus {path:?}");
        let corpus = Corpus::try_from_path(path)?;
        log::info!("found corpus: {corpus:?}");

        let tests = corpus.enumerate_tests();
        log::info!("corpus '{}' contains {} tests", &corpus.name, tests.len());

        tests.par_iter().for_each(|metadata| {
            let (leader, follower) = match (&args.leader, &args.follower) {
                (TestingPlatform::Geth, TestingPlatform::Kitchensink) => {
                    (geth::Instance::new(&args), geth::Instance::new(&args))
                }
                _ => unimplemented!(),
            };
            let mut driver = match (&args.leader, &args.follower) {
                (TestingPlatform::Geth, TestingPlatform::Kitchensink) => {
                    Driver::<Geth, Geth>::new(metadata, &args)
                }
                _ => unimplemented!(),
            };

            match Driver::<Geth, Geth>::new(metadata, &args).execute(leader, follower) {
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

use std::{collections::HashMap, sync::LazyLock};

use clap::Parser;
use rayon::{ThreadPoolBuilder, prelude::*};

use revive_dt_config::*;
use revive_dt_core::{
    Geth, Kitchensink,
    driver::{Driver, State},
};
use revive_dt_format::{corpus::Corpus, metadata::Metadata};
use revive_dt_node::pool::NodePool;
use revive_dt_report::reporter::{Report, Span};
use temp_dir::TempDir;

static TEMP_DIR: LazyLock<TempDir> = LazyLock::new(|| TempDir::new().unwrap());

fn main() -> anyhow::Result<()> {
    let args = init_cli()?;

    for (corpus, tests) in collect_corpora(&args)? {
        let span = Span::new(corpus, args.clone())?;

        match &args.compile_only {
            Some(platform) => compile_corpus(&args, &tests, platform, span),
            None => execute_corpus(&args, &tests, span)?,
        }

        Report::save()?;
    }

    Ok(())
}

fn init_cli() -> anyhow::Result<Arguments> {
    env_logger::init();

    let mut args = Arguments::parse();

    if args.corpus.is_empty() {
        anyhow::bail!("no test corpus specified");
    }

    match args.working_directory.as_ref() {
        Some(dir) => {
            if !dir.exists() {
                anyhow::bail!("workdir {} does not exist", dir.display());
            }
        }
        None => {
            args.temp_dir = Some(&TEMP_DIR);
        }
    }
    log::info!("workdir: {}", args.directory().display());

    ThreadPoolBuilder::new()
        .num_threads(args.workers)
        .build_global()?;

    Ok(args)
}

fn collect_corpora(args: &Arguments) -> anyhow::Result<HashMap<Corpus, Vec<Metadata>>> {
    let mut corpora = HashMap::new();

    for path in &args.corpus {
        let corpus = Corpus::try_from_path(path)?;
        log::info!("found corpus: {}", path.display());
        let tests = corpus.enumerate_tests();
        log::info!("corpus '{}' contains {} tests", &corpus.name, tests.len());
        corpora.insert(corpus, tests);
    }

    Ok(corpora)
}

fn execute_corpus(args: &Arguments, tests: &[Metadata], span: Span) -> anyhow::Result<()> {
    let leader_nodes = NodePool::new(args)?;
    let follower_nodes = NodePool::new(args)?;

    tests.par_iter().for_each(|metadata| {
        let mut driver = match (&args.leader, &args.follower) {
            (TestingPlatform::Geth, TestingPlatform::Kitchensink) => Driver::<Geth, Geth>::new(
                metadata,
                args,
                leader_nodes.round_robbin(),
                follower_nodes.round_robbin(),
            ),
            _ => unimplemented!(),
        };

        match driver.execute(span) {
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

    Ok(())
}

fn compile_corpus(config: &Arguments, tests: &[Metadata], platform: &TestingPlatform, span: Span) {
    tests.par_iter().for_each(|metadata| {
        for mode in &metadata.solc_modes() {
            match platform {
                TestingPlatform::Geth => {
                    let mut state = State::<Geth>::new(config, span);
                    let _ = state.build_contracts(mode, metadata);
                }
                TestingPlatform::Kitchensink => {
                    let mut state = State::<Kitchensink>::new(config, span);
                    let _ = state.build_contracts(mode, metadata);
                }
            };
        }
    });
}

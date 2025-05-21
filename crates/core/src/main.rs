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

    let corpora = collect_corpora(&args)?;

    for (corpus, tests) in corpora.iter() {
        Report::save()?;

        let span = Span::new(corpus.clone());

        if let Some(platform) = &args.compile_only {
            main_compile_only(&args, tests, platform, span);
            continue;
        }

        main_execute_differential(&args, tests, span)?;
    }

    Ok(())
}

fn init_cli() -> anyhow::Result<Arguments> {
    env_logger::init();

    let mut args = Arguments::parse();
    if args.corpus.is_empty() {
        anyhow::bail!("no test corpus specified");
    }
    if args.working_directory.is_none() {
        args.temp_dir = Some(&TEMP_DIR);
    }

    ThreadPoolBuilder::new()
        .num_threads(args.workers)
        .build_global()
        .unwrap();

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

fn main_execute_differential(
    args: &Arguments,
    tests: &[Metadata],
    span: Span,
) -> anyhow::Result<()> {
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

fn main_compile_only(
    config: &Arguments,
    tests: &[Metadata],
    platform: &TestingPlatform,
    span: Span,
) {
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

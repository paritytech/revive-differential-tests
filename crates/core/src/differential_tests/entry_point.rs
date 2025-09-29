//! The main entry point into differential testing.

use std::collections::BTreeMap;

use anyhow::Context as _;
use revive_dt_core::Platform;
use tracing::{error, instrument};

use revive_dt_config::{Context, TestExecutionContext};
use revive_dt_report::Reporter;

use crate::helpers::{NodePool, collect_metadata_files};

/// Handles the differential testing executing it according to the information defined in the
/// context
#[instrument(level = "info", err(Debug), skip_all)]
pub async fn handle_differential_tests(
    context: TestExecutionContext,
    reporter: Reporter,
) -> anyhow::Result<()> {
    // Discover all of the metadata files that are defined in the context.
    let metadata_files = collect_metadata_files(&context)
        .context("Failed to collect metadata files for differential testing")?;

    // Discover the list of platforms that the tests should run on based on the context.
    let platforms = context
        .platforms
        .iter()
        .copied()
        .map(Into::<&dyn Platform>::into)
        .collect::<Vec<_>>();

    // Starting the nodes of the various platforms specified in the context.
    let platforms_and_nodes = {
        let mut map = BTreeMap::new();

        for platform in platforms.iter() {
            let platform_identifier = platform.platform_identifier();

            let context = Context::ExecuteTests(Box::new(context.clone()));
            let node_pool = NodePool::new(context, *platform)
                .await
                .inspect_err(|err| {
                    error!(
                        ?err,
                        %platform_identifier,
                        "Failed to initialize the node pool for the platform."
                    )
                })
                .context("Failed to initialize the node pool")?;

            map.insert(platform_identifier, (*platform, node_pool));
        }

        map
    };

    todo!()
}

//! The flood workload's smoke: three agents and short phases, run in the
//! ordinary test lane so the workload keeps working between perf runs. It
//! checks the workload measures everything it reports, not the budgets:
//! those hold only for release builds on an enrolled machine.

#![cfg(unix)]

use qualification::perf::flood::{self, FloodOptions};

#[tokio::test(flavor = "multi_thread")]
async fn flood_smoke_with_three_agents_measures_every_metric() {
    let runs = flood::run(&FloodOptions::smoke()).await.unwrap();
    let names: Vec<&str> = runs.iter().map(|run| run.metric.name).collect();
    assert_eq!(
        names,
        [
            "flood fleet caught up",
            "flood chat caught up",
            "flood ingest lag",
            "flood agent process memory",
            "flood catch-up under K",
            "flood catch-up over K",
            "flood backlog growth with the daemon killed",
            "flood backlog drain after restart",
            "flood ingest cost per frame",
        ]
    );
    for run in &runs {
        assert!(
            !run.samples.is_empty(),
            "{} has no samples",
            run.metric.name
        );
        for sample in &run.samples {
            assert!(
                sample.value.is_finite() && sample.value >= 0.0,
                "{}: {}",
                run.metric.name,
                sample.value
            );
        }
        println!(
            "{}: {:?}",
            run.metric.name,
            run.samples
                .iter()
                .map(|sample| sample.value)
                .collect::<Vec<_>>()
        );
    }
}

/// The served flood is the measured one: `testnet serve
/// journeys/topologies/flood.json` hands a client the topology the perf
/// lane measures. Regenerate the file with FLOOD_TOPOLOGY_UPDATE=1.
#[test]
fn the_served_flood_topology_is_the_measured_one() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../journeys/topologies/flood.json");
    let measured = flood::topology(&FloodOptions::full());
    if std::env::var_os("FLOOD_TOPOLOGY_UPDATE").is_some() {
        let mut text = serde_json::to_string_pretty(&measured).unwrap();
        text.push('\n');
        std::fs::write(&path, text).unwrap();
    }
    let served = testnet::Topology::load(&path).unwrap();
    assert_eq!(served, measured);
}

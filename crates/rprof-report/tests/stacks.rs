use rprof_report::{
    ReportTrack,
    stacks::{Data, Group, Metric, Orientation, Query, aggregate},
};
use serde::Deserialize;

#[derive(Deserialize)]
struct Fixture {
    data: Data,
    tracks: Vec<ReportTrack>,
}
fn fixture() -> Fixture {
    serde_json::from_str(include_str!("../../../fixtures/stack-profile.json")).unwrap()
}
fn query(metric: Metric, begin: &str, end: &str) -> Query {
    Query {
        snapshot: "fixture".into(),
        domain: "d".into(),
        metric,
        start_ns: begin.into(),
        end_ns: end.into(),
        orientation: Orientation::TopDown,
        group: Group::Combined,
        filter: String::new(),
    }
}
#[test]
fn orientations_preserve_recursion_unattributed_weight_and_exact_large_totals() {
    let mut fixture = fixture();
    fixture.data.facts[0].weight = "9007199254740993".into();
    fixture.data.facts[1].weight = "9007199254740994".into();
    let mut query = query(Metric::Cpu, "0", "1000");
    let down = aggregate(&fixture.data, &fixture.tracks, &query).unwrap();
    assert_eq!(down.total, "18014398509481999");
    assert_eq!(down.observations, "4");
    assert_eq!(down.unattributed, "5");
    assert_eq!(
        down.nodes.iter().filter(|n| n.frame.key == "work").count(),
        4
    );
    assert_eq!(
        down.nodes
            .iter()
            .filter(|n| n.parent.is_none())
            .map(|n| n.total.parse::<u128>().unwrap())
            .sum::<u128>()
            .to_string(),
        down.total
    );
    query.orientation = Orientation::BottomUp;
    let up = aggregate(&fixture.data, &fixture.tracks, &query).unwrap();
    assert_eq!(down.total, up.total);
    let work = up
        .nodes
        .iter()
        .find(|n| n.parent.is_none() && n.frame.key == "work")
        .unwrap();
    assert_eq!(work.self_weight, "18014398509481994");
}
#[test]
fn range_clipping_completion_bytes_and_launch_attribution_have_distinct_semantics() {
    let fixture = fixture();
    let run = |metric, begin, end| {
        aggregate(&fixture.data, &fixture.tracks, &query(metric, begin, end)).unwrap()
    };
    let time = run(Metric::IoTime, "75", "125");
    assert_eq!(
        (time.total.as_str(), time.observations.as_str()),
        ("50", "0")
    );
    assert_eq!(run(Metric::IoBytes, "75", "125").total, "0");
    assert_eq!(run(Metric::IoBytes, "150", "151").total, "9007199254740993");
    assert_eq!(run(Metric::IoTime, "150", "151").total, "0");
    assert_eq!(run(Metric::IoTime, "150", "151").observations, "1");
    assert_eq!(run(Metric::Cpu, "20", "30").total, "100");
    let gpu = run(Metric::GpuTime, "130", "150");
    assert_eq!(gpu.total, "20");
    assert_eq!(gpu.unattributed, "0");
    assert!(gpu.nodes.iter().any(|n| n.frame.name == "train_step"));
    let mut filtered = query(Metric::GpuTime, "130", "150");
    filtered.filter = "cuda".into();
    assert_eq!(
        aggregate(&fixture.data, &fixture.tracks, &filtered)
            .unwrap()
            .total,
        "20"
    );
    assert_eq!(run(Metric::CudaApi, "130", "150").total, "0");
}
#[test]
fn scope_grouping_ambiguous_correlations_and_incomplete_exports_remain_explicit() {
    let mut fixture = fixture();
    let mut q = query(Metric::IoTime, "0", "1000");
    q.group = Group::Thread;
    q.filter = "rank:0 thread 2".into();
    let tree = aggregate(&fixture.data, &fixture.tracks, &q).unwrap();
    assert_eq!(tree.nodes[0].frame.key, "track:s:t");
    q.filter = "rank:1".into();
    assert_eq!(
        aggregate(&fixture.data, &fixture.tracks, &q).unwrap().total,
        "0"
    );
    fixture
        .data
        .sources
        .get_mut("host-launch")
        .unwrap()
        .push("compute".into());
    let gpu = aggregate(
        &fixture.data,
        &fixture.tracks,
        &query(Metric::GpuTime, "0", "1000"),
    )
    .unwrap();
    assert_eq!(gpu.unattributed, "40");
    fixture.data.complete = false;
    assert!(
        aggregate(&fixture.data, &fixture.tracks, &q)
            .unwrap_err()
            .to_string()
            .contains("complete stack")
    );
}

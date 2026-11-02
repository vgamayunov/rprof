use rprof_project::*;
use std::collections::BTreeMap;

fn request() -> Request {
    let source =
        serde_json::from_str(include_str!("../../../fixtures/projection/source.json")).unwrap();
    let target =
        serde_json::from_str(include_str!("../../../fixtures/projection/target.json")).unwrap();
    let workload: WorkloadModel =
        serde_json::from_str(include_str!("../../../fixtures/projection/workload.json")).unwrap();
    let spans = [
        ("prepare", "cpu", 0, 20),
        ("kernel A", "gpu-a", 20, 100),
        ("kernel B", "gpu-b", 20, 80),
        ("write", "cpu", 120, 40),
    ]
    .into_iter()
    .enumerate()
    .map(|(i, (name, lane, start, duration))| ObservedSpan {
        id: workload.phases[i].span_id.clone(),
        name: name.into(),
        category: "test".into(),
        lane: lane.into(),
        start_ns: start.to_string(),
        duration_ns: duration.to_string(),
        incomplete: false,
        enclosing: false,
        attributes: BTreeMap::new(),
    })
    .collect();
    Request {
        version: 1,
        baseline: Baseline {
            capture_sha256: workload.baseline_sha256.clone(),
            session_id: "synthetic".into(),
            domain: "0".into(),
            duration_ns: "160".into(),
            complete: true,
            dropped: "0".into(),
            omitted: "0".into(),
            clock_uncertainty_ns: None,
            warnings: vec![],
            metadata: BTreeMap::from([("provenance".into(), "synthetic".into())]),
            spans,
        },
        source,
        targets: vec![target],
        workload,
        nodes: vec![],
        scaling: Scaling::Fixed,
        validation: vec![],
    }
}
fn runtime(r: &Projection) -> &str {
    &r.scenarios[0].runtime.as_ref().unwrap().central_ns
}

#[test]
fn overlap_and_bottleneck_shift_are_rescheduled_not_summed() {
    let p = project(request()).unwrap();
    assert_eq!(runtime(&p), "120");
    assert_eq!(p.baseline_runtime_ns, "160");
    assert_eq!(p.source_reconstructed_ns, "160");
    assert_eq!(p.covered_ns, "160");
    let s = &p.scenarios[0];
    assert_eq!(s.critical_path, vec!["prepare", "kernel-b", "checkpoint"]);
    let b = &s.tasks[2];
    assert_eq!(b.bottleneck, "memory bandwidth");
    assert_eq!(b.start_ns, "20");
    assert_eq!(b.duration_ns, "80");
    assert_eq!(s.tasks[1].duration_ns, "50");
    assert_eq!(s.tasks[3].start_ns, "100");
    assert_eq!(
        s.resources
            .iter()
            .find(|r| r.resource == "gpu")
            .unwrap()
            .busy_ns,
        "130"
    );
}
#[test]
fn resource_contention_and_same_hardware_reconstruction() {
    let mut r = request();
    r.targets[0]
        .resources
        .get_mut("gpu")
        .unwrap()
        .slots_per_node = 1;
    let p = project(r).unwrap();
    assert_eq!(runtime(&p), "170");
    assert_eq!(p.scenarios[0].tasks[2].start_ns, "70");
    let mut r = request();
    r.targets = vec![r.source.clone()];
    let p = project(r).unwrap();
    assert_eq!(runtime(&p), "160");
    assert_eq!(
        p.scenarios[0].runtime.as_ref().unwrap().optimistic_ns,
        "160"
    );
}
#[test]
fn memory_and_feature_failure_never_yield_numeric_speedups() {
    let mut r = request();
    r.targets[0]
        .resources
        .get_mut("gpu")
        .unwrap()
        .capacity_bytes_per_node = Some("1024".into());
    let p = project(r.clone()).unwrap();
    assert_eq!(p.scenarios[0].status, "infeasible");
    assert!(p.scenarios[0].runtime.is_none() && p.scenarios[0].speedup.is_none());
    r.scaling = Scaling::Strong;
    r.nodes = vec![2];
    assert_eq!(project(r).unwrap().scenarios[0].status, "estimated");
    let mut r = request();
    r.targets[0]
        .resources
        .get_mut("gpu")
        .unwrap()
        .features
        .clear();
    assert_eq!(project(r).unwrap().scenarios[0].status, "infeasible");
}
#[test]
fn missing_metrics_unknown_capacity_and_unsupported_phases_are_explicit() {
    let mut r = request();
    r.targets[0]
        .resources
        .get_mut("gpu")
        .unwrap()
        .metrics
        .clear();
    let p = project(r).unwrap();
    assert_eq!(p.scenarios[0].status, "unsupported");
    assert!(p.scenarios[0].runtime.is_none());
    let mut r = request();
    r.targets[0]
        .resources
        .get_mut("gpu")
        .unwrap()
        .capacity_bytes_per_node = None;
    assert_eq!(project(r).unwrap().scenarios[0].status, "unsupported");
    let mut r = request();
    r.workload.phases[1].model = PhaseModel::Unsupported {
        reason: "missing work/traffic evidence".into(),
    };
    assert_eq!(project(r).unwrap().scenarios[0].status, "unsupported");
}
#[test]
fn node_sweeps_require_explicit_scaling_and_do_not_duplicate_lanes() {
    let mut r = request();
    r.nodes = vec![1, 2, 4];
    let p = project(r.clone()).unwrap();
    assert_eq!(p.scenarios[1].status, "unsupported");
    r.scaling = Scaling::Strong;
    let p = project(r.clone()).unwrap();
    assert_eq!(
        p.scenarios
            .iter()
            .map(|s| s.runtime.as_ref().unwrap().central_ns.as_str())
            .collect::<Vec<_>>(),
        ["120", "80", "60"]
    );
    assert!(p.scenarios.iter().all(|s| s.tasks.len() == 4));
    assert_eq!(p.scenarios[1].efficiency.as_deref(), Some("1.000000"));
    r.scaling = Scaling::Weak;
    assert!(
        project(r.clone()).unwrap().scenarios.iter().all(|s| s
            .runtime
            .as_ref()
            .unwrap()
            .central_ns
            == "120")
    );
    r.workload.scaling_assumption.clear();
    assert_eq!(project(r).unwrap().scenarios[2].status, "unsupported");
}
#[test]
fn latency_and_declared_node_overheads_can_dominate_bandwidth() {
    let mut r = request();
    for p in [&mut r.source, &mut r.targets[0]] {
        let prov = p.provenance.clone();
        p.resources.get_mut("storage").unwrap().metrics.insert(
            "latency".into(),
            Metric {
                unit: Unit::Nanoseconds,
                low: 40.0,
                central: 40.0,
                high: 40.0,
                provenance: prov,
            },
        );
    }
    r.workload.phases[3].model = PhaseModel::Transfer {
        bandwidth_metric: "bandwidth".into(),
        latency_metric: Some("latency".into()),
        bytes: "160".into(),
        operations: "1".into(),
    };
    let p = project(r.clone()).unwrap();
    assert_eq!(runtime(&p), "130");
    assert_eq!(p.scenarios[0].tasks[3].duration_ns, "30");
    assert_eq!(p.scenarios[0].tasks[3].bottleneck, "latency");
    r.scaling = Scaling::Strong;
    r.nodes = vec![2];
    r.workload.phases[3].node_scaling = NodeScaling::Power { exponent: 1 };
    assert_eq!(runtime(&project(r).unwrap()), "120");
}
#[test]
fn incomplete_evidence_and_uncovered_time_need_separate_assumptions() {
    let mut r = request();
    r.baseline.complete = false;
    assert_eq!(
        project(r.clone()).unwrap().scenarios[0].status,
        "unsupported"
    );
    r.workload.allow_partial_evidence = true;
    assert_eq!(project(r.clone()).unwrap().scenarios[0].status, "estimated");
    r.baseline.duration_ns = "180".into();
    r.workload.window_duration_ns = "180".into();
    assert_eq!(
        project(r.clone()).unwrap().scenarios[0].status,
        "unsupported"
    );
    r.workload.unmodeled = Unmodeled::Unchanged;
    r.workload.unmodeled_assumption = "Trailing 20 ns stays constant".into();
    let p = project(r).unwrap();
    assert_eq!(runtime(&p), "140");
    assert_eq!(p.covered_ns, "160");
    assert_eq!(p.source_reconstructed_ns, "180");
}
#[test]
fn parameter_cases_and_shared_calibration_identity_are_distinct_from_validation() {
    let mut r = request();
    let m = r.targets[0]
        .resources
        .get_mut("gpu")
        .unwrap()
        .metrics
        .get_mut("memory")
        .unwrap();
    m.low = 1e9;
    m.high = 4e9;
    let p = project(r.clone()).unwrap();
    let range = p.scenarios[0].runtime.as_ref().unwrap();
    assert_eq!(
        (
            &range.optimistic_ns,
            &range.central_ns,
            &range.pessimistic_ns
        ),
        (&"90".into(), &"120".into(), &"200".into())
    );
    assert!(p.scenarios[0].validation.is_empty());
    r.source = r.targets[0].clone();
    r.targets = vec![r.source.clone()];
    let p = project(r).unwrap();
    assert_eq!(
        p.scenarios[0].runtime.as_ref().unwrap().optimistic_ns,
        "160"
    );
    assert_eq!(
        p.scenarios[0].runtime.as_ref().unwrap().pessimistic_ns,
        "160"
    );
}
#[test]
fn malformed_models_and_inclusive_or_cyclic_graphs_are_rejected() {
    let mut r = request();
    r.source.version = 99;
    assert!(project(r).is_err());
    let mut r = request();
    r.workload.baseline_sha256 = "0".repeat(64);
    assert!(project(r).is_err());
    let mut r = request();
    r.workload.phases[1].span_id = r.workload.phases[0].span_id.clone();
    assert!(project(r).is_err());
    let mut r = request();
    r.baseline.spans[0].enclosing = true;
    assert!(project(r).is_err());
    let mut r = request();
    r.baseline.spans[2].lane = "gpu-a".into();
    assert!(project(r).is_err());
    let mut r = request();
    r.workload.phases[0].after = vec!["checkpoint".into()];
    assert!(project(r).is_err());
    let mut r = request();
    r.targets[0]
        .resources
        .get_mut("gpu")
        .unwrap()
        .metrics
        .get_mut("compute")
        .unwrap()
        .low = 0.0;
    assert!(project(r).is_err());
    let mut r = request();
    r.targets[0].placement.threads_per_rank = 2;
    assert_eq!(project(r).unwrap().scenarios[0].status, "unsupported");
}
#[test]
fn measured_values_above_js_precision_survive_same_hardware_projection() {
    let mut r = request();
    r.workload.phases.truncate(1);
    r.baseline.spans.truncate(1);
    r.baseline.spans[0].duration_ns = "9007199254740993".into();
    r.baseline.duration_ns = r.baseline.spans[0].duration_ns.clone();
    r.workload.window_duration_ns = r.baseline.duration_ns.clone();
    assert_eq!(runtime(&project(r).unwrap()), "9007199254740993");
}
#[test]
fn held_out_errors_are_hash_bound_and_never_used_for_fitting() {
    let mut r = request();
    let before = project(r.clone()).unwrap();
    r.validation.push(ValidationObservation {
        target_id: r.targets[0].id.clone(),
        nodes: 1,
        observed_ns: "100".into(),
        workload_sha256: before.hashes["workload"].clone(),
        target_profile_sha256: before.hashes["target:faster"].clone(),
        provenance: Provenance {
            kind: ProvenanceKind::Measured,
            source: "independent held-out capture hash".into(),
            date: "2026-10-30".into(),
            conditions: "not used to fit rates".into(),
        },
    });
    let after = project(r.clone()).unwrap();
    assert_eq!(runtime(&before), runtime(&after));
    assert_eq!(
        after.scenarios[0].validation[0].signed_error_percent,
        "20.000000"
    );
    r.workload.phases[0].assumption.push_str(" edited");
    let edited = project(r).unwrap();
    assert!(edited.scenarios[0].validation.is_empty());
    assert!(
        edited.scenarios[0]
            .warnings
            .iter()
            .any(|w| w.contains("does not match"))
    );
}

#[test]
fn throughput_ratios_reject_mismatched_work_and_byte_units() {
    let mut r = request();
    r.workload.phases = vec![r.workload.phases[1].clone()];
    r.workload.phases[0].after.clear();
    r.workload.window_start_ns = "20".into();
    r.workload.window_duration_ns = "100".into();
    r.targets[0]
        .resources
        .get_mut("gpu")
        .unwrap()
        .metrics
        .get_mut("compute")
        .unwrap()
        .unit = Unit::BytesPerSecond;
    let p = project(r).unwrap();
    assert_eq!(p.scenarios[0].status, "unsupported");
    assert!(p.scenarios[0].runtime.is_none());
    assert!(
        p.scenarios[0]
            .issues
            .iter()
            .any(|issue| issue.contains("units differ"))
    );
}

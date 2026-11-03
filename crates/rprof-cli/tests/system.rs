#![cfg(target_os = "linux")]
use rprof_report::{Limits, analyze};
use std::{path::Path, process::Command};

fn cli(path: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_rprof"));
    command
        .args([
            "record",
            "--collect",
            "system",
            "--system-interval-ms",
            "100",
            "-o",
        ])
        .arg(path);
    command
}

#[test]
fn real_host_metrics_have_physical_hierarchy_units_and_shared_time() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("system.jsonl");
    let output = cli(&path)
        .args(["--", "sh", "-c", "sleep 0.45; exit 7"])
        .output()
        .unwrap();
    assert_eq!(
        output.status.code(),
        Some(7),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report = analyze(&path, Limits::default()).unwrap();
    assert!(report.summary.complete);
    assert_eq!(report.summary.dropped, "0");
    let roots: Vec<_> = report
        .tracks
        .iter()
        .filter(|t| t.kind == "system" && t.parent.is_none())
        .collect();
    assert_eq!(roots.len(), 2);
    assert!(
        roots
            .iter()
            .all(|t| t.rank.is_none() && !t.details["system.host_id"].is_empty())
    );
    let cpu = roots
        .iter()
        .find(|t| t.details["resource"] == "cpu")
        .unwrap();
    let memory = roots
        .iter()
        .find(|t| t.details["resource"] == "memory")
        .unwrap();
    let children: Vec<_> = report
        .tracks
        .iter()
        .filter(|t| t.parent.as_deref() == Some(&cpu.id))
        .collect();
    let cpu_ids: Vec<_> = std::fs::read_to_string("/proc/stat")
        .unwrap()
        .lines()
        .filter_map(|l| l.split_whitespace().next())
        .filter_map(|name| {
            name.strip_prefix("cpu")
                .and_then(|id| id.parse::<u32>().ok())
        })
        .collect();
    assert_eq!(children.len(), cpu_ids.len());
    for id in cpu_ids {
        assert!(
            children
                .iter()
                .any(|t| t.details["resource"] == format!("cpu:{id}"))
        );
    }
    let samples: Vec<_> = report
        .counters
        .iter()
        .filter(|c| c.track == cpu.id && c.name == "system.cpu.utilization")
        .collect();
    assert!(samples.len() >= 2);
    assert!(samples.iter().all(|c| c.unit == "%"
        && (0.0..=100.0).contains(&c.value.parse::<f64>().unwrap())
        && c.attributes["system.window_ns"].parse::<u64>().unwrap() > 0));
    let memory_samples: Vec<_> = report
        .counters
        .iter()
        .filter(|c| c.track == memory.id && c.name == "system.memory.used")
        .collect();
    assert!(memory_samples.len() >= 2);
    for sample in memory_samples {
        assert_eq!(sample.unit, "bytes");
        assert_eq!(
            sample.value.parse::<u64>().unwrap(),
            sample.attributes["system.memory.total_bytes"]
                .parse::<u64>()
                .unwrap()
                - sample.attributes["system.memory.free_bytes"]
                    .parse::<u64>()
                    .unwrap()
        );
    }
    assert_eq!(report.domains.len(), 1);
    // Native export handles new system tracks as well as process tracks.
    let trace = directory.path().join("trace.json");
    assert!(
        Command::new(env!("CARGO_BIN_EXE_rprof"))
            .args(["report"])
            .arg(&path)
            .args(["--format", "trace", "-o"])
            .arg(trace)
            .status()
            .unwrap()
            .success()
    );
}

#[test]
fn system_settings_require_explicit_collection() {
    let directory = tempfile::tempdir().unwrap();
    let marker = directory.path().join("ran");
    let output = Command::new(env!("CARGO_BIN_EXE_rprof"))
        .args(["record", "--system-interval-ms", "100", "--", "touch"])
        .arg(&marker)
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(!marker.exists());
}

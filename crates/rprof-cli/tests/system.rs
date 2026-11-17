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

fn gpu_cli(directory: &Path) -> Command {
    let library = directory.join("libnvidia-ml.so.1");
    let build = Command::new("cc")
        .args(["-shared", "-fPIC", "-Wall", "-Wextra", "-Werror"])
        .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/gpu_nvml.c"))
        .arg("-o")
        .arg(library)
        .output()
        .unwrap();
    assert!(
        build.status.success(),
        "{}",
        String::from_utf8_lossy(&build.stderr)
    );
    let mut command = Command::new(env!("CARGO_BIN_EXE_rprof"));
    command
        .env("LD_LIBRARY_PATH", directory)
        .env("CUDA_VISIBLE_DEVICES", "")
        .args([
            "record",
            "--collect",
            "gpu",
            "--gpu-interval-ms",
            "100",
            "-o",
        ])
        .arg(directory.join("gpu.jsonl"));
    command
}

#[test]
fn nvml_gauges_are_device_wide_and_partial_reads_are_not_idle_devices() {
    let directory = tempfile::tempdir().unwrap();
    let output = gpu_cli(directory.path())
        .args(["--", "sh", "-c", "sleep 0.65; exit 7"])
        .output()
        .unwrap();
    assert_eq!(
        output.status.code(),
        Some(7),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report = analyze(&directory.path().join("gpu.jsonl"), Limits::default()).unwrap();
    assert!(report.summary.complete);
    assert_eq!(report.summary.dropped, "0");
    assert!(
        report
            .summary
            .warnings
            .iter()
            .any(|warning| warning.contains("GPU monitoring")
                && warning.contains("unknown, not zero"))
    );
    let track = |resource: &str| {
        report
            .tracks
            .iter()
            .find(|track| track.details.get("resource").map(String::as_str) == Some(resource))
            .unwrap()
    };
    assert!(track("gpu").parent.is_none());
    assert!(track("gpu_memory").parent.is_none());
    assert_eq!(
        track("gpu:GPU-mock-0").parent.as_deref(),
        Some(track("gpu").id.as_str())
    );
    assert_eq!(
        track("gpu_memory:GPU-mock-1").parent.as_deref(),
        Some(track("gpu_memory").id.as_str())
    );
    let samples = |resource: &str, name: &str| {
        report
            .counters
            .iter()
            .filter(|counter| counter.track == track(resource).id && counter.name == name)
            .collect::<Vec<_>>()
    };
    let gpu = samples("gpu", "system.gpu.utilization");
    assert!(gpu.len() >= 4);
    assert!(gpu.iter().all(|counter| counter.value == "50"
        && counter.unit == "%"
        && counter.attributes["system.gpu.count"] == "2"));
    assert!(
        !gpu.iter()
            .any(|counter| counter.attributes["system.sample_index"] == "3")
    );
    assert!(
        gpu.iter()
            .any(|counter| counter.attributes["system.sample_index"] == "4"
                && counter.attributes["system.report_gap_before"] == "true")
    );
    assert!(
        samples("gpu:GPU-mock-0", "system.gpu.utilization")
            .iter()
            .any(|counter| counter.attributes["system.sample_index"] == "3")
    );
    assert!(
        !samples("gpu:GPU-mock-1", "system.gpu.utilization")
            .iter()
            .any(|counter| counter.attributes["system.sample_index"] == "3")
    );
    let memory = samples("gpu_memory", "system.gpu.memory.used");
    assert!(memory.iter().all(|counter| counter.value == "12884901890"
        && counter.value_type == "integer"
        && counter.unit == "bytes"
        && counter.attributes["system.memory.total_bytes"] == "34359738368"));
    assert!(
        !memory
            .iter()
            .any(|counter| counter.attributes["system.sample_index"] == "4")
    );
    assert!(
        samples("gpu_memory:GPU-mock-1", "system.gpu.memory.used")
            .iter()
            .any(|counter| counter.attributes["system.sample_index"] == "4")
    );
    assert!(
        report
            .tracks
            .iter()
            .filter(|track| track.kind == "system")
            .all(|track| track.rank.is_none())
    );
}

#[test]
fn gpu_sampling_requires_explicit_collection_and_an_available_device() {
    let directory = tempfile::tempdir().unwrap();
    let marker = directory.path().join("ran");
    let output = Command::new(env!("CARGO_BIN_EXE_rprof"))
        .args(["record", "--gpu-interval-ms", "100", "--", "touch"])
        .arg(&marker)
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("--gpu-interval-ms requires --collect gpu")
    );
    let output = gpu_cli(directory.path())
        .env("RPROF_TEST_NVML_EMPTY", "1")
        .args(["--", "touch"])
        .arg(&marker)
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("NVML reports 0 GPUs"));
    assert!(!marker.exists());
}

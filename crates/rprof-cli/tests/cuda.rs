#![cfg(all(target_os = "linux", target_pointer_width = "64"))]
use std::{
    path::{Path, PathBuf},
    process::Command,
};

fn cli(path: &Path, library: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_rprof"));
    command
        .env_remove("CUDA_INJECTION64_PATH")
        .args(["record", "--collect", "cuda", "--cuda-library"])
        .arg(library)
        .arg("-o")
        .arg(path);
    command
}

#[test]
fn no_cuda_is_a_collector_failure_with_the_workload_exit_preserved() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("capture");
    // `true` never loads an injection library: no SDK is required for this path.
    let output = cli(&path, Path::new(env!("CARGO_BIN_EXE_rprof")))
        .args(["--", "true"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("No CUDA/CUPTI initialization"));
    let report = rprof_report::analyze(&path, Default::default()).unwrap();
    assert_eq!(report.shards[0].exit_code, Some(0));
    assert_eq!(report.shards[0].end_reason, "CollectorFailed");
    let cuda = report.cuda.unwrap();
    assert!(cuda.devices.is_empty() && cuda.apis.is_empty());
    assert_eq!(cuda.sources[0].initialized_images, "0");
    assert!(!cuda.sources[0].finalized);
}

#[test]
fn an_existing_injection_tool_is_not_replaced() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("capture");
    let marker = dir.path().join("ran");
    let output = cli(&path, Path::new(env!("CARGO_BIN_EXE_rprof")))
        .env("CUDA_INJECTION64_PATH", "/another/tool.so")
        .args(["--", "touch"])
        .arg(&marker)
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(!path.exists() && !marker.exists());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("CUDA_INJECTION64_PATH is already set")
    );
}

fn target() -> PathBuf {
    PathBuf::from(std::env::var_os("RPROF_TEST_CUDA_DIR").expect("run scripts/test-cuda.sh"))
}

#[test]
#[ignore = "requires CUDA 13/CUPTI and a GPU; run scripts/test-cuda.sh"]
fn multistream_gpu_counts_correlations_overlap_and_timing() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("capture");
    let target = target();
    let output = cli(&path, &target.join("librprof_cuda.so"))
        .arg("--")
        .arg(target.join("cuda-multistream"))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("verified"), "{stdout}");
    let report = rprof_report::analyze(&path, Default::default()).unwrap();
    let cuda = report.cuda.as_ref().unwrap();
    let devices: Vec<_> = cuda.devices.iter().filter(|d| d.kernels != "0").collect();
    assert_eq!(devices.len(), 1);
    let d = devices[0];
    assert_eq!(
        (&d.kernels, &d.copies, &d.memsets),
        (&"6".into(), &"6".into(), &"2".into())
    );
    assert_eq!(d.copy_bytes, "98304");
    assert_eq!(d.memset_bytes, "32768");
    assert!(cuda.runtimes[0].initialized);
    assert!(cuda.runtimes[0].clock_uncertainty.is_none());
    let source = &cuda.sources[0];
    assert!(source.finalized);
    for value in [
        &source.ingress_dropped,
        &source.cupti_dropped,
        &source.source_errors,
        &source.invalid_timing,
        &source.unresolved_correlations,
        &source.ambiguous_correlations,
    ] {
        assert_eq!(value, "0");
    }
    let kernels: Vec<_> = report
        .spans
        .iter()
        .filter(|s| s.attributes.get("cuda.role").is_some_and(|r| r == "kernel"))
        .collect();
    assert_eq!(kernels.len(), 6);
    let time = |s: &rprof_report::ReportSpan| {
        (
            s.start_ns.parse::<u64>().unwrap(),
            s.duration_ns.parse::<u64>().unwrap(),
        )
    };
    assert!(
        kernels.iter().any(|a| kernels.iter().any(|b| {
            let (sa, da) = time(a);
            let (sb, db) = time(b);
            a.track != b.track && sa < sb + db && sb < sa + da
        })),
        "the two streams should overlap on the validation GPU"
    );
    for kernel in &kernels {
        assert_eq!(kernel.name, "timed_add");
        assert!(!kernel.incomplete);
        assert!(
            report
                .dependencies
                .iter()
                .any(|edge| Some(&edge.to) == kernel.correlation.as_ref())
        );
    }
    let elapsed_ms: f64 = stdout
        .split("CUDA-event-elapsed-ms=")
        .nth(1)
        .unwrap()
        .split_whitespace()
        .next()
        .unwrap()
        .parse()
        .unwrap();
    let kernel_window = kernels
        .iter()
        .map(|s| {
            let (start, duration) = time(s);
            start + duration
        })
        .max()
        .unwrap()
        - kernels.iter().map(|s| time(s).0).min().unwrap();
    // Events also encompass copies and launch gaps; allow timestamp interpolation noise.
    assert!(
        kernel_window as f64 / 1e6 <= elapsed_ms * 1.15 + 1.0,
        "{kernel_window} ns versus {elapsed_ms} ms"
    );
    assert!(kernel_window as f64 / 1e6 > elapsed_ms * 0.3);
}

#[test]
#[ignore = "requires CUDA 13/CUPTI and a GPU; run scripts/test-cuda.sh"]
fn subscriber_conflict_and_missing_final_flush_are_visible() {
    let dir = tempfile::tempdir().unwrap();
    let target = target();
    let conflict = dir.path().join("conflict");
    let output = cli(&conflict, &target.join("librprof_cuda.so"))
        .arg("--")
        .arg(target.join("cuda-conflict"))
        .output()
        .unwrap();
    assert!(!output.status.success());
    let report = rprof_report::analyze(&conflict, Default::default()).unwrap();
    assert_eq!(report.shards[0].exit_code, Some(0));
    let cuda = report.cuda.unwrap();
    assert!(
        cuda.runtimes
            .iter()
            .any(|r| r.errors.iter().any(|e| e.contains("subscriber conflict")))
    );
    let abrupt = dir.path().join("abrupt");
    let output = cli(&abrupt, &target.join("librprof_cuda.so"))
        .arg("--")
        .arg(target.join("cuda-abrupt"))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report = rprof_report::analyze(&abrupt, Default::default()).unwrap();
    assert!(!report.cuda.unwrap().sources[0].finalized);
    assert!(
        report
            .summary
            .warnings
            .iter()
            .any(|w| w.contains("final activity flush is unconfirmed"))
    );
}

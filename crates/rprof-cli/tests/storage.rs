#![cfg(target_os = "linux")]
use std::{path::Path, process::Command};

fn cli() -> Command {
    Command::new(env!("CARGO_BIN_EXE_rprof"))
}

#[test]
fn storage_only_sampling_keeps_workload_status_and_reports_scope() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("storage.jsonl");
    let output = cli()
        .args([
            "record",
            "--collect",
            "storage",
            "--storage-interval-ms",
            "100",
            "-o",
        ])
        .arg(&path)
        .args(["--", "sh", "-c", "sleep 0.35; exit 7"])
        .output()
        .unwrap();
    assert_eq!(
        output.status.code(),
        Some(7),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report = rprof_report::analyze(&path, Default::default()).unwrap();
    assert!(report.shards[0].collectors.iter().any(|c| c == "storage"));
    assert!(
        report
            .counters
            .iter()
            .any(|c| c.name == "storage.meta.read_errors")
    );
    for sample in report
        .counters
        .iter()
        .filter(|c| c.name.ends_with("bytes_per_second"))
    {
        assert!(
            sample.attributes["storage.window_ns"]
                .parse::<u64>()
                .unwrap()
                > 0
        );
        assert!(sample.value.parse::<f64>().unwrap().is_finite());
        assert!(sample.value.parse::<f64>().unwrap() >= 0.0);
        assert!(
            sample.attributes["storage.scope"].contains("host")
                || sample.attributes["storage.scope"].contains("client")
        );
    }
}

#[test]
fn io_enables_storage_and_can_explicitly_disable_it() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    assert!(
        Command::new("make")
            .arg("preload")
            .current_dir(&root)
            .status()
            .unwrap()
            .success()
    );
    let dir = tempfile::tempdir().unwrap();
    for disabled in [false, true] {
        let path = dir.path().join(if disabled {
            "posix.jsonl"
        } else {
            "both.jsonl"
        });
        let mut cmd = cli();
        cmd.args(["record", "--collect", "io", "-o"]).arg(&path);
        if disabled {
            cmd.arg("--no-storage");
        }
        let output = cmd.args(["--", "sleep", "0.15"]).output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let report = rprof_report::analyze(&path, Default::default()).unwrap();
        assert_eq!(
            report.shards[0].collectors.iter().any(|c| c == "storage"),
            !disabled
        );
        assert_eq!(
            report
                .counters
                .iter()
                .any(|c| c.name.starts_with("storage.meta.")),
            !disabled
        );
    }
}

#[test]
fn invalid_storage_selection_is_rejected_before_launch() {
    let dir = tempfile::tempdir().unwrap();
    let marker = dir.path().join("ran");
    for args in [
        vec!["--collect", "storage", "--no-storage"],
        vec!["--storage-interval-ms", "200"],
    ] {
        let output = cli()
            .arg("record")
            .args(args)
            .args(["--", "touch"])
            .arg(&marker)
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(!marker.exists());
    }
}

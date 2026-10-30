#![cfg(all(target_os = "linux", target_pointer_width = "64"))]
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};
fn cli(path: &Path) -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_rprof"));
    c.args([
        "record",
        "--collect",
        "pmu",
        "--pmu-events",
        "task-clock,minor-faults",
        "--pmu-interval-ms",
        "10",
        "-o",
    ])
    .arg(path);
    c
}
fn compile(dir: &Path, name: &str) -> PathBuf {
    let binary = dir.join(name);
    assert!(
        Command::new("cc")
            .args(["-O2", "-Wall", "-Wextra", "-Werror", "-pthread"])
            .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join(format!("tests/fixtures/{name}.c")))
            .arg("-o")
            .arg(&binary)
            .status()
            .unwrap()
            .success()
    );
    binary
}
#[test]
fn permission_denial_never_becomes_a_measured_zero_or_changes_the_workload_exit() {
    let dir = tempfile::tempdir().unwrap();
    let deny = compile(dir.path(), "deny_perf");
    let path = dir.path().join("denied");
    let output = Command::new(deny)
        .arg(env!("CARGO_BIN_EXE_rprof"))
        .args([
            "record",
            "--collect",
            "pmu",
            "--pmu-events",
            "task-clock",
            "-o",
        ])
        .arg(&path)
        .args(["--", "true"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("permission_denied"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report = rprof_report::analyze(&path, Default::default()).unwrap();
    assert_eq!(report.shards[0].exit_code, Some(0));
    assert_eq!(report.shards[0].end_reason, "CollectorFailed");
    let event = &report.pmu.as_ref().unwrap().events[0];
    assert_eq!(event.status, "permission_denied");
    assert!(event.raw.is_none() && event.scaled_estimate.is_none());
    assert!(!event.final_read);
}
#[test]
fn invalid_event_configuration_precedes_launch_and_capture_creation() {
    let dir = tempfile::tempdir().unwrap();
    for events in [
        "cycles:k",
        "rxyz",
        "task-clock,task-clock",
        "r00c0,rc0",
        "cpu-cycles,cycles",
        "branch-instructions,branches",
    ] {
        let path = dir.path().join("capture");
        let marker = dir.path().join("ran");
        let output = Command::new(env!("CARGO_BIN_EXE_rprof"))
            .args(["record", "--collect", "pmu", "--pmu-events", events, "-o"])
            .arg(&path)
            .args(["--", "touch"])
            .arg(&marker)
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(!path.exists() && !marker.exists());
    }
}
#[test]
fn short_execs_and_failed_execs_preserve_lifecycle() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("short");
    let output = cli(&path)
        .args(["--", "sh", "-c", "exit 23"])
        .output()
        .unwrap();
    let report = rprof_report::analyze(&path, Default::default()).unwrap();
    assert_eq!(report.shards[0].exit_code, Some(23));
    let pmu = report.pmu.unwrap();
    if pmu.events.iter().any(|e| e.status == "permission_denied") {
        eprintln!("perf unavailable on this test host; denial covered explicitly");
    } else {
        assert_eq!(output.status.code(), Some(23));
        assert!(pmu.events.iter().all(|e| e.final_read));
        assert!(
            pmu.events
                .iter()
                .any(|e| e.raw.as_deref().is_some_and(|v| v != "0"))
        );
    }
    let path = dir.path().join("failed");
    assert!(
        !cli(&path)
            .args(["--", "/nonexistent/rprof-pmu-fixture"])
            .status()
            .unwrap()
            .success()
    );
    let report = rprof_report::analyze(&path, Default::default()).unwrap();
    assert_eq!(report.shards[0].end_reason, "LaunchFailed");
}
#[test]
fn inherited_counts_include_short_threads_and_forks_without_supervisor_counts() {
    let dir = tempfile::tempdir().unwrap();
    let fixture = compile(dir.path(), "pmu_workload");
    let mut counts = Vec::new();
    for (index, mode, scope) in [
        (0, "threads", "workload-tree"),
        (1, "threads", "root-thread"),
        (2, "fork", "workload-tree"),
        (3, "fork", "root-thread"),
    ] {
        let path = dir.path().join(format!("capture-{index}"));
        let output = cli(&path)
            .args(["--pmu-scope", scope, "--"])
            .arg(&fixture)
            .arg(mode)
            .output()
            .unwrap();
        let report = rprof_report::analyze(&path, Default::default()).unwrap();
        let pmu = report.pmu.unwrap();
        if pmu.events.iter().any(|e| e.status == "permission_denied") {
            eprintln!(
                "software perf permission unavailable; inheritance requires the opt-in target check"
            );
            return;
        }
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let faults = pmu
            .events
            .iter()
            .find(|e| e.name == "minor-faults")
            .unwrap();
        assert_eq!(faults.scope, scope);
        assert!(faults.final_read);
        assert_eq!(faults.status, "measured");
        assert!(faults.samples.parse::<u64>().unwrap() > 2);
        counts.push(faults.raw.as_ref().unwrap().parse::<u64>().unwrap());
        assert!(pmu.hosts[0].metadata.contains_key("kernel"));
        assert!(
            report
                .counters
                .iter()
                .any(|c| c.name == "pmu.task-clock.raw")
        );
    }
    assert!(counts[0] >= 8192 && counts[2] >= 4096, "{counts:?}");
    assert!(
        counts[1] < 1024 && counts[3] < 1024,
        "root-thread must exclude descendants: {counts:?}"
    );
    assert!(
        counts[0] > counts[2] + 3000,
        "two workers should contribute two fault sets: {counts:?}"
    );
    // Ensure the source captures are complete and all values were cumulative snapshots.
    assert!(
        fs::read_to_string(dir.path().join("capture-0"))
            .unwrap()
            .contains("\"pmu.final\"")
    );
}

#[test]
fn interruption_finalizes_counter_reads_after_workload_cleanup() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("interrupted");
    let mut child = cli(&path)
        .args(["--", "sh", "-c", "while :; do :; done"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    std::thread::sleep(std::time::Duration::from_millis(150));
    assert_eq!(unsafe { libc::kill(child.id() as i32, libc::SIGTERM) }, 0);
    let status = child.wait().unwrap();
    let report = rprof_report::analyze(&path, Default::default()).unwrap();
    assert_eq!(report.shards[0].signal, Some(libc::SIGTERM));
    let pmu = report.pmu.unwrap();
    if pmu.events.iter().all(|e| e.status == "measured") {
        assert_eq!(status.code(), Some(143));
        assert!(pmu.events.iter().all(|e| e.final_read));
    } else {
        assert!(pmu.events.iter().any(|e| e.status == "permission_denied"));
    }
}

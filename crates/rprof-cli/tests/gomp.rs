#![cfg(all(target_os = "linux", target_arch = "x86_64"))]
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
    sync::OnceLock,
};
fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}
fn compiler() -> String {
    std::env::var("RPROF_TEST_GOMP_CC").unwrap_or_else(|_| "gcc".into())
}
fn available() -> bool {
    let result = Command::new(compiler())
        .arg("-dumpfullversion")
        .output()
        .unwrap();
    if result.status.success() && String::from_utf8_lossy(&result.stdout).starts_with("13.") {
        true
    } else {
        eprintln!("GCC 13 target required; GNU adapter test skipped on this compiler");
        false
    }
}
fn library() -> &'static PathBuf {
    static PATH: OnceLock<PathBuf> = OnceLock::new();
    PATH.get_or_init(|| {
        if let Some(path) = std::env::var_os("RPROF_TEST_GOMP_LIBRARY") {
            return fs::canonicalize(path).unwrap();
        }
        assert!(
            Command::new("make")
                .arg("gomp")
                .arg(format!("GOMP_CC={}", compiler()))
                .current_dir(root())
                .status()
                .unwrap()
                .success()
        );
        root()
            .join("target/debug/librprof_gomp.so")
            .canonicalize()
            .unwrap()
    })
}
fn cli(path: &Path, detail: &str) -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_rprof"));
    c.env_remove("LD_PRELOAD")
        .env_remove("OMP_TOOL_LIBRARIES")
        .env("OMP_CANCELLATION", "false")
        .env("OMP_MAX_ACTIVE_LEVELS", "2")
        .args([
            "record",
            "--collect",
            "openmp",
            "--openmp-backend",
            "gomp",
            "--gomp-detail",
            detail,
            "--openmp-library",
        ])
        .arg(library())
        .arg("-o")
        .arg(path);
    c
}
fn compile(dir: &Path, name: &str, cpp: bool) -> PathBuf {
    let exe = dir.join(name);
    let cc = if cpp {
        std::env::var("RPROF_TEST_GOMP_CXX").unwrap_or_else(|_| "g++".into())
    } else {
        compiler()
    };
    assert!(
        Command::new(cc)
            .args(["-O1", "-g", "-fopenmp"])
            .arg(root().join(format!(
                "crates/rprof-gomp/tests/fixtures/{name}.{}",
                if cpp { "cpp" } else { "c" }
            )))
            .arg("-o")
            .arg(&exe)
            .status()
            .unwrap()
            .success()
    );
    exe
}
#[test]
fn gcc_parallel_work_tasks_and_api_scopes_have_honest_backend_coverage() {
    if !available() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let exe = compile(dir.path(), "workload", false);
    let baseline = Command::new(&exe)
        .env("OMP_MAX_ACTIVE_LEVELS", "2")
        .output()
        .unwrap();
    assert!(baseline.status.success());
    for level in ["regions", "bodies", "full"] {
        let path = dir.path().join(level);
        let output = cli(&path, level).arg("--").arg(&exe).output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(output.stdout, baseline.stdout);
        let report = rprof_report::analyze(&path, Default::default()).unwrap();
        assert_eq!(report.summary.dropped, "0");
        let omp = report.openmp.unwrap();
        let runtime = &omp.runtimes[0];
        assert_eq!(runtime.backend, "gomp");
        assert!(runtime.initialized && runtime.finalized);
        assert_eq!(runtime.sequence_gaps, "0");
        assert_eq!(runtime.unmatched_endpoints, "0");
        assert_eq!(runtime.abandoned_intervals, "0");
        assert!(runtime.version.contains("SHA256"));
        assert!(runtime.version.contains(&format!("detail={level}")));
        assert_eq!(omp.regions.len(), 32);
        assert!(omp.regions.iter().any(|r| r.observed_threads == "4"));
        assert!(report.spans.iter().all(|s| !s.incomplete));
        let gomp = omp.gomp.unwrap();
        assert!(gomp.bodies.iter().any(|b| b.kind == "parallel_body"));
        assert_eq!(
            gomp.task_creations,
            if level == "regions" { "0" } else { "57" }
        );
        assert_eq!(gomp.task_body_spans, gomp.task_creations);
        assert_eq!(gomp.task_lifetimes_to_body_return, gomp.task_creations);
        assert_eq!(
            omp.execution_segments, "0",
            "GNU body observations must not become OMPT scheduled execution"
        );
        if level == "full" {
            assert!(gomp.apis.iter().any(|a| a.name == "GOMP_barrier"));
            assert!(omp.threads.iter().any(|t| t.work_ns != "0"));
        } else {
            assert!(gomp.apis.is_empty());
            assert!(omp.threads.iter().all(|t| t.work_ns == "0"));
        }
        assert!(omp.threads.iter().all(|t| t.wait_ns == "0"));
    }
}

#[test]
fn fork_errno_and_cancellation_selection_preserve_runtime_behavior() {
    if !available() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let exe = compile(dir.path(), "lifecycle", false);
    for after in [false, true] {
        let args: Vec<&str> = if after { vec!["after"] } else { vec![] };
        let baseline = Command::new(&exe).args(&args).output().unwrap();
        assert!(baseline.status.success());
        let path = dir.path().join(if after { "after" } else { "before" });
        let output = cli(&path, "full")
            .arg("--")
            .arg(&exe)
            .args(&args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(output.stdout, baseline.stdout);
        let omp = rprof_report::analyze(&path, Default::default())
            .unwrap()
            .openmp
            .unwrap();
        assert_eq!(omp.runtimes.len(), 2);
        assert_eq!(
            omp.runtimes.iter().filter(|r| r.initialized).count(),
            if after { 1 } else { 2 }
        );
        assert!(
            omp.runtimes
                .iter()
                .all(|r| r.sequence_gaps == "0" && r.unmatched_endpoints == "0")
        );
        if after {
            assert!(
                omp.runtimes
                    .iter()
                    .any(|r| r.version.contains("fork after observed"))
            );
        }
    }
    let exe = compile(dir.path(), "firstprivate", true);
    let path = dir.path().join("cancellation");
    let baseline = Command::new(&exe)
        .env("OMP_CANCELLATION", "true")
        .output()
        .unwrap();
    let output = cli(&path, "full")
        .env("OMP_CANCELLATION", "true")
        .arg("--")
        .arg(exe)
        .output()
        .unwrap();
    assert!(baseline.status.success() && output.status.success());
    assert_eq!(output.stdout, baseline.stdout);
    let omp = rprof_report::analyze(&path, Default::default())
        .unwrap()
        .openmp
        .unwrap();
    assert_eq!(omp.gomp.unwrap().task_creations, "0");
    assert!(
        omp.runtimes[0]
            .callbacks
            .iter()
            .any(|c| c.name == "gomp.task_body" && c.detail.contains("cancellation"))
    );
}
#[test]
fn aligned_cpp_firstprivate_and_unsupported_task_forms_keep_application_results() {
    if !available() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    for (name, cpp) in [("firstprivate", true), ("passthrough", false)] {
        let exe = compile(dir.path(), name, cpp);
        let baseline = Command::new(&exe).output().unwrap();
        assert!(baseline.status.success());
        let path = dir.path().join(format!("{name}-capture"));
        let output = cli(&path, "full").arg("--").arg(&exe).output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(output.stdout, baseline.stdout);
        let report = rprof_report::analyze(&path, Default::default()).unwrap();
        let omp = report.openmp.unwrap();
        assert_eq!(omp.runtimes[0].abandoned_intervals, "0");
        let gomp = omp.gomp.unwrap();
        if cpp {
            assert_eq!(gomp.task_creations, "48");
            assert_eq!(gomp.task_body_spans, "48");
        } else {
            assert_eq!(gomp.task_creations, "0");
            assert!(gomp.apis.iter().any(|a| a.name == "GOMP_taskloop"));
        }
    }
}
#[test]
fn absent_runtime_and_target_fingerprint_mismatch_preserve_workload_status() {
    if !available() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("empty");
    let output = cli(&path, "regions").args(["--", "true"]).output().unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("No GNU/libgomp initialization"));
    assert_eq!(
        rprof_report::analyze(&path, Default::default())
            .unwrap()
            .shards[0]
            .exit_code,
        Some(0)
    );
    // Keep the installed adapter unchanged; a separate deliberately mismatched
    // build exercises the transparent tail-forwarding path.
    let wrong = dir.path().join("wrong.so");
    let output = Command::new(compiler())
        .current_dir(root())
        .args([
            "-O2",
            "-fPIC",
            "-shared",
            "-fcf-protection=branch",
            "-DRPROF_GOMP_BUILD_VERSION=\"13.test\"",
        ])
        .arg(format!("-DRPROF_GOMP_BUILD_SHA=\"{}\"", "0".repeat(64)))
        .args([
            "crates/rprof-gomp/native/adapter.c",
            "crates/rprof-gomp/native/dispatch.S",
            "target/debug/librprof_ompt.a",
            "-Wl,--exclude-libs,ALL",
            "-ldl",
            "-lpthread",
            "-lm",
            "-lrt",
            "-lutil",
            "-o",
        ])
        .arg(&wrong)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let exe = compile(dir.path(), "firstprivate", true);
    let baseline = Command::new(&exe).output().unwrap();
    let path = dir.path().join("mismatch");
    let output = Command::new(env!("CARGO_BIN_EXE_rprof"))
        .env_remove("LD_PRELOAD")
        .env_remove("OMP_TOOL_LIBRARIES")
        .args([
            "record",
            "--collect",
            "openmp",
            "--openmp-backend",
            "gomp",
            "--openmp-library",
        ])
        .arg(wrong)
        .arg("-o")
        .arg(&path)
        .arg("--")
        .arg(exe)
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert_eq!(output.stdout, baseline.stdout);
    let report = rprof_report::analyze(&path, Default::default()).unwrap();
    assert_eq!(report.shards[0].exit_code, Some(0));
    assert!(
        report
            .openmp
            .unwrap()
            .runtimes
            .iter()
            .any(|r| !r.initialized && r.version.contains("fingerprint mismatch"))
    );
}

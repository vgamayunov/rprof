#![cfg(all(target_os = "linux", target_pointer_width = "64"))]
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
    sync::OnceLock,
};

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}
fn tool() -> &'static PathBuf {
    static PATH: OnceLock<PathBuf> = OnceLock::new();
    PATH.get_or_init(|| {
        if let Some(path) = std::env::var_os("RPROF_TEST_OMPT_LIBRARY") {
            return fs::canonicalize(path).unwrap();
        }
        let output = Command::new("make")
            .arg("ompt")
            .current_dir(root())
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        fs::canonicalize(root().join("target/debug/librprof_ompt.so")).unwrap()
    })
}
fn cli(output: &Path) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_rprof"));
    cmd.env_remove("OMP_TOOL_LIBRARIES")
        .env_remove("OMP_TOOL")
        .env_remove("LD_PRELOAD")
        .args(["record", "--collect", "openmp", "--openmp-library"])
        .arg(tool())
        .arg("-o")
        .arg(output);
    cmd
}
fn compile(directory: &Path, fixture: &str, gcc: bool) -> PathBuf {
    let binary = directory.join("workload");
    let compiler = if gcc {
        "gcc".into()
    } else {
        std::env::var_os("RPROF_TEST_OMP_CC").unwrap_or_else(|| "clang".into())
    };
    let mut command = Command::new(compiler);
    command.args(["-O0", "-g"]);
    if !gcc {
        if let Some(paths) = std::env::var_os("RPROF_TEST_OMP_COMPILER_LD") {
            command.env("LD_LIBRARY_PATH", paths);
        }
        if let Some(include) = std::env::var_os("RPROF_TEST_OMP_INCLUDE") {
            command.arg("-I").arg(include);
        }
        if let Some(lib) = std::env::var_os("RPROF_TEST_OMP_LIBDIR") {
            command.arg("-L").arg(&lib);
            command.arg(format!("-Wl,-rpath,{}", Path::new(&lib).display()));
        }
    }
    command.arg(
        if !gcc && std::env::var("RPROF_TEST_OMP_RUNTIME").as_deref() == Ok("intel") {
            "-fopenmp=libiomp5"
        } else {
            "-fopenmp"
        },
    );
    let output = command
        .arg(root().join(format!("crates/rprof-ompt/tests/fixtures/{fixture}.c")))
        .arg("-o")
        .arg(&binary)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "OpenMP compiler failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    binary
}
#[test]
fn no_runtime_is_a_collection_failure_not_a_successful_empty_profile() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("capture");
    let output = cli(&path).args(["--", "true"]).output().unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("No OMPT initialization observed"));
    let report = rprof_report::analyze(&path, Default::default()).unwrap();
    assert_eq!(report.shards[0].exit_code, Some(0));
    assert_eq!(report.shards[0].end_reason, "CollectorFailed");
    assert!(!report.openmp.unwrap().runtimes[0].initialized);
}
#[test]
fn native_gcc_runtime_is_probed_without_being_replaced() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("gcc-capture");
    let binary = compile(dir.path(), "omp_host", true);
    let output = cli(&path).arg("--").arg(binary).output().unwrap();
    assert!(String::from_utf8_lossy(&output.stdout).contains("omp_host ok"));
    let report = rprof_report::analyze(&path, Default::default()).unwrap();
    let omp = report.openmp.unwrap();
    if output.status.success() {
        assert!(omp.runtimes.iter().any(|r| r.initialized));
    } else {
        assert!(omp.runtimes.iter().all(|r| !r.initialized));
        assert_eq!(report.shards[0].exit_code, Some(0));
    }
}
#[test]
fn another_ompt_tool_is_not_silently_displaced() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("capture");
    let marker = dir.path().join("ran");
    let output = cli(&path)
        .env("OMP_TOOL_LIBRARIES", "/example/another-tool.so")
        .args(["--", "touch"])
        .arg(&marker)
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(!path.exists() && !marker.exists());
    assert!(String::from_utf8_lossy(&output.stderr).contains("OMP_TOOL_LIBRARIES is already set"));
}
#[test]
#[ignore = "requires Clang and an OMPT runtime; run scripts/test-openmp.sh"]
fn runtime_collects_nested_work_tasks_waits_and_capabilities() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("capture");
    let binary = compile(dir.path(), "omp_host", false);
    let output = cli(&path).arg("--").arg(binary).output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("omp_host ok"));
    let report = rprof_report::analyze(&path, Default::default()).unwrap();
    assert_eq!(report.summary.dropped, "0");
    let omp = report.openmp.as_ref().unwrap();
    assert_eq!(omp.runtimes.len(), 1);
    let runtime = &omp.runtimes[0];
    assert!(runtime.initialized && runtime.finalized);
    assert_eq!(runtime.sequence_gaps, "0");
    assert_eq!(runtime.unmatched_endpoints, "0");
    assert_eq!(runtime.abandoned_intervals, "0");
    assert_eq!(omp.tasks_created, "24");
    assert_eq!(omp.tasks_completed, "24");
    assert_eq!(omp.regions.len(), 2);
    assert!(omp.regions.iter().any(|r| r.observed_threads == "4"));
    assert!(omp.regions.iter().any(|r| r.observed_threads == "2"));
    assert!(
        omp.threads
            .iter()
            .any(|t| t.wait_ns.parse::<u128>().unwrap() > 0)
    );
    assert!(report.spans.iter().all(|s| !s.incomplete));
    assert!(runtime.callbacks.iter().any(|c| {
        c.name == "ompt_callback_work"
            && c.observed
                .as_deref()
                .is_some_and(|v| v.parse::<u64>().unwrap() > 0)
    }));
    let text = dir.path().join("report.txt");
    let html = dir.path().join("report.html");
    for (format, path) in [("text", &text), ("html", &html)] {
        assert!(
            Command::new(env!("CARGO_BIN_EXE_rprof"))
                .arg("report")
                .arg(path.parent().unwrap().join("capture"))
                .args(["--format", format, "-o"])
                .arg(path)
                .status()
                .unwrap()
                .success()
        );
    }
    assert!(
        fs::read_to_string(text)
            .unwrap()
            .contains("OpenMP runtime capabilities")
    );
    assert!(
        fs::read_to_string(html)
            .unwrap()
            .contains("ompt_callback_work")
    );
}
#[test]
#[ignore = "requires Clang and an OMPT runtime; run scripts/test-openmp.sh"]
fn openmp_and_io_use_the_same_physical_thread_tracks() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("capture");
    let binary = compile(dir.path(), "omp_io", false);
    assert!(
        Command::new("make")
            .arg("preload")
            .current_dir(root())
            .status()
            .unwrap()
            .success()
    );
    let data = dir.path().join("data");
    fs::create_dir(&data).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_rprof"))
        .env_remove("OMP_TOOL_LIBRARIES")
        .env_remove("LD_PRELOAD")
        .args(["record", "--collect", "openmp,io", "--openmp-library"])
        .arg(tool())
        .args(["--io-library"])
        .arg(root().join("target/debug/librprof_io.so"))
        .arg("-o")
        .arg(&path)
        .arg("--")
        .arg(binary)
        .arg(&data)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report = rprof_report::analyze(&path, Default::default()).unwrap();
    assert_eq!(report.summary.dropped, "0");
    for span in report.spans.iter().filter(|s| {
        s.category == "io"
            && s.attributes
                .get("io.file")
                .is_some_and(|p| p.starts_with(data.to_str().unwrap()))
    }) {
        let file = report.tracks.iter().find(|t| t.id == span.track).unwrap();
        let thread = file.parent.as_ref().unwrap();
        assert!(report.spans.iter().any(|s| {
            s.track == *thread
                && s.attributes
                    .get("openmp.role")
                    .is_some_and(|r| r == "task_execution")
        }));
    }
    assert_eq!(
        report
            .io
            .iter()
            .filter(|f| f.file.starts_with(data.to_str().unwrap()))
            .count(),
        4
    );
}

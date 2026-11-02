#![cfg(all(target_os = "linux", target_pointer_width = "64"))]
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};
fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}
fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/python_workload.py")
}
fn python() -> std::ffi::OsString {
    std::env::var_os("RPROF_TEST_PYTHON").unwrap_or_else(|| "python3".into())
}
fn cli(path: &Path, backend: &str) -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_rprof"));
    c.env_remove("RPROF_PYTHON_SOCKET")
        .args([
            "record",
            "--collect",
            "python",
            "--python-backend",
            backend,
            "--python-include",
        ])
        .arg(fixture().parent().unwrap())
        .arg("-o")
        .arg(path);
    c
}
#[test]
fn python_segments_preserve_suspension_threads_ranges_and_results() {
    let dir = tempfile::tempdir().unwrap();
    for backend in ["auto", "profile"] {
        let path = dir.path().join(backend);
        let output = cli(&path, backend)
            .arg("--")
            .arg(python())
            .arg(fixture())
            .arg("basic")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(String::from_utf8_lossy(&output.stdout).contains("python workload verified"));
        let report = rprof_report::analyze(&path, Default::default()).unwrap();
        let py = report.python.as_ref().unwrap();
        assert_eq!(report.summary.dropped, "0");
        assert_eq!(py.runtimes.len(), 1);
        let source = &py.runtimes[0];
        assert!(source.initialized && source.finalized);
        assert_eq!(source.source_errors, "0");
        assert_eq!(source.unmatched, "0");
        let generator = py
            .functions
            .iter()
            .find(|f| f.name == "suspended_generator")
            .unwrap();
        assert_eq!(generator.segments, "3");
        assert_eq!(generator.yields, "2");
        let coro = py
            .functions
            .iter()
            .find(|f| f.name == "suspended_coroutine")
            .unwrap();
        assert_eq!(coro.segments, "2");
        assert_eq!(coro.yields, "1");
        for (function, range) in [
            (generator, "generator elapsed"),
            (coro, "coroutine elapsed"),
        ] {
            let elapsed = py.functions.iter().find(|f| f.name == range).unwrap();
            assert!(elapsed.self_ns.is_none());
            assert!(
                elapsed.total_ns.parse::<u64>().unwrap()
                    > function.total_ns.parse::<u64>().unwrap() + 20_000_000
            );
        }
        let threads: std::collections::BTreeSet<_> = report
            .spans
            .iter()
            .filter(|s| s.name == "thread_worker")
            .map(|s| s.track.clone())
            .collect();
        assert_eq!(threads.len(), 2);
        assert!(report.spans.iter().all(|s| !s.incomplete));
    }
}
#[test]
fn multiprocessing_methods_have_distinct_images_and_normal_worker_completion() {
    let dir = tempfile::tempdir().unwrap();
    for method in ["fork", "spawn", "forkserver"] {
        let path = dir.path().join(method);
        let data = dir.path().join(format!("data-{method}"));
        let output = cli(&path, "auto")
            .arg("--")
            .arg(python())
            .arg(fixture())
            .arg(method)
            .arg(&data)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let report = rprof_report::analyze(&path, Default::default()).unwrap();
        let py = report.python.unwrap();
        let workers: Vec<_> = py
            .functions
            .iter()
            .filter(|f| f.name == "process_worker")
            .collect();
        assert_eq!(workers.len(), 2, "{method}");
        assert_ne!(workers[0].source, workers[1].source);
        for worker in workers {
            let runtime = py
                .runtimes
                .iter()
                .find(|r| r.source == worker.source)
                .unwrap();
            assert!(
                runtime.initialized && runtime.finalized,
                "{method}: {}",
                runtime.source
            );
            assert_eq!(runtime.source_errors, "0");
        }
    }
}
#[test]
fn python_and_io_share_native_process_thread_identity() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("capture");
    let data = dir.path().join("data");
    assert!(
        Command::new("make")
            .arg("preload")
            .current_dir(root())
            .status()
            .unwrap()
            .success()
    );
    let output = Command::new(env!("CARGO_BIN_EXE_rprof"))
        .env_remove("RPROF_PYTHON_SOCKET")
        .env_remove("LD_PRELOAD")
        .args(["record", "--collect", "python,io", "--io-library"])
        .arg(root().join("target/debug/librprof_io.so"))
        .arg("--python-include")
        .arg(fixture().parent().unwrap())
        .arg("-o")
        .arg(&path)
        .arg("--")
        .arg(python())
        .arg(fixture())
        .arg("io")
        .arg(&data)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report = rprof_report::analyze(&path, Default::default()).unwrap();
    let function = report.spans.iter().find(|s| s.name == "io_region").unwrap();
    let io: Vec<_> = report
        .spans
        .iter()
        .filter(|s| {
            s.attributes
                .get("io.file")
                .is_some_and(|p| p == data.to_str().unwrap())
        })
        .collect();
    assert!(!io.is_empty());
    for span in io {
        let track = report.tracks.iter().find(|t| t.id == span.track).unwrap();
        assert_eq!(track.parent.as_ref(), Some(&function.track));
    }
}
#[test]
fn existing_profile_hook_and_sitecustomize_are_preserved() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("capture");
    fs::write(
        dir.path().join("sitecustomize.py"),
        "import sys\ndef original_profile(*args): pass\nsys.setprofile(original_profile)\n",
    )
    .unwrap();
    let output=cli(&path,"profile").env("PYTHONPATH",dir.path()).arg("--").arg(python()).args(["-c","import sys; assert sys.getprofile().__name__ == 'original_profile'; print('owner preserved')"]).output().unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("owner preserved"));
    let report = rprof_report::analyze(&path, Default::default()).unwrap();
    assert_eq!(report.shards[0].exit_code, Some(0));
    assert!(
        report.python.unwrap().runtimes[0]
            .errors
            .iter()
            .any(|e| e.contains("existing Python profile hook"))
    );
}
#[test]
fn absent_initialization_exit_codes_and_bounded_recursion_are_explicit() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("absent");
    assert!(
        !cli(&path, "auto")
            .args(["--", "true"])
            .status()
            .unwrap()
            .success()
    );
    let report = rprof_report::analyze(&path, Default::default()).unwrap();
    assert_eq!(report.shards[0].exit_code, Some(0));
    assert!(report.python.unwrap().runtimes.is_empty());
    let path = dir.path().join("exit");
    let output = cli(&path, "auto")
        .arg("--")
        .arg(python())
        .arg(fixture())
        .arg("exit")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(23));
    let path = dir.path().join("overflow");
    let output = cli(&path, "auto")
        .arg("--")
        .arg(python())
        .arg(fixture())
        .arg("overflow")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report = rprof_report::analyze(&path, Default::default()).unwrap();
    assert!(report.summary.dropped.parse::<u64>().unwrap() > 0);
    assert!(
        report.python.unwrap().runtimes[0]
            .omitted_callbacks
            .parse::<u64>()
            .unwrap()
            > 0
    );
}

#[test]
fn ranges_only_mode_emits_annotations_without_function_hooks() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("ranges");
    let output = cli(&path, "ranges")
        .arg("--")
        .arg(python())
        .arg(fixture())
        .arg("basic")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report = rprof_report::analyze(&path, Default::default()).unwrap();
    let py = report.python.unwrap();
    assert_eq!(py.runtimes[0].backend, "ranges");
    assert!(py.functions.iter().all(|f| f.kind == "range"));
    assert!(py.functions.iter().any(|f| f.name == "generator elapsed"));
}

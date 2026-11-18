#![cfg(all(target_os = "linux", target_arch = "x86_64"))]
use rprof_report::{
    Limits, analyze,
    indexed::CaptureIndex,
    stacks::{Group, Metric, Orientation, Query},
};
use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    process::{Command, Output},
};

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}
fn source() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/stack_workload.c")
}
fn compile(output: &Path, include: Option<&str>) {
    let mut command = Command::new("cc");
    command.args([
        "-O2",
        "-g",
        "-fno-omit-frame-pointer",
        "-fno-optimize-sibling-calls",
        "-pthread",
        "-Wall",
        "-Wextra",
        "-Werror",
    ]);
    if let Some(include) = include {
        command
            .args(["-fPIC", "-shared", "-DRPROF_PYTHON"])
            .arg(format!("-I{include}"));
    }
    let result = command
        .arg(source())
        .arg("-o")
        .arg(output)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
}
fn require_capture(output: &Output) -> bool {
    let error = String::from_utf8_lossy(&output.stderr);
    if !output.status.success()
        && std::env::var_os("RPROF_REQUIRE_STACKS").is_none()
        && [
            "Permission denied",
            "Operation not permitted",
            "Function not implemented",
        ]
        .iter()
        .any(|s| error.contains(s))
    {
        eprintln!("CPU stack sampling is unavailable on this host: {error}");
        return false;
    }
    assert!(output.status.success(), "{error}");
    true
}
fn command(path: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_rprof"));
    command
        .env_remove("LD_PRELOAD")
        .env_remove("RPROF_PYTHON_SOCKET")
        .env("PYTHONDONTWRITEBYTECODE", "1")
        .args([
            "record",
            "--collect",
            "stacks,io",
            "--no-storage",
            "--io-library",
        ])
        .arg(root().join("target/debug/librprof_io.so"))
        .arg("-o")
        .arg(path);
    command
}
fn library() {
    let output = Command::new("make")
        .arg("preload")
        .current_dir(root())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}
fn query(report: &rprof_report::Report, metric: Metric, orientation: Orientation) -> Query {
    Query {
        snapshot: report
            .viewer
            .as_ref()
            .map(|v| v.snapshot.clone())
            .unwrap_or_else(|| "offline".into()),
        domain: report.domains[0].id.clone(),
        start_ns: "0".into(),
        end_ns: (report.domains[0].end_ns.parse::<u128>().unwrap() + 1).to_string(),
        metric,
        orientation,
        group: Group::Combined,
        filter: String::new(),
    }
}

#[test]
fn native_samples_and_io_call_sites_preserve_threads_forks_and_full_scope_totals() {
    library();
    let directory = tempfile::tempdir().unwrap();
    let executable = directory.path().join("workload");
    compile(&executable, None);
    let path = directory.path().join("capture.jsonl");
    let output = command(&path).arg("--").arg(&executable).output().unwrap();
    if !require_capture(&output) {
        return;
    }
    let report = analyze(
        &path,
        Limits {
            max_events: 1,
            ..Default::default()
        },
    )
    .unwrap();
    let stacks = report.stacks.as_ref().unwrap();
    assert!(stacks.complete, "{:?}", stacks.warnings);
    let samples: Vec<_> = stacks
        .facts
        .iter()
        .filter(|f| f.metric == Metric::Cpu)
        .collect();
    assert!(samples.len() >= 3, "only {} CPU samples", samples.len());
    assert!(
        samples
            .iter()
            .map(|s| &s.track)
            .collect::<HashSet<_>>()
            .len()
            >= 3
    );
    assert!(samples.iter().any(|s| {
        s.stack
            .as_ref()
            .and_then(|id| stacks.traces.get(id))
            .is_some_and(|t| t.frames.iter().any(|f| f.name == "stack_spin"))
    }));
    assert!(
        stacks
            .facts
            .iter()
            .filter(|f| f.metric == Metric::IoTime && f.name == "write")
            .any(|s| s
                .stack
                .as_ref()
                .and_then(|id| stacks.traces.get(id))
                .is_some_and(|t| t.frames.iter().any(|f| f.name == "stack_spin")))
    );
    let index = CaptureIndex::open(&path, &directory.path().join("index"), false).unwrap();
    let indexed = index.report().unwrap();
    for metric in [Metric::Cpu, Metric::IoTime, Metric::IoBytes] {
        let offline = rprof_report::stacks::aggregate(
            stacks,
            &report.tracks,
            &query(&report, metric, Orientation::TopDown),
        )
        .unwrap();
        let down = index
            .stacks(query(&indexed, metric, Orientation::TopDown))
            .unwrap();
        let up = index
            .stacks(query(&indexed, metric, Orientation::BottomUp))
            .unwrap();
        assert_eq!(offline.total, down.total);
        assert_eq!(down.total, up.total);
        assert_eq!(offline.observations, down.observations);
    }
}

#[test]
fn mixed_python_native_sampling_and_io_share_real_call_chains_without_function_tracing() {
    let python = std::env::var_os("RPROF_TEST_PYTHON").unwrap_or_else(|| "python3".into());
    let probe = Command::new(&python).args(["-c",
        "import sys,sysconfig; print(sysconfig.get_path('include')); print(int(hasattr(sys,'activate_stack_trampoline') and '-fno-omit-frame-pointer' in (sysconfig.get_config_var('PY_CFLAGS') or '')))"])
        .output().unwrap();
    assert!(probe.status.success());
    let probe = String::from_utf8(probe.stdout).unwrap();
    let mut lines = probe.lines();
    let include = lines.next().unwrap();
    if lines.next() != Some("1") {
        eprintln!(
            "Mixed stack fixture requires a frame-pointer CPython 3.12+ build; set RPROF_TEST_PYTHON"
        );
        return;
    }
    library();
    let directory = tempfile::tempdir().unwrap();
    let extension = directory.path().join("_rprof_stack_workload.so");
    compile(&extension, Some(include));
    let path = directory.path().join("mixed.jsonl");
    let output = Command::new(env!("CARGO_BIN_EXE_rprof"))
        .env_remove("LD_PRELOAD")
        .env_remove("RPROF_PYTHON_SOCKET")
        .env_remove("PYTHONPERFSUPPORT")
        .env_remove("PYTHON_PERF_JIT_SUPPORT")
        .env("PYTHONDONTWRITEBYTECODE", "1")
        .env("PYTHONPATH", directory.path())
        .args([
            "record",
            "--collect",
            "stacks,python,io",
            "--no-storage",
            "--io-library",
        ])
        .arg(root().join("target/debug/librprof_io.so"))
        .arg("-o")
        .arg(&path)
        .arg("--")
        .arg(&python)
        .arg(source().with_extension("py"))
        .output()
        .unwrap();
    if !require_capture(&output) {
        return;
    }
    let report = analyze(&path, Default::default()).unwrap();
    let stacks = report.stacks.as_ref().unwrap();
    let mixed = |stack: &Option<String>| {
        stack
            .as_ref()
            .and_then(|id| stacks.traces.get(id))
            .is_some_and(|trace| {
                trace
                    .frames
                    .iter()
                    .any(|f| f.language == "python" && f.name.contains("python_outer"))
                    && trace
                        .frames
                        .iter()
                        .any(|f| f.language == "native" && f.name == "stack_spin")
            })
    };
    assert!(
        stacks
            .facts
            .iter()
            .filter(|f| f.metric == Metric::Cpu)
            .any(|f| mixed(&f.stack)),
        "no mixed CPU sample; CPU tails: {:?}; symbols: {:?}; Python names: {:?}",
        stacks
            .facts
            .iter()
            .filter(|f| f.metric == Metric::Cpu)
            .filter_map(|f| f.stack.as_ref().and_then(|id| stacks.traces.get(id)))
            .take(12)
            .map(|t| t
                .frames
                .iter()
                .rev()
                .take(7)
                .map(|f| (&f.language, &f.name))
                .collect::<Vec<_>>())
            .collect::<Vec<_>>(),
        report
            .counters
            .iter()
            .filter(|c| c.name.starts_with("stacks.meta."))
            .map(|c| (&c.name, &c.value, c.attributes.get("stacks.symbol_error")))
            .collect::<Vec<_>>(),
        stacks
            .traces
            .values()
            .flat_map(|t| &t.frames)
            .filter(|f| f.language == "python")
            .map(|f| &f.name)
            .take(20)
            .collect::<Vec<_>>()
    );
    assert!(
        stacks
            .facts
            .iter()
            .filter(|f| f.metric == Metric::IoTime && f.name == "write")
            .any(|f| mixed(&f.stack)),
        "no mixed I/O callsite; write paths: {:?}",
        stacks
            .facts
            .iter()
            .filter(|f| f.metric == Metric::IoTime && f.name == "write")
            .take(8)
            .map(|f| f
                .stack
                .as_ref()
                .and_then(|id| stacks.traces.get(id))
                .map(|t| t
                    .frames
                    .iter()
                    .map(|f| (&f.language, &f.name))
                    .take(30)
                    .collect::<Vec<_>>()))
            .collect::<Vec<_>>()
    );
    let python = report.python.unwrap();
    assert!(python.runtimes.iter().all(|r| r.backend == "stacks"));
    assert!(
        python.functions.is_empty(),
        "sampling must not enable Python function tracing"
    );
}

#![cfg(all(target_os = "linux", target_pointer_width = "64"))]

use std::{
    collections::HashSet,
    fs,
    io::BufReader,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Child, Command, Output, Stdio},
    sync::OnceLock,
    time::{Duration, Instant},
};

use rprof_capture::{Reader, Summary};
use rprof_core::{
    Category, CounterValue, Span,
    stream::{Attribute, Payload, Record},
};

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}
fn library() -> &'static PathBuf {
    static LIBRARY: OnceLock<PathBuf> = OnceLock::new();
    LIBRARY.get_or_init(|| {
        if let Some(path) = std::env::var_os("RPROF_TEST_IO_LIBRARY") {
            return fs::canonicalize(path).expect("invalid RPROF_TEST_IO_LIBRARY");
        }
        let result = Command::new("make")
            .arg("preload")
            .current_dir(root())
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "make preload failed: {}",
            String::from_utf8_lossy(&result.stderr)
        );
        fs::canonicalize(root().join("target/debug/librprof_io.so")).unwrap()
    })
}

fn fixture(directory: &Path) -> PathBuf {
    let binary = directory.join("io-workload");
    let output = Command::new("cc")
        .args(["-std=c11", "-O2", "-Wall", "-Wextra", "-Werror", "-pthread"])
        .arg(root().join("crates/rprof-cli/tests/fixtures/io_workload.c"))
        .arg("-o")
        .arg(&binary)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "native fixture compilation: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    binary
}

fn command(capture: &Path) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_rprof"));
    cmd.env_remove("RPROF_SESSION_ID")
        .env_remove("RPROF_SHARD_ID")
        .args(["record", "--collect", "io", "--io-library"])
        .arg(library())
        .arg("-o")
        .arg(capture);
    cmd
}

struct Run {
    _directory: tempfile::TempDir,
    path: PathBuf,
    capture: PathBuf,
    output: Output,
}
fn run(mode: &str) -> Run {
    let directory = tempfile::tempdir().unwrap();
    let binary = fixture(directory.path());
    let path = directory.path().join("data");
    let capture = directory.path().join("capture.jsonl");
    let mut child = command(&capture)
        .arg("--")
        .arg(binary)
        .arg(mode)
        .arg(&path)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(15);
    while child.try_wait().unwrap().is_none() {
        if Instant::now() > deadline {
            let _ = child.kill();
            panic!("native fixture timed out");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "fixture {mode}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    Run {
        _directory: directory,
        path,
        capture,
        output,
    }
}

struct Call {
    span: Span,
    attrs: std::collections::BTreeMap<String, Attribute>,
}
fn read(path: &Path) -> (Vec<Call>, Summary, u64, u64) {
    let mut reader = Reader::new(BufReader::new(fs::File::open(path).unwrap()));
    let mut calls = vec![];
    let mut attempted = 0;
    let mut received = 0;
    while let Some(record) = reader.next_record().unwrap() {
        if let Record::Chunk(chunk) = record {
            for event in chunk.events {
                match event.payload {
                    Payload::Span(span) if matches!(span.category, Category::Io) => {
                        calls.push(Call {
                            span,
                            attrs: event.attributes,
                        })
                    }
                    Payload::Counter(sample) => {
                        if let CounterValue::Integer(value) = sample.value {
                            if sample.name == "io.attempted_events" {
                                attempted = value;
                            }
                            if sample.name == "io.received_events" {
                                received = value;
                            }
                        }
                    }
                    _ => (),
                }
            }
        }
    }
    (calls, reader.summary().unwrap(), attempted, received)
}
fn text(call: &Call, key: &str) -> String {
    if let Some(Attribute::Text(v)) = call.attrs.get(key) {
        v.clone()
    } else {
        panic!("missing text {key}");
    }
}
fn uint(call: &Call, key: &str) -> u64 {
    if let Some(Attribute::Unsigned(v)) = call.attrs.get(key) {
        *v
    } else {
        panic!("missing uint {key}");
    }
}
fn int(call: &Call, key: &str) -> i64 {
    if let Some(Attribute::Signed(v)) = call.attrs.get(key) {
        *v
    } else {
        panic!("missing int {key}");
    }
}

#[test]
fn real_calls_preserve_data_errno_modes_and_report_exact_bytes() {
    let run = run("basic");
    assert_eq!(fs::read(&run.path).unwrap(), b"abXYef");
    assert_eq!(
        fs::metadata(&run.path).unwrap().permissions().mode() & 0o777,
        0o601
    );
    let (calls, summary, attempted, received) = read(&run.capture);
    assert!(summary.is_complete());
    assert_eq!(attempted, 9);
    assert_eq!(received, 9);
    assert_eq!(calls.len(), 9);
    assert_eq!(summary.shards.values().next().unwrap().loss.total(), 0);
    assert!(
        calls
            .iter()
            .all(|c| text(c, "io.file") == run.path.display().to_string())
    );
    let report = rprof_report::analyze(&run.capture, rprof_report::Limits::default()).unwrap();
    let file = report
        .io
        .iter()
        .find(|f| f.file == run.path.display().to_string())
        .unwrap();
    assert_eq!(file.read_bytes, "6");
    assert_eq!(file.requested_read_bytes, "17");
    assert_eq!(file.write_bytes, "8");
    assert_eq!(file.operations, "9");
    let limited = rprof_report::analyze(
        &run.capture,
        rprof_report::Limits {
            max_events: 0,
            ..Default::default()
        },
    )
    .unwrap();
    assert!(limited.spans.is_empty());
    assert_eq!(limited.io[0].read_bytes, "6");
    assert_eq!(limited.io[0].write_bytes, "8");
    assert!(String::from_utf8_lossy(&run.output.stderr).contains("0 ingress drops"));
}

#[test]
fn invalid_pointers_and_failed_calls_do_not_crash_or_change_errno() {
    let run = run("errors");
    let (calls, _, _, _) = read(&run.capture);
    assert!(
        calls
            .iter()
            .any(|c| int(c, "io.errno") == libc::EFAULT as i64)
    );
    assert!(
        calls
            .iter()
            .any(|c| int(c, "io.errno") == libc::EBADF as i64)
    );
    assert!(
        calls
            .iter()
            .any(|c| int(c, "io.errno") == libc::ENOENT as i64)
    );
    assert!(
        calls
            .iter()
            .filter(|c| int(c, "io.result") < 0)
            .all(|c| uint(c, "io.completed_bytes") == 0)
    );
}

#[test]
fn descriptor_reuse_and_duplication_keep_resource_identity() {
    let run = run("reuse");
    assert_eq!(fs::read(&run.path).unwrap(), b"A");
    assert_eq!(
        fs::read(format!("{}.second", run.path.display())).unwrap(),
        b"BCDE"
    );
    let (calls, _, _, _) = read(&run.capture);
    let writes: Vec<_> = calls.iter().filter(|c| c.span.name == "write").collect();
    assert_eq!(writes.len(), 5);
    assert_eq!(int(writes[0], "io.fd"), int(writes[1], "io.fd"));
    assert_ne!(
        uint(writes[0], "io.resource"),
        uint(writes[1], "io.resource")
    );
    assert!(
        writes[1..]
            .iter()
            .all(|c| text(c, "io.file").ends_with(".second"))
    );
    for name in ["dup", "dup2", "dup3"] {
        assert!(calls.iter().any(|c| c.span.name == name));
    }
}

#[test]
fn large_file_aliases_and_fortified_calls_are_interposed() {
    let run = run("aliases");
    let (calls, _, _, _) = read(&run.capture);
    assert!(
        calls
            .iter()
            .any(|c| c.span.name == "pwrite" && int(c, "io.offset") == 3 * 1024 * 1024 * 1024)
    );
    assert!(calls.iter().any(|c| c.span.name == "openat"));
    assert!(calls.iter().any(|c| c.span.name == "creat"));
}

#[test]
fn short_io_reports_actual_not_requested_byte_counts() {
    let run = run("partial");
    let (calls, _, _, _) = read(&run.capture);
    assert!(calls.iter().any(|c| c.span.name == "write"
        && uint(c, "io.requested_bytes") == 8192
        && uint(c, "io.completed_bytes") == 4096));
}

#[test]
fn concurrent_threads_have_distinct_tracks_and_consistent_observation_order() {
    let run = run("threads");
    let (calls, summary, _, _) = read(&run.capture);
    let tids: HashSet<_> = calls
        .iter()
        .filter(|c| c.span.name == "pwrite")
        .map(|c| uint(c, "tid"))
        .collect();
    assert_eq!(tids.len(), 4);
    assert_eq!(calls.iter().filter(|c| c.span.name == "pwrite").count(), 40);
    assert!(summary.is_complete());
    assert_eq!(
        fs::read(&run.path).unwrap(),
        b"aaaaaaaaaabbbbbbbbbbccccccccccdddddddddd"
    );
}

#[test]
fn fork_and_exec_use_distinct_process_image_namespaces() {
    for (mode, expected, same_pid) in [("fork", "childparent", false), ("exec", "parentexec", true)]
    {
        let run = run(mode);
        assert_eq!(fs::read_to_string(&run.path).unwrap(), expected);
        let (calls, summary, _, _) = read(&run.capture);
        let images: HashSet<_> = calls.iter().map(|c| uint(c, "process_instance")).collect();
        let pids: HashSet<_> = calls.iter().map(|c| uint(c, "pid")).collect();
        assert_eq!(images.len(), 2, "{mode}");
        assert_eq!(pids.len(), if same_pid { 1 } else { 2 });
        assert!(summary.is_complete());
    }
}

#[test]
fn pthread_cancellation_unwinds_the_libc_call_without_entering_rust_hooks() {
    let run = run("cancel");
    let (calls, summary, _, _) = read(&run.capture);
    assert!(summary.is_complete());
    assert!(calls.iter().all(|call| call.span.name != "read"));
}

struct Guard(Child);
impl Drop for Guard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn termination_keeps_native_events_and_finalizes_capture() {
    let directory = tempfile::tempdir().unwrap();
    let binary = fixture(directory.path());
    let path = directory.path().join("ready");
    let capture = directory.path().join("capture");
    let mut child = Guard(
        command(&capture)
            .arg("--")
            .arg(binary)
            .arg("signal")
            .arg(&path)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let deadline = Instant::now() + Duration::from_secs(5);
    while fs::read(&path).ok().as_deref() != Some(b"ready") {
        assert!(Instant::now() < deadline, "workload not ready");
        std::thread::sleep(Duration::from_millis(10));
    }
    unsafe {
        libc::kill(child.0.id() as i32, libc::SIGTERM);
    }
    let deadline = Instant::now() + Duration::from_secs(5);
    let status = loop {
        if let Some(status) = child.0.try_wait().unwrap() {
            break status;
        }
        assert!(Instant::now() < deadline, "profiler did not terminate");
        std::thread::sleep(Duration::from_millis(10));
    };
    assert_eq!(status.code(), Some(143));
    let (calls, summary, attempted, received) = read(&capture);
    assert!(summary.is_complete());
    assert!(calls.iter().any(|c| c.span.name == "write"));
    assert_eq!(attempted, received);
}

#[test]
fn overload_has_explicit_ingress_and_writer_loss_accounting() {
    let directory = tempfile::tempdir().unwrap();
    let binary = fixture(directory.path());
    let capture = directory.path().join("capture");
    let result = command(&capture)
        .args([
            "--queue-chunks",
            "1",
            "--chunk-events",
            "1",
            "--io-buffer-events",
            "1",
        ])
        .arg("--")
        .arg(binary)
        .arg("bench")
        .arg(directory.path().join("data"))
        .arg("3000")
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let (calls, summary, attempted, received) = read(&capture);
    let loss = &summary.shards.values().next().unwrap().loss;
    assert_eq!(attempted, 3002);
    assert_eq!(attempted, received + loss.ingress);
    assert!(
        loss.ingress > 0,
        "overload fixture did not exercise ingress loss"
    );
    assert!(calls.len() as u64 <= received);
    assert!(received - calls.len() as u64 <= loss.queue_full);
}

#[test]
fn unavailable_library_is_rejected_before_launch_and_noninitializing_library_is_reported() {
    let directory = tempfile::tempdir().unwrap();
    let capture = directory.path().join("capture");
    let marker = directory.path().join("ran");
    let output = Command::new(env!("CARGO_BIN_EXE_rprof"))
        .args(["record", "--collect", "io", "--io-library"])
        .arg(directory.path().join("missing.so"))
        .arg("-o")
        .arg(&capture)
        .args(["--", "touch"])
        .arg(&marker)
        .output()
        .unwrap();
    assert!(!output.status.success() && !marker.exists() && !capture.exists());
    let source = directory.path().join("empty.c");
    let library = directory.path().join("empty.so");
    fs::write(&source, "int unrelated_library_symbol = 1;\n").unwrap();
    assert!(
        Command::new("cc")
            .args(["-fPIC", "-shared"])
            .arg(source)
            .arg("-o")
            .arg(&library)
            .status()
            .unwrap()
            .success()
    );
    let output = Command::new(env!("CARGO_BIN_EXE_rprof"))
        .args(["record", "--collect", "io", "--io-library"])
        .arg(library)
        .arg("-o")
        .arg(&capture)
        .args(["--", "true"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("did not initialize"));
    let (_, summary, _, _) = read(&capture);
    let end = summary
        .shards
        .values()
        .next()
        .unwrap()
        .end
        .as_ref()
        .unwrap();
    assert_eq!(end.exit_code, Some(0));
    assert_eq!(end.reason, rprof_core::stream::EndReason::CollectorFailed);
}

#[test]
fn long_and_non_utf8_paths_are_marked_without_losing_raw_identity() {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt;
    let directory = tempfile::tempdir().unwrap();
    let binary = fixture(directory.path());
    let non_utf8 = directory
        .path()
        .join(OsString::from_vec(b"data-\xff".to_vec()));
    let mut long = directory.path().to_path_buf();
    for _ in 0..3 {
        long.push("segment".repeat(12));
    }
    fs::create_dir_all(&long).unwrap();
    long.push("data");
    for (index, path) in [non_utf8, long].iter().enumerate() {
        let capture = directory.path().join(format!("capture-{index}"));
        let output = command(&capture)
            .arg("--")
            .arg(&binary)
            .arg("basic")
            .arg(path)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(fs::read(path).unwrap(), b"abXYef");
        let (calls, _, _, _) = read(&capture);
        if index == 0 {
            assert!(
                calls
                    .iter()
                    .all(|c| text(c, "io.path_raw_hex").ends_with("ff"))
            );
        } else {
            assert!(calls.iter().all(|c| matches!(
                c.attrs.get("io.path_truncated"),
                Some(Attribute::Bool(true))
            )));
        }
    }
}

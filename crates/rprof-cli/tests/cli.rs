#![cfg(target_os = "linux")]

use std::{
    fs,
    io::BufReader,
    path::Path,
    process::{Child, Command, ExitStatus, Stdio},
    thread,
    time::{Duration, Instant},
};

use rprof_capture::summarize;
use rprof_core::stream::EndReason;

fn cli() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_rprof"));
    command
        .env_remove("RPROF_SESSION_ID")
        .env_remove("RPROF_SHARD_ID");
    command
}

fn summary(path: &Path) -> rprof_capture::Summary {
    summarize(BufReader::new(fs::File::open(path).unwrap())).unwrap()
}

struct Running {
    child: Child,
    group: Option<i32>,
}

impl Running {
    fn wait(&mut self) -> ExitStatus {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                return status;
            }
            assert!(Instant::now() < deadline, "profiler did not exit");
            thread::sleep(Duration::from_millis(10));
        }
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        if let Some(group) = self.group {
            unsafe {
                libc::kill(-group, libc::SIGKILL);
            }
        }
    }
}

fn wait_for(mut condition: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !condition() {
        assert!(Instant::now() < deadline, "fixture did not become ready");
        thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn preserves_exit_stdio_and_propagates_session_to_exec() {
    let dir = tempfile::tempdir().unwrap();
    let capture = dir.path().join("capture");
    let session = "00000000-0000-4000-8000-000000000001";
    let output = cli()
        .args(["record", "--session", session, "-o"])
        .arg(&capture)
        .args([
            "--",
            "sh",
            "-c",
            "printf '%s' \"$RPROF_SESSION_ID\"; printf 'diagnostic' >&2; exit 7",
        ])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(7));
    assert_eq!(String::from_utf8(output.stdout).unwrap(), session);
    assert!(
        String::from_utf8(output.stderr)
            .unwrap()
            .contains("diagnostic")
    );
    let summary = summary(&capture);
    assert!(summary.is_complete());
    assert_eq!(summary.header.session_id.to_string(), session);
    let shard = summary.shards.values().next().unwrap();
    assert_eq!(shard.spans, 1);
    assert_eq!(shard.end.as_ref().unwrap().exit_code, Some(7));
    let report = cli().arg("report").arg(capture).output().unwrap();
    assert!(report.status.success());
    assert!(
        String::from_utf8(report.stdout)
            .unwrap()
            .contains("complete")
    );
}

#[test]
fn no_clobber_and_configuration_errors_precede_workload_launch() {
    let dir = tempfile::tempdir().unwrap();
    let capture = dir.path().join("capture");
    let marker = dir.path().join("ran");
    fs::write(&capture, "user data").unwrap();
    let output = cli()
        .args(["record", "-o"])
        .arg(&capture)
        .args(["--", "touch"])
        .arg(&marker)
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(!marker.exists());
    assert_eq!(fs::read_to_string(&capture).unwrap(), "user data");
    fs::remove_file(&capture).unwrap();
    let output = cli()
        .args(["record", "--queue-chunks", "0", "-o"])
        .arg(&capture)
        .args(["--", "touch"])
        .arg(&marker)
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(!capture.exists());
    assert!(!marker.exists());
}

#[test]
fn launch_failure_leaves_readable_failure_metadata() {
    let dir = tempfile::tempdir().unwrap();
    let capture = dir.path().join("capture");
    let output = cli()
        .args(["record", "-o"])
        .arg(&capture)
        .args(["--", "/nonexistent/rprof-test-command"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    let summary = summary(&capture);
    let shard = summary.shards.values().next().unwrap();
    assert_eq!(shard.tracks, 0);
    assert_eq!(shard.end.as_ref().unwrap().reason, EndReason::LaunchFailed);
}

#[test]
fn signals_finalize_capture_and_escalate_ignored_termination() {
    for signal in [libc::SIGTERM, libc::SIGINT] {
        let dir = tempfile::tempdir().unwrap();
        let capture = dir.path().join("capture");
        let pidfile = dir.path().join("pid");
        let child = cli()
            .args(["record", "--grace-ms", "30", "-o"])
            .arg(&capture)
            .args([
                "--",
                "sh",
                "-c",
                "trap '' TERM INT; echo $$ > \"$1\"; exec sleep 30",
                "fixture",
            ])
            .arg(&pidfile)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let mut running = Running { child, group: None };
        wait_for(|| fs::read_to_string(&pidfile).is_ok_and(|s| !s.trim().is_empty()));
        let pid: i32 = fs::read_to_string(&pidfile)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        running.group = Some(pid);
        assert_eq!(unsafe { libc::kill(running.child.id() as i32, signal) }, 0);
        assert_eq!(running.wait().code(), Some(128 + signal));
        let summary = summary(&capture);
        assert!(summary.is_complete());
        let end = summary
            .shards
            .values()
            .next()
            .unwrap()
            .end
            .as_ref()
            .unwrap();
        assert_eq!(end.interrupted_by, Some(signal));
        assert_eq!(end.signal, Some(libc::SIGKILL));
        assert_eq!(end.reason, EndReason::Interrupted);
        assert_eq!(unsafe { libc::kill(pid, 0) }, -1);
        running.group = None;
    }
}

#[test]
fn fork_exec_descendants_are_cleaned_up_after_leader_exit() {
    let dir = tempfile::tempdir().unwrap();
    let capture = dir.path().join("capture");
    let pidfile = dir.path().join("pid");
    let child = cli()
        .args(["record", "--grace-ms", "30", "-o"])
        .arg(&capture)
        .args([
            "--",
            "sh",
            "-c",
            "(trap '' TERM; exec sleep 30) & echo $! > \"$1\"; exit 7",
            "fixture",
        ])
        .arg(&pidfile)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut running = Running { child, group: None };
    assert_eq!(running.wait().code(), Some(7));
    let pid: i32 = fs::read_to_string(pidfile).unwrap().trim().parse().unwrap();
    assert_eq!(unsafe { libc::kill(pid, 0) }, -1);
    assert!(summary(&capture).is_complete());
}

#[test]
fn profiler_sigkill_preserves_a_readable_incomplete_prefix() {
    let dir = tempfile::tempdir().unwrap();
    let capture = dir.path().join("capture");
    let pidfile = dir.path().join("pid");
    let child = cli()
        .args(["record", "--flush-interval-ms", "10", "-o"])
        .arg(&capture)
        .args([
            "--",
            "sh",
            "-c",
            "echo $$ > \"$1\"; exec sleep 30",
            "fixture",
        ])
        .arg(&pidfile)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut running = Running { child, group: None };
    wait_for(|| fs::read_to_string(&pidfile).is_ok_and(|s| !s.trim().is_empty()));
    running.group = Some(fs::read_to_string(pidfile).unwrap().trim().parse().unwrap());
    wait_for(|| {
        fs::File::open(&capture)
            .ok()
            .and_then(|f| summarize(BufReader::new(f)).ok())
            .is_some_and(|s| {
                s.shards
                    .values()
                    .any(|s| s.tracks == 1 && s.observed_ns > 0)
            })
    });
    running.child.kill().unwrap();
    running.wait();
    let summary = summary(&capture);
    assert!(!summary.is_complete());
    assert_eq!(summary.shards.values().next().unwrap().tracks, 1);
    let report = cli().arg("report").arg(&capture).output().unwrap();
    assert!(report.status.success());
    assert!(
        String::from_utf8(report.stdout)
            .unwrap()
            .contains("missing footer")
    );
    // SIGKILL cannot run supervisor cleanup; Running's guard kills the fixture group.
}

#[test]
fn unique_shards_merge_under_an_inherited_session() {
    let dir = tempfile::tempdir().unwrap();
    let session = "00000000-0000-4000-8000-000000000002";
    for _ in 0..2 {
        let output = cli()
            .env("RPROF_SESSION_ID", session)
            .args(["record", "--output-dir"])
            .arg(dir.path())
            .args(["--", "true"])
            .output()
            .unwrap();
        assert!(output.status.success());
    }
    let inputs: Vec<_> = fs::read_dir(dir.path())
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    assert_eq!(inputs.len(), 2);
    let merged = dir.path().join("merged");
    let output = cli()
        .args(["merge", "-o"])
        .arg(&merged)
        .args(inputs)
        .output()
        .unwrap();
    assert!(output.status.success());
    assert_eq!(summary(&merged).shards.len(), 2);
}

#[test]
fn legacy_v1_reports_remain_readable() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("legacy");
    fs::write(
        &path,
        serde_json::to_vec_pretty(&serde_json::json!({
            "schema_version": 1,
            "metadata": { "command": ["true"], "started_unix_ns": 0, "duration_ns": 1,
                "exit_code": 0, "signal": null, "collectors": ["process"] },
            "tracks": [], "spans": [], "counters": []
        }))
        .unwrap(),
    )
    .unwrap();
    let output = cli().arg("report").arg(path).output().unwrap();
    assert!(output.status.success());
    assert!(
        String::from_utf8(output.stdout)
            .unwrap()
            .contains("legacy v1")
    );
}

#[test]
fn foreground_workload_can_read_its_inherited_terminal() {
    use std::{
        io::Write,
        os::{fd::FromRawFd, unix::process::CommandExt},
    };
    let dir = tempfile::tempdir().unwrap();
    let capture = dir.path().join("capture");
    let mut master = -1;
    let mut slave = -1;
    assert_eq!(
        unsafe {
            libc::openpty(
                &mut master,
                &mut slave,
                std::ptr::null_mut(),
                std::ptr::null(),
                std::ptr::null(),
            )
        },
        0
    );
    let mut master = unsafe { fs::File::from_raw_fd(master) };
    let slave = unsafe { fs::File::from_raw_fd(slave) };
    let mut command = cli();
    command
        .args(["record", "-o"])
        .arg(&capture)
        .args(["--", "sh", "-c", "read answer; test \"$answer\" = hello"])
        .stdin(slave.try_clone().unwrap())
        .stdout(slave.try_clone().unwrap())
        .stderr(slave);
    // Give rprof a controlling foreground terminal, as an interactive shell would.
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() < 0 || libc::ioctl(0, libc::TIOCSCTTY, 0) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            if libc::tcsetpgrp(0, libc::getpgrp()) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut running = Running {
        child: command.spawn().unwrap(),
        group: None,
    };
    master.write_all(b"hello\n").unwrap();
    assert!(running.wait().success());
    assert!(summary(&capture).is_complete());
}

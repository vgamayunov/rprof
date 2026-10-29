#![cfg(target_os = "linux")]

use std::{
    fs,
    io::{BufRead, BufReader, Read, Write},
    net::TcpStream,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::mpsc,
    time::Duration,
};

fn cli() -> Command {
    Command::new(env!("CARGO_BIN_EXE_rprof"))
}
fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/multitrack.jsonl")
}

struct Server(Child);
impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn report_files_are_no_clobber_and_failed_exports_are_not_published() {
    let directory = tempfile::tempdir().unwrap();
    let html = directory.path().join("timeline.html");
    let output = cli()
        .arg("report")
        .arg(fixture())
        .args(["--format", "html", "-o"])
        .arg(&html)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let original = fs::read(&html).unwrap();
    let output = cli()
        .arg("report")
        .arg(fixture())
        .args(["--format", "html", "-o"])
        .arg(&html)
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert_eq!(fs::read(&html).unwrap(), original);
    let trace = directory.path().join("trace.json");
    let output = cli()
        .arg("report")
        .arg(fixture())
        .args(["--format", "trace", "-o"])
        .arg(&trace)
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(!trace.exists());
    let output = cli()
        .arg("report")
        .arg(fixture())
        .args(["--format", "trace", "--domain", "0", "-o"])
        .arg(&trace)
        .output()
        .unwrap();
    assert!(output.status.success());
    let trace: serde_json::Value = serde_json::from_slice(&fs::read(trace).unwrap()).unwrap();
    assert_eq!(trace["displayTimeUnit"], "ns");
    assert_eq!(
        fs::read_dir(directory.path()).unwrap().count(),
        2,
        "temporary report files leaked"
    );
}

#[test]
fn real_process_capture_produces_html_and_reference_trace() {
    let directory = tempfile::tempdir().unwrap();
    let capture = directory.path().join("capture.jsonl");
    let output = cli()
        .args(["record", "-o"])
        .arg(&capture)
        .args(["--", "true"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let html = cli()
        .arg("report")
        .arg(&capture)
        .args(["--format", "html"])
        .output()
        .unwrap();
    assert!(html.status.success());
    let html = String::from_utf8(html.stdout).unwrap();
    assert!(html.contains("Measured capture") && html.contains("supervised workload"));
    let trace = cli()
        .arg("report")
        .arg(&capture)
        .args(["--format", "trace"])
        .output()
        .unwrap();
    assert!(trace.status.success());
    let trace: serde_json::Value = serde_json::from_slice(&trace.stdout).unwrap();
    assert_eq!(
        trace["traceEvents"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|e| e["ph"] == "X")
            .count(),
        1
    );
}

fn request(address: &str, method: &str, route: &str) -> String {
    let mut stream = TcpStream::connect(address).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    // HTTP/1.0 asks for a close-delimited body, avoiding a chunk decoder in this
    // minimal test client. Browsers handle HTTP/1.1 chunked responses themselves.
    write!(
        stream,
        "{method} {route} HTTP/1.0\r\nHost: {address}\r\nConnection: close\r\n\r\n"
    )
    .unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).unwrap();
    response
}

#[test]
fn local_viewer_serves_same_artifact_and_exits_on_signal() {
    let mut server = Server(
        cli()
            .arg("view")
            .arg(fixture())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let stdout = server.0.stdout.take().unwrap();
    let (sender, receiver) = mpsc::channel();
    std::thread::spawn(move || {
        let mut first = String::new();
        let _ = BufReader::new(stdout).read_line(&mut first);
        let _ = sender.send(first);
    });
    let line = receiver
        .recv_timeout(Duration::from_secs(10))
        .expect("viewer did not start");
    let url = line.trim().strip_prefix("Viewer: ").unwrap();
    assert!(url.starts_with("http://127.0.0.1:"));
    let address = url.trim_start_matches("http://").trim_end_matches('/');
    let response = request(address, "GET", "/");
    assert_eq!(response.split_whitespace().nth(1), Some("200"));
    let (_, body) = response.split_once("\r\n\r\n").unwrap();
    let direct = cli()
        .arg("report")
        .arg(fixture())
        .args(["--format", "html"])
        .output()
        .unwrap();
    assert!(direct.status.success());
    assert!(
        body.as_bytes() == direct.stdout,
        "served artifact differs from offline export"
    );
    let head = request(address, "HEAD", "/");
    assert_eq!(head.split_whitespace().nth(1), Some("200"));
    assert_eq!(head.split_once("\r\n\r\n").unwrap().1, "");
    assert_eq!(
        request(address, "GET", "/../Cargo.toml")
            .split_whitespace()
            .nth(1),
        Some("404")
    );
    assert_eq!(
        request(address, "POST", "/").split_whitespace().nth(1),
        Some("405")
    );
    unsafe {
        libc::kill(server.0.id() as i32, libc::SIGTERM);
    }
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(status) = server.0.try_wait().unwrap() {
            assert!(status.success());
            break;
        }
        assert!(std::time::Instant::now() < deadline, "viewer did not stop");
        std::thread::sleep(Duration::from_millis(10));
    }
}

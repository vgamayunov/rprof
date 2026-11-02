use serde_json::Value;
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};
fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/projection")
}
fn project() -> Command {
    let f = fixtures();
    let mut c = Command::new(env!("CARGO_BIN_EXE_rprof"));
    c.arg("project")
        .arg(f.join("baseline.jsonl"))
        .arg("--source-hardware")
        .arg(f.join("source.json"))
        .arg("--target-hardware")
        .arg(f.join("target.json"))
        .arg("--workload-model")
        .arg(f.join("workload.json"));
    c
}
#[test]
fn inspect_project_and_replay_bind_evidence_without_trusting_edited_observations() {
    let dir = tempfile::tempdir().unwrap();
    let f = fixtures();
    let inspection = Command::new(env!("CARGO_BIN_EXE_rprof"))
        .arg("project")
        .arg(f.join("baseline.jsonl"))
        .arg("--inspect")
        .output()
        .unwrap();
    assert!(
        inspection.status.success(),
        "{}",
        String::from_utf8_lossy(&inspection.stderr)
    );
    let evidence: Value = serde_json::from_slice(&inspection.stdout).unwrap();
    assert_eq!(evidence["spans"].as_array().unwrap().len(), 4);
    let output = project().output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let artifact: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(artifact["kind"], "rprof-projection");
    assert_eq!(artifact["scenarios"][0]["runtime"]["central_ns"], "120");
    assert_eq!(artifact["hashes"]["capture"], evidence["capture_sha256"]);
    let mut request = artifact["request"].clone();
    request["baseline"]["spans"][0]["duration_ns"] = "1".into();
    let path = dir.path().join("request.json");
    fs::write(&path, serde_json::to_vec(&request).unwrap()).unwrap();
    let replay = Command::new(env!("CARGO_BIN_EXE_rprof"))
        .arg("project")
        .arg(f.join("baseline.jsonl"))
        .arg("--request")
        .arg(path)
        .output()
        .unwrap();
    assert!(replay.status.success());
    let replay: Value = serde_json::from_slice(&replay.stdout).unwrap();
    assert_eq!(
        artifact, replay,
        "saved requests must reload measured evidence, not accept edited observations"
    );
}
#[test]
fn html_and_text_preserve_provenance_and_outputs_are_no_clobber() {
    let dir = tempfile::tempdir().unwrap();
    let html = dir.path().join("projection.html");
    let text = dir.path().join("projection.txt");
    assert!(
        project()
            .args(["--format", "html", "-o"])
            .arg(&html)
            .status()
            .unwrap()
            .success()
    );
    assert!(
        project()
            .args(["--format", "text", "-o"])
            .arg(&text)
            .status()
            .unwrap()
            .success()
    );
    let data = fs::read_to_string(&html).unwrap();
    assert!(data.contains("id=\"rprof-project-wasm\""));
    assert!(data.contains("\"kind\":\"rprof-projection\""));
    assert!(
        fs::read_to_string(&text)
            .unwrap()
            .contains("consistency check")
    );
    let bytes = fs::read(&html).unwrap();
    assert!(
        !project()
            .args(["--format", "html", "-o"])
            .arg(&html)
            .status()
            .unwrap()
            .success()
    );
    assert_eq!(fs::read(html).unwrap(), bytes);
}
#[test]
fn invalid_inputs_leave_no_published_artifact_and_infeasible_scenarios_are_null() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("request.json");
    let output_path = dir.path().join("output");
    let output = project().output().unwrap();
    let artifact: Value = serde_json::from_slice(&output.stdout).unwrap();
    let mut request = artifact["request"].clone();
    request["source"]["version"] = 99.into();
    fs::write(&path, serde_json::to_vec(&request).unwrap()).unwrap();
    let run = || {
        Command::new(env!("CARGO_BIN_EXE_rprof"))
            .arg("project")
            .arg(fixtures().join("baseline.jsonl"))
            .arg("--request")
            .arg(&path)
            .arg("-o")
            .arg(&output_path)
            .output()
            .unwrap()
    };
    assert!(!run().status.success());
    assert!(!output_path.exists());
    request["source"]["version"] = 1.into();
    request["targets"][0]["resources"]["gpu"]["capacity_bytes_per_node"] = "1024".into();
    fs::write(&path, serde_json::to_vec(&request).unwrap()).unwrap();
    assert!(run().status.success());
    let p: Value = serde_json::from_slice(&fs::read(output_path).unwrap()).unwrap();
    assert_eq!(p["scenarios"][0]["status"], "infeasible");
    assert!(p["scenarios"][0]["runtime"].is_null());
}

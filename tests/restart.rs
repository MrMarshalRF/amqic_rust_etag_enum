use std::{
    process::Command,
    time::{SystemTime, UNIX_EPOCH},
};

#[test]
fn real_process_restart_preserves_committed_reports_and_unknowns() {
    let path = std::env::temp_dir().join(format!(
        "etag-restart-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let output = Command::new(env!("CARGO_BIN_EXE_lab-restart-scenarios"))
        .args(["--lab-bin", env!("CARGO_BIN_EXE_etag-lab"), "--work-dir"])
        .arg(&path)
        .output()
        .expect("start finite restart consumer");
    assert!(
        output.status.success(),
        "retained evidence: {}\nstdout: {}\nstderr: {}",
        path.display(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let report: serde_json::Value =
        serde_json::from_slice(&std::fs::read(path.join("report.json")).unwrap()).unwrap();
    let tagged: etag_for_codex::Report<serde_json::Value> =
        serde_json::from_value(report.clone()).unwrap();
    tagged.verify(&tagged.input_etag).unwrap();
    assert_eq!(report["payload"]["failed"], 0);
    assert_eq!(report["payload"]["cases"].as_array().unwrap().len(), 7);
    let processes = report["payload"]["processes"].as_array().unwrap();
    assert_eq!(processes.len(), 12);
    assert_eq!(
        processes
            .iter()
            .filter(|p| p["mode"] == "hard_kill")
            .count(),
        5
    );
    assert!(report["payload"]["processes"]
        .as_array()
        .unwrap()
        .iter()
        .all(|p| p["listener_absent"] == true && p["waited"] == true));
    let lifecycle_before = std::fs::read(path.join("lifecycle.jsonl")).unwrap();
    let refused = Command::new(env!("CARGO_BIN_EXE_lab-restart-scenarios"))
        .args(["--lab-bin", env!("CARGO_BIN_EXE_etag-lab"), "--work-dir"])
        .arg(&path)
        .output()
        .unwrap();
    assert!(
        !refused.status.success(),
        "existing work directory must be refused"
    );
    assert_eq!(
        std::fs::read(path.join("lifecycle.jsonl")).unwrap(),
        lifecycle_before,
        "refused run must not launch another child"
    );
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(
            &std::fs::read(path.join("report.json")).unwrap()
        )
        .unwrap(),
        report,
        "refused run preserves report"
    );
}

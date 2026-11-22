//! Local synthetic fixtures are retained under out/durable-tests for inspection.
use etag_for_codex::{api, journal::Journal, kernel, Error};
use rusqlite::Connection;
use serde_json::{json, Value};
use std::{fs, path::{Path, PathBuf}, sync::{atomic::{AtomicU64, Ordering}, Arc, Barrier}, time::{Duration, Instant, SystemTime, UNIX_EPOCH}};

#[cfg(windows)]
fn child_start_identity(child: &std::process::Child) -> String {
    use std::os::windows::io::AsRawHandle;
    #[repr(C)] #[derive(Default, Copy, Clone)] struct FileTime { low: u32, high: u32 }
    #[link(name = "kernel32")]
    unsafe extern "system" { fn GetProcessTimes(handle: *mut std::ffi::c_void, creation: *mut FileTime, exit: *mut FileTime, kernel: *mut FileTime, user: *mut FileTime) -> i32; }
    let (mut creation, mut exit, mut kernel, mut user) = (FileTime::default(), FileTime::default(), FileTime::default(), FileTime::default());
    let ok = unsafe { GetProcessTimes(child.as_raw_handle(), &mut creation, &mut exit, &mut kernel, &mut user) };
    assert_ne!(ok, 0, "GetProcessTimes failed");
    format!("windows-filetime:{}", ((creation.high as u64) << 32) | creation.low as u64)
}
#[cfg(not(windows))]
fn child_start_identity(child: &std::process::Child) -> String {
    fs::read_to_string(format!("/proc/{}/stat", child.id())).unwrap_or_else(|_| "unavailable-non-linux-host".into())
}

struct OwnedChild { child: std::process::Child, evidence: PathBuf, start: String }
impl OwnedChild {
    fn wait(&mut self) -> std::process::ExitStatus {
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                fs::write(&self.evidence, serde_json::to_vec_pretty(&json!({"pid":self.child.id(),"start_identity":self.start,"exit_code":status.code(),"waited":true,"cleanup":"released"})).unwrap()).unwrap();
                return status;
            }
            assert!(Instant::now() < deadline, "owned API helper exceeded deadline");
            std::thread::sleep(Duration::from_millis(25));
        }
    }
}
impl Drop for OwnedChild {
    fn drop(&mut self) {
        match self.child.try_wait() {
            Ok(Some(_)) => {},
            _ => { let _ = self.child.kill(); let _ = self.child.wait(); },
        }
    }
}

fn fixture(name: &str) -> PathBuf {
    static SEQUENCE: AtomicU64 = AtomicU64::new(0);
    let unique = format!("{}-{}-{}", std::process::id(), SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos(), SEQUENCE.fetch_add(1, Ordering::Relaxed));
    let directory = Path::new(env!("CARGO_MANIFEST_DIR")).join("out/durable-tests").join(unique);
    fs::create_dir_all(&directory).unwrap();
    directory.join(format!("{name}.sqlite"))
}
fn batch(operation: &str, revision: u64) -> kernel::Batch {
    kernel::Batch { schema: "amqic.etag.kernel.v1".into(), scope: "durable-test".into(), kernel_sha256: "0".repeat(64),
        operation_id: operation.into(), expected_revision: revision, members: vec![kernel::Member {
            id: "counter".into(), command: kernel::Command::IncrementCounter { amount: 7 },
        }] }
}
fn kernel_at(path: &Path) -> kernel::Session {
    kernel::Session::with_journal("durable-test".into(), "0".repeat(64), Journal::open(path).unwrap()).unwrap()
}
fn api_batch() -> api::Batch {
    api::Batch { schema: "amqic.etag.openai-batch.v1".into(), scope: "durable-test".into(), operation_id: "submit-1".into(),
        endpoint: api::Endpoint::Responses, requests: vec![api::Request { custom_id: "one".into(), body: json!({"model":"synthetic", "input":"hello"}) }] }
}
fn api_at(path: &Path) -> api::ApiSession {
    api::ApiSession::with_journal("durable-test".into(), Journal::open(path).unwrap()).unwrap()
}

#[test]
fn completed_and_partial_reopen_preserve_exact_report_and_revision() {
    for partial in [false, true] {
        let path = fixture(if partial { "partial" } else { "completed" });
        let mut b = batch("first", 0);
        if partial { b.members.push(kernel::Member { id: "overflow".into(), command: kernel::Command::SumI64 { values: vec![i64::MAX, 1] } }); }
        let first = {
            let s = kernel_at(&path);
            s.execute(&b, Some(b.etag().unwrap().as_str())).unwrap()
        };
        assert_eq!(first.payload.status, if partial { kernel::Status::Partial } else { kernel::Status::Completed });
        let s = kernel_at(&path);
        assert_eq!(s.lookup("first").unwrap(), Some(first.clone()));
        assert_eq!(s.execute(&b, Some(b.etag().unwrap().as_str())).unwrap(), first);
        assert_eq!(s.state().unwrap(), (1, 7));
        let next = batch("second", 1);
        let second = s.execute(&next, Some(next.etag().unwrap().as_str())).unwrap();
        assert_eq!((second.payload.revision, second.payload.counter), (2, 14));
        assert_eq!(s.execute(&b, Some(b.etag().unwrap().as_str())).unwrap(), first);
        assert_eq!(s.state().unwrap(), (2, 14));
    }
}

#[test]
fn reopen_conflicts_and_stale_inputs_have_zero_effects() {
    let path = fixture("conflict");
    let b = batch("first", 0);
    { let s = kernel_at(&path); s.execute(&b, Some(b.etag().unwrap().as_str())).unwrap(); }
    let s = kernel_at(&path);
    let mut changed = b.clone();
    changed.members[0].command = kernel::Command::IncrementCounter { amount: 100 };
    assert_eq!(s.execute(&changed, Some(changed.etag().unwrap().as_str())), Err(Error::OperationConflict));
    let stale = batch("new-operation", 0);
    assert_eq!(s.execute(&stale, Some(stale.etag().unwrap().as_str())), Err(Error::StaleRevision));
    assert_eq!(s.execute(&batch("missing", 1), None), Err(Error::MissingPrecondition));
    assert_eq!(s.execute(&batch("wrong-tag", 1), Some(b.etag().unwrap().as_str())), Err(Error::StaleSnapshot));
    assert_eq!(s.state().unwrap(), (1, 7));
    assert!(kernel::Session::with_journal("durable-test".into(), "1".repeat(64), Journal::open(&path).unwrap()).is_err());
}

#[test]
fn independent_connections_admit_one_revision_and_recover_one_operation() {
    for same_operation in [false, true] {
        let path = fixture("competing");
        let sessions: Vec<_> = (0..4).map(|_| kernel_at(&path)).collect();
        let gate = Arc::new(Barrier::new(sessions.len()));
        let threads: Vec<_> = sessions.into_iter().enumerate().map(|(i, session)| {
            let gate = gate.clone();
            std::thread::spawn(move || {
                let b = batch(if same_operation { "same" } else { ["a", "b", "c", "d"][i] }, 0);
                gate.wait(); session.execute(&b, Some(b.etag().unwrap().as_str()))
            })
        }).collect();
        let results: Vec<_> = threads.into_iter().map(|t| t.join().unwrap()).collect();
        assert_eq!(results.iter().filter(|r| r.is_ok()).count(), if same_operation { 4 } else { 1 });
        assert!(results.iter().filter_map(|r| r.as_ref().err()).all(|e| *e == Error::StaleRevision));
        if same_operation { assert!(results.iter().all(|r| r == &results[0])); }
        assert_eq!(kernel_at(&path).state().unwrap(), (1, 7));
    }
}

struct InspectTransport { path: PathBuf, upload: api::TransportOutcome, create: api::TransportOutcome, uploads: usize, creates: usize }
impl InspectTransport {
    fn new(path: &Path) -> Self {
        Self { path: path.into(), upload: api::TransportOutcome::Confirmed { id: "file-1".into() },
            create: api::TransportOutcome::Confirmed { id: "batch-1".into() }, uploads: 0, creates: 0 }
    }
    fn persisted_intent(&self, expected: api::SubmissionState) {
        // A separate SQLite connection must see a COMMITTED marker before the
        // callback returns. Reading this session's memory would miss that bug.
        let separate = Journal::open(&self.path).unwrap();
        let snapshot: Value = separate.read("api.v1", "durable-test").unwrap().unwrap();
        let report: etag_for_codex::Report<api::SubmissionState> = serde_json::from_value(snapshot["ledger"]["submit-1"]["report"].clone()).unwrap();
        report.verify(api_batch().prepare().unwrap().etag()).unwrap();
        assert_eq!(report.payload, expected);
        assert_eq!(api_at(&self.path).lookup("submit-1").unwrap(), Some(report));
    }
}
impl api::Transport for InspectTransport {
    fn upload(&mut self, bytes: &[u8]) -> api::TransportOutcome {
        self.uploads += 1;
        assert_eq!(bytes, api_batch().prepare().unwrap().jsonl());
        self.persisted_intent(api::SubmissionState::UnknownUpload);
        self.upload.clone()
    }
    fn create(&mut self, body: &Value) -> api::TransportOutcome {
        self.creates += 1;
        assert_eq!(body, &api_batch().prepare().unwrap().create_body("file-1").unwrap());
        self.persisted_intent(api::SubmissionState::UnknownCreate { file_id: "file-1".into() });
        self.create.clone()
    }
}
struct NeverTransport;
impl api::Transport for NeverTransport {
    fn upload(&mut self, _: &[u8]) -> api::TransportOutcome { panic!("terminal operation uploaded twice") }
    fn create(&mut self, _: &Value) -> api::TransportOutcome { panic!("terminal operation created twice") }
}

#[test]
fn uploaded_reopen_resumes_create_and_submitted_returns_original_report() {
    let path = fixture("submission");
    let p = api_batch().prepare().unwrap();
    let mut t = InspectTransport::new(&path);
    let uploaded = { api_at(&path).advance(&p, Some(p.etag().as_str()), &mut t).unwrap() };
    assert_eq!(uploaded.payload, api::SubmissionState::Uploaded { file_id: "file-1".into() });
    let submitted = { api_at(&path).advance(&p, Some(p.etag().as_str()), &mut t).unwrap() };
    assert_eq!(submitted.payload, api::SubmissionState::Submitted { file_id: "file-1".into(), batch_id: "batch-1".into() });
    assert_eq!((t.uploads, t.creates), (1, 1));
    let reopened = api_at(&path);
    assert_eq!(reopened.lookup(p.operation_id()).unwrap(), Some(submitted.clone()));
    assert_eq!(reopened.advance(&p, Some(p.etag().as_str()), &mut NeverTransport).unwrap(), submitted);
    let mut changed = api_batch(); changed.requests[0].body["input"] = json!("changed"); let changed = changed.prepare().unwrap();
    assert_eq!(reopened.advance(&changed, Some(changed.etag().as_str()), &mut NeverTransport), Err(Error::OperationConflict));
    assert_eq!(reopened.lookup(p.operation_id()).unwrap(), Some(submitted));
}

#[test]
fn unknown_upload_and_create_remain_terminal_after_reopen() {
    for upload_unknown in [true, false] {
        let path = fixture("unknown");
        let p = api_batch().prepare().unwrap();
        let mut t = InspectTransport::new(&path);
        if upload_unknown { t.upload = api::TransportOutcome::Unknown; } else { t.create = api::TransportOutcome::Unknown; }
        let first = api_at(&path).advance(&p, Some(p.etag().as_str()), &mut t).unwrap();
        let terminal = if upload_unknown { first } else { api_at(&path).advance(&p, Some(p.etag().as_str()), &mut t).unwrap() };
        let expected = if upload_unknown { api::SubmissionState::UnknownUpload } else { api::SubmissionState::UnknownCreate { file_id: "file-1".into() } };
        assert_eq!(terminal.payload, expected);
        for _ in 0..3 { assert_eq!(api_at(&path).advance(&p, Some(p.etag().as_str()), &mut NeverTransport).unwrap(), terminal); }
        assert_eq!((t.uploads, t.creates), (1, usize::from(!upload_unknown)));
    }
}

#[test]
#[ignore = "subprocess fixture, invoked only by api_process_death_preserves_terminal_intent"]
fn durable_api_process_death_child() {
    let path = PathBuf::from(std::env::var_os("ETAG_DURABLE_CHILD_PATH").expect("parent-bound fixture path"));
    let phase = std::env::var("ETAG_DURABLE_CHILD_PHASE").unwrap();
    let marker = PathBuf::from(std::env::var_os("ETAG_DURABLE_CHILD_MARKER").unwrap());
    struct Die { inner: InspectTransport, phase: String, marker: PathBuf }
    impl Die {
        fn stop(&self, phase: &str) {
            fs::write(&self.marker, serde_json::to_vec(&json!({"phase":phase,"pid":std::process::id(),"committed_intent_observed":true})).unwrap()).unwrap();
            // Abrupt process exit skips Rust drops and SQLite connection cleanup.
            std::process::exit(73);
        }
    }
    impl api::Transport for Die {
        fn upload(&mut self, bytes: &[u8]) -> api::TransportOutcome {
            let outcome = self.inner.upload(bytes);
            if self.phase == "upload" { self.stop("upload"); }
            outcome
        }
        fn create(&mut self, body: &Value) -> api::TransportOutcome {
            let outcome = self.inner.create(body);
            self.stop("create");
            outcome
        }
    }
    assert!(phase == "upload" || phase == "create");
    let p = api_batch().prepare().unwrap();
    let session = api_at(&path);
    let mut t = Die { inner: InspectTransport::new(&path), phase, marker };
    session.advance(&p, Some(p.etag().as_str()), &mut t).unwrap();
    session.advance(&p, Some(p.etag().as_str()), &mut t).unwrap();
    panic!("helper did not exit in requested callback");
}

#[test]
fn api_process_death_preserves_terminal_intent() {
    for phase in ["upload", "create"] {
        let path = fixture("process-death");
        let marker = path.with_extension("callback.json");
        let evidence = path.with_extension("process.json");
        fs::write(&evidence, serde_json::to_vec_pretty(&json!({"task":"etag-durable-20261122","owner":"durable_library_tests","kind":"launch-intent","executable":std::env::current_exe().unwrap(),"phase":phase,"disposable_process":true,"retained_database":path,"cleanup_owner":"durable_library_tests"})).unwrap()).unwrap();
        let child = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "durable_api_process_death_child", "--ignored", "--nocapture"])
            .env("ETAG_DURABLE_CHILD_PATH", &path).env("ETAG_DURABLE_CHILD_PHASE", phase).env("ETAG_DURABLE_CHILD_MARKER", &marker)
            .stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).spawn().unwrap();
        let mut owned = OwnedChild { start: String::new(), child, evidence };
        owned.start = child_start_identity(&owned.child);
        fs::write(&owned.evidence, serde_json::to_vec_pretty(&json!({"pid":owned.child.id(),"start_identity":owned.start,"phase":phase,"kind":"observed-owned-child","cleanup":"pending"})).unwrap()).unwrap();
        assert_eq!(owned.wait().code(), Some(73));
        let callback: Value = serde_json::from_slice(&fs::read(&marker).unwrap()).unwrap();
        assert_eq!(callback["pid"], owned.child.id()); assert_eq!(callback["phase"], phase);
        assert_eq!(callback["committed_intent_observed"], true);
        let p = api_batch().prepare().unwrap(); let reopened = api_at(&path);
        let report = reopened.lookup(p.operation_id()).unwrap().unwrap();
        assert_eq!(report.payload, if phase == "upload" { api::SubmissionState::UnknownUpload } else { api::SubmissionState::UnknownCreate { file_id: "file-1".into() } });
        assert_eq!(reopened.advance(&p, Some(p.etag().as_str()), &mut NeverTransport).unwrap(), report);
    }
}

#[test]
fn confirmed_transport_with_failed_completion_commit_remains_unknown() {
    let path = fixture("completion-failure");
    let p = api_batch().prepare().unwrap();
    let session = api_at(&path);
    struct LockAfterUpload { inspect: InspectTransport, lock: Connection }
    impl api::Transport for LockAfterUpload {
        fn upload(&mut self, bytes: &[u8]) -> api::TransportOutcome {
            let confirmed = self.inspect.upload(bytes);
            // Intent is committed, external transport has succeeded, but its
            // completion cannot acquire the writer lock. Preserve uncertainty.
            self.lock.execute_batch("BEGIN IMMEDIATE").unwrap();
            confirmed
        }
        fn create(&mut self, _: &Value) -> api::TransportOutcome { panic!("create after failed upload completion") }
    }
    let mut t = LockAfterUpload { inspect: InspectTransport::new(&path), lock: Connection::open(&path).unwrap() };
    let started = Instant::now();
    let result = session.advance(&p, Some(p.etag().as_str()), &mut t);
    assert!(result.is_err()); assert!(started.elapsed() < Duration::from_secs(6));
    t.lock.execute_batch("ROLLBACK").unwrap();
    assert_eq!(t.inspect.uploads, 1);
    drop(t); drop(session);
    let reopened = api_at(&path);
    let report = reopened.lookup(p.operation_id()).unwrap().unwrap();
    assert_eq!(report.payload, api::SubmissionState::UnknownUpload);
    assert_eq!(reopened.advance(&p, Some(p.etag().as_str()), &mut NeverTransport).unwrap(), report);
}

#[test]
fn invalid_checkpoint_restore_cannot_mutate_memory_or_roll_back_disk() {
    let b = batch("first", 0);
    let memory = kernel::Session::new(b.scope.clone(), b.kernel_sha256.clone()).unwrap();
    let report = memory.execute(&b, Some(b.etag().unwrap().as_str())).unwrap();
    let good = memory.checkpoint().unwrap();
    for field in ["counter", "revision", "scope", "kernel_sha256"] {
        let mut value = serde_json::to_value(&good).unwrap();
        value[field] = if field == "counter" || field == "revision" { json!(999) } else { json!("forged") };
        let bad: kernel::Checkpoint = serde_json::from_value(value).unwrap();
        assert!(memory.restore(&bad).is_err());
        assert_eq!(memory.lookup("first").unwrap(), Some(report.clone()));
        assert_eq!(memory.state().unwrap(), (1, 7));
    }
    // Rehash forged reports so rejection must come from checkpoint semantics,
    // rather than merely from a stale result ETag.
    for mutation in ["counter", "revision", "member-kind"] {
        let mut value = serde_json::to_value(&good).unwrap();
        let mut forged = report.clone();
        match mutation {
            "counter" => { forged.payload.counter = 999; value["counter"] = json!(999); }
            "revision" => forged.payload.revision = 2,
            "member-kind" => forged.payload.members[0].result = kernel::MemberResult::SumI64 { value: 7 },
            _ => unreachable!(),
        }
        let forged = etag_for_codex::Report::new(forged.input_etag, forged.payload).unwrap();
        forged.verify(&b.etag().unwrap()).unwrap();
        value["ledger"]["first"]["report"] = serde_json::to_value(forged).unwrap();
        let bad: kernel::Checkpoint = serde_json::from_value(value).unwrap();
        assert!(memory.restore(&bad).is_err(), "{mutation}");
        assert_eq!(memory.lookup("first").unwrap(), Some(report.clone()));
    }
    let restored = kernel::Session::new(b.scope.clone(), b.kernel_sha256.clone()).unwrap();
    restored.restore(&good).unwrap();
    assert_eq!(restored.execute(&b, Some(b.etag().unwrap().as_str())).unwrap(), report);
    let disk = kernel_at(&fixture("import-refused"));
    assert!(disk.restore(&good).is_err());
    assert_eq!(disk.state().unwrap(), (0, 0));
    let api_memory = api::ApiSession::new("durable-test".into()).unwrap();
    let api_good = api_memory.checkpoint().unwrap();
    let mut value = serde_json::to_value(&api_good).unwrap(); value["scope"] = json!("other");
    let bad: api::Checkpoint = serde_json::from_value(value).unwrap();
    assert!(api_memory.restore(&bad).is_err());
    let api_disk = api_at(&fixture("api-import-refused"));
    assert!(api_disk.restore(&api_good).is_err());
    assert!(api_disk.lookup("submit-1").unwrap().is_none());
}

#[test]
fn foreign_empty_and_truncated_databases_fail_closed_without_recreation() {
    for bytes in [Vec::new(), b"not a sqlite database".to_vec()] {
        let path = fixture("foreign-bytes"); fs::write(&path, &bytes).unwrap();
        assert!(Journal::open(&path).is_err()); assert_eq!(fs::read(&path).unwrap(), bytes);
    }
    let path = fixture("foreign-schema");
    { let c = Connection::open(&path).unwrap(); c.execute_batch("CREATE TABLE customer_data(id INTEGER)").unwrap(); }
    let before = fs::read(&path).unwrap();
    assert!(Journal::open(&path).is_err()); assert_eq!(fs::read(&path).unwrap(), before);
    let path = fixture("truncated");
    { let s = kernel_at(&path); let b = batch("first", 0); s.execute(&b, Some(b.etag().unwrap().as_str())).unwrap(); }
    let mut bytes = fs::read(&path).unwrap(); bytes.truncate(bytes.len() / 2); fs::write(&path, &bytes).unwrap();
    assert!(Journal::open(&path).is_err()); assert_eq!(fs::read(&path).unwrap(), bytes);
}

#[test]
fn corrupt_row_and_schema_versions_refuse_before_transaction_closure() {
    for mutation in ["UPDATE snapshots SET payload=x'7b7d'", "UPDATE snapshots SET checksum='bad'", "UPDATE snapshots SET version=2", "PRAGMA user_version=2", "PRAGMA application_id=0"] {
        let path = fixture("corrupt-row");
        let journal = Journal::open(&path).unwrap();
        journal.transact("test", "scope", &json!({"count":0}), |v| { v["count"] = json!(1); Ok(()) }).unwrap();
        { let c = Connection::open(&path).unwrap(); c.execute_batch(mutation).unwrap(); }
        let mut called = false;
        let result = journal.transact("test", "scope", &json!({"count":0}), |_| { called = true; Ok(()) });
        assert!(result.is_err(), "{mutation}"); assert!(!called, "{mutation}");
        assert!(journal.read::<Value>("test", "scope").is_err());
    }
}

#[test]
fn missing_established_rows_cannot_reinitialize_live_sessions() {
    let path = fixture("missing-row");
    let kernel = kernel_at(&path);
    let api = api_at(&path);
    let b = batch("first", 0);
    kernel.execute(&b, Some(b.etag().unwrap().as_str())).unwrap();
    { let c = Connection::open(&path).unwrap(); c.execute_batch("DELETE FROM snapshots").unwrap(); }
    assert!(kernel.execute(&b, Some(b.etag().unwrap().as_str())).is_err());
    let p = api_batch().prepare().unwrap();
    assert!(api.advance(&p, Some(p.etag().as_str()), &mut NeverTransport).is_err());
    // The failed calls must not recreate either missing row.
    let journal = Journal::open(&path).unwrap();
    assert!(journal.read::<Value>("kernel.v1", "durable-test").unwrap().is_none());
    assert!(journal.read::<Value>("api.v1", "durable-test").unwrap().is_none());
}

#[test]
fn failed_closure_and_failed_sqlite_write_leave_committed_state_unchanged() {
    let path = fixture("rollback"); let journal = Journal::open(&path).unwrap();
    journal.transact("test", "scope", &0u64, |v| { *v = 1; Ok(()) }).unwrap();
    let result: etag_for_codex::Result<()> = journal.transact("test", "scope", &0u64, |v| { *v = 99; Err(Error::OperationConflict) });
    assert_eq!(result, Err(Error::OperationConflict));
    assert_eq!(journal.read::<u64>("test", "scope").unwrap(), Some(1));
    let committed_bytes = fs::read(&path).unwrap();
    // A directory at SQLite's rollback-journal filename deterministically
    // prevents a write, without changing permissions or deleting the ledger.
    fs::create_dir(format!("{}-journal", path.display())).unwrap();
    assert!(journal.transact("test", "scope", &0u64, |v| { *v = 2; Ok(()) }).is_err());
    assert_eq!(fs::read(&path).unwrap(), committed_bytes);
}

#[test]
fn busy_writer_is_bounded_and_cannot_enter_local_effect_closure() {
    let path = fixture("busy"); let journal = Journal::open(&path).unwrap();
    journal.transact("test", "scope", &0u64, |_| Ok(())).unwrap();
    let lock = Connection::open(&path).unwrap(); lock.execute_batch("BEGIN IMMEDIATE").unwrap();
    let mut called = false; let started = Instant::now();
    let result = journal.transact("test", "scope", &0u64, |_| { called = true; Ok(()) });
    let elapsed = started.elapsed(); lock.execute_batch("ROLLBACK").unwrap();
    assert!(result.is_err()); assert!(!called); assert!(elapsed < Duration::from_secs(6), "busy wait exceeded bound: {elapsed:?}");
    assert_eq!(journal.read::<u64>("test", "scope").unwrap(), Some(0));
}

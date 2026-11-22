//! Behavioral tests of the purpose-built synthetic lab; no live upstreams.
use etag_for_codex::{
    lab::{self, Lab, LabResponse},
    Etag, Report,
};
use serde_json::{json, Value};
use std::{
    io::{Read, Write},
    net::{SocketAddr, TcpListener, TcpStream},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant, SystemTime},
};

fn lab() -> Lab {
    Lab::new("a".repeat(64)).unwrap()
}
fn descriptor(id: &str, revision: u64, amount: i64) -> Value {
    json!({"schema":"amqic.etag.lab.v1","fault":"none","batch":{
        "schema":"amqic.etag.kernel.v1","scope":"lab","kernel_sha256":"a".repeat(64),
        "operation_id":id,"expected_revision":revision,
        "members":[{"id":"counter","command":{"kind":"increment_counter","amount":amount}}]}})
}
fn body(value: &Value) -> Vec<u8> {
    serde_json::to_vec(value).unwrap()
}
fn value(response: LabResponse, status: u16) -> Value {
    assert_eq!(
        response.status,
        status,
        "{}",
        String::from_utf8_lossy(&response.body)
    );
    serde_json::from_slice(&response.body).unwrap()
}
fn info(lab: &Lab) -> Value {
    value(lab.handle("GET", "/api/lab/info", None, &[]), 200)["result"].clone()
}
fn tag(lab: &Lab, path: &str, descriptor: &Value) -> String {
    value(lab.handle("POST", path, None, &body(descriptor)), 200)["result"]["input_etag"]
        .as_str()
        .unwrap()
        .to_owned()
}
fn run(lab: &Lab, descriptor: &Value) -> Value {
    let tag = tag(lab, "/api/lab/plan", descriptor);
    value(
        lab.handle("POST", "/api/lab/run", Some(&tag), &body(descriptor)),
        200,
    )["result"]
        .clone()
}
fn effects(lab: &Lab) -> (Value, Value, Value) {
    let i = info(lab);
    (
        i["counter"].clone(),
        i["revision"].clone(),
        i["executions"].clone(),
    )
}

#[test]
fn accepted_report_replays_exactly_and_stale_or_conflicting_work_has_no_effect() {
    let lab = lab();
    let first = descriptor("first", 0, 7);
    let accepted_tag = tag(&lab, "/api/lab/plan", &first);
    let report = run(&lab, &first);
    let typed: Report<etag_for_codex::kernel::Output> =
        serde_json::from_value(report.clone()).unwrap();
    typed.verify(&Etag::parse(&accepted_tag).unwrap()).unwrap();
    assert_eq!(report["payload"]["counter"], 7);
    assert_eq!(effects(&lab), (json!(7), json!(1), json!(1)));
    assert_eq!(run(&lab, &first), report);
    assert_eq!(
        value(
            lab.handle("GET", "/api/lab/operations/first", None, &[]),
            200
        )["result"],
        report
    );
    let before = effects(&lab);
    let fresh = descriptor("fresh", 1, 99);
    value(
        lab.handle("POST", "/api/lab/run", Some(&accepted_tag), &body(&fresh)),
        412,
    );
    value(lab.handle("POST", "/api/lab/run", None, &body(&fresh)), 428);
    let old_revision = descriptor("old-revision", 0, 99);
    let old_tag = tag(&lab, "/api/lab/plan", &old_revision);
    value(
        lab.handle("POST", "/api/lab/run", Some(&old_tag), &body(&old_revision)),
        412,
    );
    let conflict = descriptor("first", 1, 99);
    let conflict_tag = tag(&lab, "/api/lab/plan", &conflict);
    value(
        lab.handle(
            "POST",
            "/api/lab/run",
            Some(&conflict_tag),
            &body(&conflict),
        ),
        409,
    );
    assert_eq!(effects(&lab), before);
}

#[test]
fn invalid_descriptors_are_rejected_before_effects() {
    let lab = lab();
    for bytes in [b"{".to_vec(), b"null".to_vec(), b"[]".to_vec()] {
        value(lab.handle("POST", "/api/lab/run", None, &bytes), 400);
    }
    let mut invalid = descriptor("invalid", 0, 1);
    invalid["fault"] = json!("invented_fault");
    value(
        lab.handle("POST", "/api/lab/plan", None, &body(&invalid)),
        400,
    );
    invalid = descriptor("invalid", 0, 1);
    invalid["batch"]["members"][0]["command"]["kind"] = json!("unbounded_command");
    value(
        lab.handle("POST", "/api/lab/plan", None, &body(&invalid)),
        400,
    );
    invalid = descriptor("invalid", 0, 1);
    let member = invalid["batch"]["members"][0].clone();
    invalid["batch"]["members"]
        .as_array_mut()
        .unwrap()
        .push(member);
    value(
        lab.handle("POST", "/api/lab/plan", None, &body(&invalid)),
        400,
    );
    value(
        lab.handle("POST", "/api/lab/run", None, &vec![b' '; 65_537]),
        413,
    );
    assert_eq!(effects(&lab), (json!(0), json!(0), json!(0)));
}

#[test]
fn partial_batch_keeps_successful_members_and_a_verifiable_report() {
    let lab = lab();
    let mut request = descriptor("partial", 0, 3);
    request["batch"]["members"].as_array_mut().unwrap().extend([
        json!({"id":"overflow","command":{"kind":"sum_i64","values":[i64::MAX,1]}}),
        json!({"id":"sum","command":{"kind":"sum_i64","values":[2,4]}}),
    ]);
    let report = run(&lab, &request);
    assert_eq!(report["payload"]["status"], "partial");
    assert_eq!(report["payload"]["members"][0]["result"]["value"], 3);
    assert_eq!(report["payload"]["members"][1]["result"]["kind"], "refused");
    assert_eq!(report["payload"]["members"][2]["result"]["value"], 6);
    assert_eq!(effects(&lab), (json!(3), json!(1), json!(1)));
    assert_eq!(run(&lab, &request), report);
}

#[test]
fn fake_provider_unknown_stages_keep_observations_and_never_retry() {
    let lab = lab();
    let mut known_files = Vec::new();
    for (fault, expected_state, creates) in [
        ("none", "submitted", 1),
        ("unknown_upload", "unknown_upload", 0),
        ("unknown_create", "unknown_create", 1),
    ] {
        let request = json!({"schema":"amqic.etag.lab-provider.v1","fault":fault,"batch":{
            "schema":"amqic.etag.openai-batch.v1","scope":"lab-api","operation_id":fault,
            "endpoint":"/v1/responses","requests":[{"custom_id":"one","body":{"model":"synthetic-model","input":"fixture"}}]}});
        let input = tag(&lab, "/api/lab/provider/plan", &request);
        let first = value(
            lab.handle(
                "POST",
                "/api/lab/provider/run",
                Some(&input),
                &body(&request),
            ),
            200,
        )["result"]
            .clone();
        assert_eq!(
            first["payload"]["submission"]["payload"]["state"],
            expected_state
        );
        assert_eq!(first["payload"]["provider_observation"]["upload_calls"], 1);
        assert_eq!(
            first["payload"]["provider_observation"]["create_calls"],
            creates
        );
        let file = first["payload"]["provider_observation"]["file_id"]
            .as_str()
            .unwrap()
            .to_owned();
        assert!(file.starts_with("file-synthetic-"));
        assert!(!known_files.contains(&file));
        known_files.push(file);
        if creates == 1 {
            assert!(first["payload"]["provider_observation"]["batch_id"]
                .as_str()
                .unwrap()
                .starts_with("batch-synthetic-"));
        }
        let repeated = value(
            lab.handle(
                "POST",
                "/api/lab/provider/run",
                Some(&input),
                &body(&request),
            ),
            200,
        );
        assert_eq!(repeated["result"], first);
        let recovered = value(
            lab.handle(
                "GET",
                &format!("/api/lab/provider/operations/{fault}"),
                None,
                &[],
            ),
            200,
        );
        assert_eq!(recovered["result"], first);
    }
    assert_eq!(effects(&lab), (json!(0), json!(0), json!(0)));
}

#[test]
fn operation_capacity_refuses_new_work_and_preserves_recovery() {
    let lab = lab();
    let first = descriptor("capacity-0", 0, 1);
    let original = run(&lab, &first);
    for revision in 1..128 {
        run(
            &lab,
            &descriptor(&format!("capacity-{revision}"), revision, 1),
        );
    }
    let before = effects(&lab);
    let extra = descriptor("capacity-overflow", 128, 1);
    let input = tag(&lab, "/api/lab/plan", &extra);
    value(
        lab.handle("POST", "/api/lab/run", Some(&input), &body(&extra)),
        503,
    );
    assert_eq!(effects(&lab), before);
    assert_eq!(run(&lab, &first), original);
    assert_eq!(info(&lab)["operations"], 128);
}

/// The fixture owns this listener and serving thread, never a shared service.
struct Server {
    lab: Arc<Lab>,
    address: SocketAddr,
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<std::io::Result<()>>>,
}
impl Server {
    fn start() -> Self {
        eprintln!("FIXTURE launch-intent owner=lab-tests baseline=none disposable=thread/listener cleanup=Server::drop pid={} at={:?}", std::process::id(), SystemTime::now());
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let lab = Arc::new(lab());
        let stop = Arc::new(AtomicBool::new(false));
        let shared = lab.clone();
        let shutdown = stop.clone();
        let worker = thread::spawn(move || lab::serve(listener, shared, shutdown));
        eprintln!(
            "FIXTURE bound pid={} server_thread={:?} actual_address={address}",
            std::process::id(),
            worker.thread().id()
        );
        Self {
            lab,
            address,
            stop,
            worker: Some(worker),
        }
    }
    fn connect(&self) -> TcpStream {
        let socket = TcpStream::connect_timeout(&self.address, Duration::from_secs(1)).unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        socket
            .set_write_timeout(Some(Duration::from_secs(1)))
            .unwrap();
        socket
    }
    fn open(&self, method: &str, path: &str, extra: &str, bytes: &[u8]) -> TcpStream {
        let mut socket = self.connect();
        write!(
            socket,
            "{method} {path} HTTP/1.1\r\nHost: {}\r\nContent-Length: {}\r\n{extra}\r\n",
            self.address,
            bytes.len()
        )
        .unwrap();
        socket.write_all(bytes).unwrap();
        socket
    }
    fn request(&self, method: &str, path: &str, extra: &str, bytes: &[u8]) -> Vec<u8> {
        let mut socket = self.open(method, path, extra, bytes);
        let mut response = Vec::new();
        socket.read_to_end(&mut response).unwrap();
        response
    }
    fn raw(&self, request: &str) -> Vec<u8> {
        let mut socket = self.connect();
        socket.write_all(request.as_bytes()).unwrap();
        let mut response = Vec::new();
        socket.read_to_end(&mut response).unwrap();
        response
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let result = self.worker.take().unwrap().join();
        let joined = matches!(result, Ok(Ok(())));
        let absent = TcpStream::connect_timeout(&self.address, Duration::from_millis(100)).is_err();
        eprintln!(
            "FIXTURE teardown address={} joined={joined} listener_absent={absent}",
            self.address
        );
        if !thread::panicking() {
            assert!(joined && absent, "fixture server failed to stop/join");
        }
    }
}
#[track_caller]
fn wait_for(mut predicate: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(3);
    while !predicate() {
        assert!(Instant::now() < deadline, "bounded state wait expired");
        thread::sleep(Duration::from_millis(2));
    }
}
fn status(response: &[u8]) -> u16 {
    std::str::from_utf8(response)
        .unwrap()
        .split_whitespace()
        .nth(1)
        .unwrap()
        .parse()
        .unwrap()
}
fn wire_json(response: &[u8]) -> Value {
    let delimiter = response.windows(4).position(|w| w == b"\r\n\r\n").unwrap() + 4;
    serde_json::from_slice(&response[delimiter..]).unwrap()
}
fn read_headers(socket: &mut TcpStream) {
    let mut bytes = Vec::new();
    let mut one = [0];
    while !bytes.ends_with(b"\r\n\r\n") {
        socket.read_exact(&mut one).unwrap();
        bytes.push(one[0]);
        assert!(bytes.len() <= 8192);
    }
    assert_eq!(status(&bytes), 200);
}

fn verify_http_etag(response: &[u8]) {
    let delimiter = response.windows(4).position(|w| w == b"\r\n\r\n").unwrap();
    let headers = std::str::from_utf8(&response[..delimiter]).unwrap();
    let validator = headers
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("etag").then(|| value.trim())
        })
        .expect("HTTP representation ETag");
    assert_eq!(
        validator,
        Etag::of_bytes(&response[delimiter + 4..]).as_str()
    );
}

#[test]
fn real_socket_drop_capacity_stream_cursor_and_teardown() {
    let server = Server::start();
    let health = server.request("GET", "/health", "", &[]);
    assert_eq!(status(&health), 200);
    verify_http_etag(&health);
    let before_invalid = effects(&server.lab);
    for (headers, expected) in [
        ("Content-Length: 0\r\nContent-Length: 0\r\n", 400),
        ("Transfer-Encoding: chunked\r\n", 400),
        ("Content-Length: 65537\r\n", 413),
        ("Origin: https://foreign.invalid\r\n", 403),
    ] {
        let raw = format!(
            "POST /api/lab/run HTTP/1.1\r\nHost: {}\r\n{headers}\r\n",
            server.address
        );
        assert_eq!(status(&server.raw(&raw)), expected);
    }
    assert_eq!(
        status(&server.request("GET", "/api/lab/events-unknown", "", &[])),
        404
    );
    assert_eq!(effects(&server.lab), before_invalid);
    let mut request = descriptor("lost-response", 0, 5);
    request["fault"] = json!("drop_after_commit");
    let input = tag(&server.lab, "/api/lab/plan", &request);
    let response = server.request(
        "POST",
        "/api/lab/run",
        &format!("If-Match: {input}\r\n"),
        &body(&request),
    );
    assert!(
        response.is_empty(),
        "commit fault must produce actual EOF without HTTP response"
    );
    assert_eq!(effects(&server.lab), (json!(5), json!(1), json!(1)));
    let recovered = server.request("GET", "/api/lab/operations/lost-response", "", &[]);
    assert_eq!(status(&recovered), 200);
    verify_http_etag(&recovered);
    let recovered = wire_json(&recovered)["result"].clone();
    assert_eq!(run(&server.lab, &request), recovered);

    // Incomplete request headers occupy admitted handlers until explicitly closed.
    wait_for(|| info(&server.lab)["active_connections"] == 0);
    let mut occupied = Vec::new();
    for n in 1..=8 {
        occupied.push(server.connect());
        let deadline = Instant::now() + Duration::from_secs(1);
        loop {
            let observed = info(&server.lab)["active_connections"].clone();
            if observed == n {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "handler admission expected {n}, observed {observed}"
            );
            thread::sleep(Duration::from_millis(2));
        }
    }
    // Overflow is rejected at accept, before parsing any request bytes.
    let mut overflow = Vec::new();
    server.connect().read_to_end(&mut overflow).unwrap();
    assert_eq!(status(&overflow), 503);
    assert_eq!(info(&server.lab)["peak_connections"], 8);
    drop(occupied);
    wait_for(|| info(&server.lab)["active_connections"] == 0);
    assert_eq!(status(&server.request("GET", "/health", "", &[])), 200);

    run(&server.lab, &descriptor("event-two", 1, 1));
    let before = effects(&server.lab);
    let mut streams = Vec::new();
    for n in 1..=2 {
        let mut stream = server.open("GET", "/api/lab/events", "", &[]);
        read_headers(&mut stream);
        streams.push(stream);
        wait_for(|| info(&server.lab)["stream_active"] == n);
    }
    assert_eq!(
        status(&server.request("GET", "/api/lab/events", "", &[])),
        503
    );
    drop(streams);
    wait_for(|| info(&server.lab)["stream_active"] == 0);
    let dropped = server.request("GET", "/api/lab/events?drop_after=1", "", &[]);
    assert_eq!(status(&dropped), 200);
    let dropped = String::from_utf8(dropped).unwrap();
    assert!(dropped.contains("id: 1\n"));
    assert!(!dropped.contains("id: 2\n"));
    let replayed = server.request("GET", "/api/lab/events", "Last-Event-ID: 1\r\n", &[]);
    assert_eq!(status(&replayed), 200);
    let replayed = String::from_utf8(replayed).unwrap();
    assert!(replayed.contains("id: 2\n"));
    assert!(!replayed.contains("id: 1\n"));
    wait_for(|| info(&server.lab)["stream_active"] == 0);
    assert_eq!(
        effects(&server.lab),
        before,
        "SSE reconnect cannot execute a batch"
    );
    assert_eq!(
        status(&server.request("GET", "/api/lab/events", "Last-Event-ID: 999\r\n", &[])),
        400
    );
    for revision in 2..35 {
        run(
            &server.lab,
            &descriptor(&format!("event-{revision}"), revision, 0),
        );
    }
    let before_gap = effects(&server.lab);
    assert_eq!(
        status(&server.request("GET", "/api/lab/events", "Last-Event-ID: 1\r\n", &[])),
        409
    );
    assert_eq!(effects(&server.lab), before_gap);
    wait_for(|| {
        info(&server.lab)["stream_active"] == 0 && info(&server.lab)["active_connections"] == 0
    });
    drop(server); // Explicitly verify the same guard that also runs on panic.
}

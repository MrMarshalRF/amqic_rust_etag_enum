//! Finite consumer of the synthetic loopback APIs. Never contacts a provider.
use etag_for_codex::{
    api, identity, kernel,
    lab::{Fault, LabBatch, ProviderBatch, ProviderFault},
    Etag, Report,
};
use serde_json::{json, Value};
use std::{
    collections::BTreeSet,
    fs::OpenOptions,
    io::{self, Read, Write},
    net::{Shutdown, SocketAddr, TcpStream},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;
struct Response {
    status: u16,
    body: Vec<u8>,
    partial: bool,
}
fn connect(address: SocketAddr) -> Result<TcpStream> {
    let stream = TcpStream::connect_timeout(&address, Duration::from_secs(2))?;
    stream.set_write_timeout(Some(Duration::from_secs(2)))?;
    Ok(stream)
}
fn write_request(
    stream: &mut TcpStream,
    address: SocketAddr,
    method: &str,
    path: &str,
    tag: Option<&str>,
    extra: &str,
    body: &[u8],
) -> Result<()> {
    let condition = tag
        .map(|t| format!("If-Match: {t}\r\n"))
        .unwrap_or_default();
    write!(stream,"{method} {path} HTTP/1.1\r\nHost: {address}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n{condition}{extra}\r\n",body.len())?;
    stream.write_all(body)?;
    Ok(())
}
fn read_response(stream: &mut TcpStream) -> Result<Response> {
    let deadline = Instant::now() + Duration::from_secs(4);
    let mut bytes = Vec::new();
    loop {
        stream.set_read_timeout(Some(
            deadline
                .checked_duration_since(Instant::now())
                .ok_or("client total deadline")?,
        ))?;
        let mut part = [0u8; 4096];
        let n = stream.read(&mut part)?;
        if n == 0 {
            break;
        }
        bytes.extend_from_slice(&part[..n]);
        if bytes.len() > 262_144 {
            return Err("client response bound".into());
        }
    }
    if bytes.is_empty() {
        return Ok(Response {
            status: 0,
            body: Vec::new(),
            partial: true,
        });
    }
    let split = bytes
        .windows(4)
        .position(|s| s == b"\r\n\r\n")
        .ok_or("response headers")?;
    let headers = std::str::from_utf8(&bytes[..split])?;
    let status = headers
        .lines()
        .next()
        .and_then(|s| s.split(' ').nth(1))
        .ok_or("response status")?
        .parse()?;
    let length = headers
        .lines()
        .filter_map(|s| s.split_once(':'))
        .find(|(k, _)| k.eq_ignore_ascii_case("content-length"))
        .and_then(|(_, v)| v.trim().parse::<usize>().ok());
    let body = bytes[split + 4..].to_vec();
    let partial = length.is_some_and(|n| n != body.len());
    Ok(Response {
        status,
        body,
        partial,
    })
}
fn request(
    address: SocketAddr,
    method: &str,
    path: &str,
    tag: Option<&str>,
    extra: &str,
    body: &[u8],
) -> Result<Response> {
    let mut stream = connect(address)?;
    write_request(&mut stream, address, method, path, tag, extra, body)?;
    read_response(&mut stream)
}
fn open_stream(address: SocketAddr, cursor: u64) -> Result<TcpStream> {
    let mut stream = connect(address)?;
    write_request(
        &mut stream,
        address,
        "GET",
        "/api/lab/events",
        None,
        &format!("Last-Event-ID: {cursor}\r\n"),
        b"",
    )?;
    let deadline = Instant::now() + Duration::from_secs(2);
    let mut header = Vec::new();
    while !header.ends_with(b"\r\n\r\n") {
        stream.set_read_timeout(Some(
            deadline
                .checked_duration_since(Instant::now())
                .ok_or("stream header deadline")?,
        ))?;
        let mut byte = [0u8; 1];
        if stream.read(&mut byte)? == 0 {
            return Err("stream closed before headers".into());
        }
        header.push(byte[0]);
        if header.len() > 8192 {
            return Err("stream header bound".into());
        }
    }
    if !header.starts_with(b"HTTP/1.1 200 ") {
        return Err("stream admission failed".into());
    }
    Ok(stream)
}
fn value(response: &Response) -> Result<Value> {
    Ok(serde_json::from_slice(&response.body)?)
}
fn info(address: SocketAddr) -> Result<Value> {
    let r = request(address, "GET", "/api/lab/info", None, "", b"")?;
    if r.status != 200 {
        return Err(
            "lab info unavailable: run suite exclusively on a fresh synthetic stand".into(),
        );
    }
    let info = value(&r)?["result"].clone();
    if info["schema"] != "amqic.etag.lab-info.v1"
        || info["synthetic"] != true
        || info["upstream_calls"] != 0
    {
        return Err(
            "target does not declare the exact synthetic zero-upstream lab contract".into(),
        );
    }
    etag_for_codex::check_sha256(
        info["kernel_sha256"]
            .as_str()
            .ok_or("actual lab kernel pin")?,
    )?;
    Ok(info)
}
fn batch(initial: &Value, id: String, fault: Fault, partial: bool) -> LabBatch {
    let mut members = vec![kernel::Member {
        id: "effect".into(),
        command: kernel::Command::IncrementCounter { amount: 1 },
    }];
    if partial {
        members.push(kernel::Member {
            id: "overflow".into(),
            command: kernel::Command::SumI64 {
                values: vec![i64::MAX, 1],
            },
        });
    }
    LabBatch {
        schema: "amqic.etag.lab.v1".into(),
        fault,
        batch: kernel::Batch {
            schema: "amqic.etag.kernel.v1".into(),
            scope: "lab".into(),
            kernel_sha256: initial["kernel_sha256"].as_str().unwrap_or_default().into(),
            operation_id: id,
            expected_revision: initial["revision"].as_u64().unwrap_or(0),
            members,
        },
    }
}
fn plan(address: SocketAddr, descriptor: &LabBatch) -> Result<(Vec<u8>, String)> {
    let body = serde_json::to_vec(descriptor)?;
    let r = request(address, "POST", "/api/lab/plan", None, "", &body)?;
    let tag = value(&r)?["result"]["input_etag"]
        .as_str()
        .ok_or("plan did not return ETag")?
        .to_owned();
    Ok((body, tag))
}
fn record(rows: &mut Vec<Value>, name: &str, expected: &str, ok: bool, status: u16, detail: Value) {
    println!(
        "{:<26} {:<5} HTTP {:<3} {}",
        name,
        if ok { "PASS" } else { "FAIL" },
        status,
        expected
    );
    rows.push(json!({"scenario":name,"passed":ok,"http_status":status,"expected":expected,"observed":detail}));
}
fn ids(bytes: &[u8]) -> Vec<u64> {
    String::from_utf8_lossy(bytes)
        .lines()
        .filter_map(|l| l.strip_prefix("id: ")?.parse().ok())
        .collect()
}
fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let address: SocketAddr = args
        .next()
        .ok_or("usage: lab-scenarios 127.0.0.1:PORT [--report NEW.json]")?
        .parse()?;
    if address.ip() != std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST) || address.port() == 0 {
        return Err("only exact synthetic127.0.0.1:nonzero port allowed".into());
    }
    let mut output = None;
    while let Some(arg) = args.next() {
        if arg != "--report" || output.is_some() {
            return Err("unknown/duplicate option".into());
        }
        let path = args.next().ok_or("report path")?;
        output = Some((
            path.clone(),
            OpenOptions::new().write(true).create_new(true).open(path)?,
        ));
    }
    let initial = info(address)?;
    if initial["max_operations"]
        .as_u64()
        .unwrap_or(0)
        .saturating_sub(initial["operations"].as_u64().unwrap_or(0))
        < 10
    {
        return Err("insufficient retained operation capacity; use a fresh stand after preserving prior reports".into());
    }
    let run_id = SystemTime::now()
        .duration_since(UNIX_EPOCH)?
        .as_nanos()
        .to_string();
    let input = identity(&(
        "amqic.etag.lab-run.v1",
        &initial,
        &run_id,
        "finite-scenarios.v1",
    ))?;
    if let Some((_, file)) = &mut output {
        writeln!(
            file,
            "{}",
            json!({"state":"intent","input_etag":input,"run_id":run_id,"synthetic":true})
        )?;
        file.sync_all()?;
    }
    println!("ETag / capacity / connection lab — SYNTHETIC, process-local");
    println!("Target {address}; no upstream calls. Each PASS is an observed bounded oracle.\n");
    let mut rows = Vec::new();
    let descriptor = batch(&initial, format!("ok-{run_id}"), Fault::None, false);
    let (body, tag) = plan(address, &descriptor)?;
    let r = request(address, "POST", "/api/lab/run", Some(&tag), "", &body)?;
    let first = value(&r)?;
    let report: Report<kernel::Output> = serde_json::from_value(first["result"].clone())?;
    report.verify(&Etag::parse(&tag)?)?;
    record(
        &mut rows,
        "success",
        "effect committed once",
        r.status == 200 && report.payload.counter == initial["counter"].as_i64().unwrap_or(0) + 1,
        r.status,
        json!({"input_etag":tag,"etag":report.etag,"revision":report.payload.revision}),
    );
    let before = info(address)?;
    let replay = request(address, "POST", "/api/lab/run", Some(&tag), "", &body)?;
    let after = info(address)?;
    record(
        &mut rows,
        "replay",
        "same report, no new effect",
        value(&replay)? == first && before["executions"] == after["executions"],
        replay.status,
        json!({"executions_before":before["executions"],"executions_after":after["executions"]}),
    );
    let mut conflict = descriptor.clone();
    conflict.batch.members[0].command = kernel::Command::IncrementCounter { amount: 2 };
    let (cb, ct) = plan(address, &conflict)?;
    let r = request(address, "POST", "/api/lab/run", Some(&ct), "", &cb)?;
    record(
        &mut rows,
        "operation_conflict",
        "changed same-op refused",
        r.status == 409,
        r.status,
        value(&r)?,
    );
    let fresh = batch(&after, format!("stale-{run_id}"), Fault::None, false);
    let (fb, ft) = plan(address, &fresh)?;
    let r = request(
        address,
        "POST",
        "/api/lab/run",
        Some(Etag::of_bytes(b"stale").as_str()),
        "",
        &fb,
    )?;
    let end = info(address)?;
    record(
        &mut rows,
        "stale_etag",
        "412 and zero new effects",
        r.status == 412 && end["executions"] == after["executions"],
        r.status,
        json!({"executions":end["executions"],"accepted_tag":ft}),
    );
    let r = request(address, "POST", "/api/lab/run", None, "", &fb)?;
    record(
        &mut rows,
        "missing_if_match",
        "428 before effect",
        r.status == 428,
        r.status,
        value(&r)?,
    );
    let r = request(address, "POST", "/api/lab/run", Some(&ft), "", b"{garbage")?;
    record(
        &mut rows,
        "malformed_json",
        "400, service survives",
        r.status == 400,
        r.status,
        value(&r)?,
    );
    let mut invalid = serde_json::to_value(&fresh)?;
    invalid["batch"]["members"][0]["command"]["kind"] = json!("arbitrary_exec");
    let r = request(
        address,
        "POST",
        "/api/lab/run",
        Some(&ft),
        "",
        &serde_json::to_vec(&invalid)?,
    )?;
    record(
        &mut rows,
        "unknown_enum",
        "400 before dispatch",
        r.status == 400,
        r.status,
        value(&r)?,
    );
    let mut large = connect(address)?;
    write!(
        large,
        "POST /api/lab/run HTTP/1.1\r\nHost: {address}\r\nContent-Length: 65537\r\n\r\n"
    )?;
    let r = read_response(&mut large)?;
    record(
        &mut rows,
        "oversized_body",
        "413 before body allocation",
        r.status == 413,
        r.status,
        value(&r)?,
    );
    let snapshot = info(address)?;
    let partial = batch(&snapshot, format!("partial-{run_id}"), Fault::None, true);
    let (pb, pt) = plan(address, &partial)?;
    let r = request(address, "POST", "/api/lab/run", Some(&pt), "", &pb)?;
    let p = value(&r)?;
    record(
        &mut rows,
        "partial_batch",
        "success retained + refusal",
        p["result"]["payload"]["status"] == "partial",
        r.status,
        p,
    );
    let snapshot = info(address)?;
    let drop = batch(
        &snapshot,
        format!("drop-{run_id}"),
        Fault::DropAfterCommit,
        false,
    );
    let (db, dt) = plan(address, &drop)?;
    let lost = request(address, "POST", "/api/lab/run", Some(&dt), "", &db)?;
    let recovered = request(
        address,
        "GET",
        &format!("/api/lab/operations/{}", drop.batch.operation_id),
        None,
        "",
        b"",
    )?;
    let current = info(address)?;
    let rr: Report<kernel::Output> = serde_json::from_value(value(&recovered)?["result"].clone())?;
    rr.verify(&Etag::parse(&dt)?)?;
    record(
        &mut rows,
        "disconnect_after_commit",
        "lost response; GET recovers once",
        lost.status == 0
            && recovered.status == 200
            && current["executions"].as_u64() == snapshot["executions"].as_u64().map(|n| n + 1),
        recovered.status,
        json!({"send_status":lost.status,"recovered_etag":rr.etag,"executions":current["executions"]}),
    );
    for (label, fault) in [
        ("provider_unknown_upload", ProviderFault::UnknownUpload),
        ("provider_unknown_create", ProviderFault::UnknownCreate),
    ] {
        let d = ProviderBatch {
            schema: "amqic.etag.lab-provider.v1".into(),
            fault,
            batch: api::Batch {
                schema: "amqic.etag.openai-batch.v1".into(),
                scope: "lab-api".into(),
                operation_id: format!("{label}-{run_id}"),
                endpoint: api::Endpoint::Responses,
                requests: vec![api::Request {
                    custom_id: "one".into(),
                    body: json!({"model":"synthetic-only","input":"hello"}),
                }],
            },
        };
        let bytes = serde_json::to_vec(&d)?;
        let pr = request(address, "POST", "/api/lab/provider/plan", None, "", &bytes)?;
        let tag = value(&pr)?["result"]["input_etag"]
            .as_str()
            .ok_or("provider tag")?
            .to_owned();
        let a_response = request(
            address,
            "POST",
            "/api/lab/provider/run",
            Some(&tag),
            "",
            &bytes,
        )?;
        let b_response = request(
            address,
            "POST",
            "/api/lab/provider/run",
            Some(&tag),
            "",
            &bytes,
        )?;
        let a = value(&a_response)?;
        let b = value(&b_response)?;
        let report: Report<Value> = serde_json::from_value(a["result"].clone())?;
        report.verify(&Etag::parse(&tag)?)?;
        let observation = &report.payload["provider_observation"];
        let state = if fault == ProviderFault::UnknownCreate {
            "unknown_create"
        } else {
            "unknown_upload"
        };
        record(
            &mut rows,
            label,
            "known fake commits, no retry",
            a_response.status == 200
                && b_response.status == 200
                && a == b
                && report.payload["submission"]["payload"]["state"] == state
                && observation["upload_calls"] == 1
                && observation["create_calls"]
                    == if fault == ProviderFault::UnknownCreate {
                        1
                    } else {
                        0
                    },
            a_response.status,
            a,
        );
    }
    let before = info(address)?;
    let mut held = Vec::new();
    for _ in 0..8 {
        held.push(connect(address)?);
    }
    // Coordination delay is not the oracle: actual503/peak/zero-effect checks are.
    thread::sleep(Duration::from_millis(25));
    let mut ninth = connect(address)?;
    let overflow = read_response(&mut ninth)?;
    let overloaded = overflow.status == 503;
    for stream in held {
        let _ = stream.shutdown(Shutdown::Both);
    }
    let deadline = Instant::now() + Duration::from_secs(3);
    let after = loop {
        if let Ok(state) = info(address) {
            if state["active_connections"].as_u64().unwrap_or(99) <= 1 {
                break state;
            }
        }
        if Instant::now() > deadline {
            return Err("capacity did not release".into());
        }
        thread::sleep(Duration::from_millis(10));
    };
    record(
        &mut rows,
        "connection_capacity",
        "503, <=8 handlers, slots return",
        overloaded
            && after["peak_connections"].as_u64().unwrap_or(99) <= 8
            && after["executions"] == before["executions"],
        if overloaded { 503 } else { 200 },
        after,
    );
    let before = info(address)?;
    let stream = request(
        address,
        "GET",
        "/api/lab/events?drop_after=1",
        None,
        "",
        b"",
    )?;
    let first_ids = ids(&stream.body);
    let last = *first_ids.last().ok_or("SSE fixture needs prior events")?;
    let resumed = request(
        address,
        "GET",
        "/api/lab/events",
        None,
        &format!("Last-Event-ID: {last}\r\n"),
        b"",
    )?;
    let remaining = ids(&resumed.body);
    let after = info(address)?;
    let unique: BTreeSet<_> = remaining.iter().copied().collect();
    record(
        &mut rows,
        "stream_cursor_reconnect",
        "partial snapshot resumes, no effects",
        stream.partial
            && !remaining.is_empty()
            && remaining.iter().all(|n| *n > last)
            && unique.len() == remaining.len()
            && before["executions"] == after["executions"],
        resumed.status,
        json!({"last_complete_id":last,"replayed_ids":remaining,"stream_active":after["stream_active"],"executions":after["executions"]}),
    );
    let before = info(address)?;
    let cursor = before["event_id"].as_u64().ok_or("event counter")?;
    let one = open_stream(address, cursor)?;
    let two = open_stream(address, cursor)?;
    let third = request(address, "GET", "/api/lab/events", None, "", b"")?;
    let _ = one.shutdown(Shutdown::Both);
    let _ = two.shutdown(Shutdown::Both);
    let deadline = Instant::now() + Duration::from_secs(2);
    let after = loop {
        let state = info(address)?;
        if state["stream_active"] == 0 {
            break state;
        }
        if Instant::now() > deadline {
            return Err("stream slots did not return".into());
        }
        thread::sleep(Duration::from_millis(10));
    };
    record(
        &mut rows,
        "stream_capacity",
        "third503; both slots released",
        third.status == 503 && after["executions"] == before["executions"],
        third.status,
        json!({"stream_active":after["stream_active"],"rejected_streams":after["rejected_streams"]}),
    );
    let failed = rows.iter().filter(|r| r["passed"] != true).count();
    let final_state = info(address)?;
    let report = Report::new(
        input,
        json!({"schema":"amqic.etag.lab-run.v1","synthetic":true,"run_id":run_id,"target":address.to_string(),"cases":rows,
        "failed":failed,"final_state":final_state,"upstream_calls":0,"limits":"same-process synthetic lab; no restart durability, unlimited load or provider guarantee"}),
    )?;
    if let Some((path, mut file)) = output {
        file.set_len(0)?;
        use std::io::Seek;
        file.rewind()?;
        serde_json::to_writer_pretty(&mut file, &report)?;
        file.sync_all()?;
        println!("\nReport: {path}");
    }
    println!("Report ETag: {}\nFailed scenarios: {failed}", report.etag);
    if failed > 0 {
        return Err(io::Error::other(
            "scenario oracle failed; preserve report and original operation IDs",
        )
        .into());
    }
    Ok(())
}

use etag_for_codex::{kernel, Etag};
use serde_json::{json, Value};
use std::{
    io::Write,
    process::{Command, Stdio},
};
fn run(args: &[&str], requests: &[Value]) -> Vec<Value> {
    let mut child = Command::new(env!("CARGO_BIN_EXE_etag-for-codex"))
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    {
        let mut stdin = child.stdin.take().unwrap();
        for request in requests {
            writeln!(stdin, "{request}").unwrap();
        }
    }
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}
#[test]
fn stale_scopes_do_not_consume_capacity_and_serve_recovers() {
    let executable = std::fs::read(env!("CARGO_BIN_EXE_etag-for-codex")).unwrap();
    let tag = Etag::of_bytes(&executable);
    let pin = tag.as_str()[8..72].to_owned();
    let mut requests = Vec::new();
    for i in 0..32 {
        requests.push(json!({"protocol":"etag-for-codex.v1","action":{"kind":"kernel_run","expected_etag":Etag::of_bytes(b"stale"),
        "batch":{"schema":"amqic.etag.kernel.v1","scope":format!("bad-{i}"),"kernel_sha256":pin,"operation_id":"op","expected_revision":0,
            "members":[{"id":"a","command":{"kind":"increment_counter","amount":1}}]}}}));
    }
    for i in 0..32 {
        let batch = kernel::Batch {
            schema: "amqic.etag.kernel.v1".into(),
            scope: format!("bad-revision-{i}"),
            kernel_sha256: pin.clone(),
            operation_id: "op".into(),
            expected_revision: 1,
            members: vec![kernel::Member {
                id: "a".into(),
                command: kernel::Command::IncrementCounter { amount: 1 },
            }],
        };
        requests.push(json!({"protocol":"etag-for-codex.v1","action":{"kind":"kernel_run","expected_etag":batch.etag().unwrap(),"batch":batch}}));
    }
    let batch = kernel::Batch {
        schema: "amqic.etag.kernel.v1".into(),
        scope: "valid".into(),
        kernel_sha256: pin,
        operation_id: "op".into(),
        expected_revision: 0,
        members: vec![kernel::Member {
            id: "a".into(),
            command: kernel::Command::IncrementCounter { amount: 1 },
        }],
    };
    let request = json!({"protocol":"etag-for-codex.v1","action":{"kind":"kernel_run","expected_etag":batch.etag().unwrap(),"batch":batch}});
    requests.push(request.clone());
    requests.push(request);
    let output = run(&["serve"], &requests);
    assert!(output[..64].iter().all(|v| v["http_status"] == 412));
    assert_eq!(output[64]["result"]["payload"]["counter"], 1);
    assert_eq!(output[64], output[65]);
}
#[test]
fn mcp_handshake_discovery_tool_call_and_invalid_enum() {
    let output = run(
        &["mcp"],
        &[
            json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-03-26","capabilities":{},"clientInfo":{"name":"test","version":"1"}}}),
            json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
            json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}),
            json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"etag_batch","arguments":{"protocol":"etag-for-codex.v1","action":{"kind":"kernel_info"}}}}),
            json!({"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"etag_batch","arguments":{"protocol":"etag-for-codex.v1","action":{"kind":"arbitrary_exec"}}}}),
            json!({"jsonrpc":"2.0","id":{},"method":"tools/call","params":{"name":"etag_batch","arguments":{"protocol":"etag-for-codex.v1","action":{"kind":"kernel_info"}}}}),
            json!([42, {"jsonrpc":"2.0","method":"notifications/initialized"}]),
        ],
    );
    assert_eq!(output.len(), 6);
    assert_eq!(output[0]["result"]["protocolVersion"], "2025-03-26");
    assert_eq!(output[1]["result"]["tools"][0]["name"], "etag_batch");
    assert_eq!(output[2]["result"]["isError"], false);
    assert_eq!(output[3]["result"]["isError"], true);
    assert_eq!(output[4]["error"]["code"], -32600);
    assert_eq!(output[4]["id"], Value::Null);
    assert_eq!(output[5][0]["error"]["code"], -32600);
}

use etag_for_codex::{api, kernel, Error, Etag, MAX_BYTES};
use serde::Deserialize;
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    io::{self, BufRead, Read, Write},
};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    protocol: String,
    action: Action,
}
#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum Action {
    KernelInfo,
    KernelPlan {
        batch: kernel::Batch,
    },
    KernelRun {
        batch: kernel::Batch,
        expected_etag: String,
    },
    ApiPlan {
        batch: api::Batch,
    },
    ApiExport {
        batch: api::Batch,
        expected_etag: String,
    },
    ApiReconcile {
        batch: api::Batch,
        expected_etag: String,
        result_jsonl: String,
    },
    VerifyResult {
        input_etag: Etag,
        report: etag_for_codex::Report<Value>,
    },
    ToolSchema,
}
fn tool_schema() -> Value {
    fn object(properties: Value, required: Value) -> Value {
        json!({"type":"object","properties":properties,"required":required,"additionalProperties":false})
    }
    let id = json!({"type":"string","minLength":1,"maxLength":128});
    let etag = json!({"type":"string","pattern":"^\"sha256-[0-9a-f]{64}\"$"});
    let kernel_command = json!({"oneOf":[
        object(json!({"kind":{"const":"sha256"},"text":{"type":"string"}}),json!(["kind","text"])),
        object(json!({"kind":{"const":"sum_i64"},"values":{"type":"array","items":{"type":"integer"}}}),json!(["kind","values"])),
        object(json!({"kind":{"const":"increment_counter"},"amount":{"type":"integer"}}),json!(["kind","amount"]))]});
    let kernel_batch = object(
        json!({"schema":{"const":"amqic.etag.kernel.v1"},"scope":id,"operation_id":id,
        "kernel_sha256":{"type":"string","pattern":"^[0-9a-f]{64}$"},"expected_revision":{"type":"integer","minimum":0},
        "members":{"type":"array","minItems":1,"maxItems":1024,"items":object(json!({"id":id,"command":kernel_command}),json!(["id","command"]))}}),
        json!([
            "schema",
            "scope",
            "operation_id",
            "kernel_sha256",
            "expected_revision",
            "members"
        ]),
    );
    let api_batch = object(
        json!({"schema":{"const":"amqic.etag.openai-batch.v1"},"scope":id,"operation_id":id,
        "endpoint":{"enum":["/v1/responses","/v1/chat/completions","/v1/embeddings"]},
        "requests":{"type":"array","minItems":1,"maxItems":1024,"items":object(json!({"custom_id":id,
            "body":{"type":"object","properties":{"model":{"type":"string"}},"required":["model"],"additionalProperties":true}}),json!(["custom_id","body"]))}}),
        json!(["schema", "scope", "operation_id", "endpoint", "requests"]),
    );
    let report = object(
        json!({"schema":{"const":"amqic.etag.result.v1"},"input_etag":etag,"etag":etag,"payload":{}}),
        json!(["schema", "input_etag", "etag", "payload"]),
    );
    let actions = json!({"oneOf":[
        object(json!({"kind":{"const":"kernel_info"}}),json!(["kind"])),
        object(json!({"kind":{"const":"kernel_plan"},"batch":kernel_batch}),json!(["kind","batch"])),
        object(json!({"kind":{"const":"kernel_run"},"batch":kernel_batch,"expected_etag":etag}),json!(["kind","batch","expected_etag"])),
        object(json!({"kind":{"const":"api_plan"},"batch":api_batch}),json!(["kind","batch"])),
        object(json!({"kind":{"const":"api_export"},"batch":api_batch,"expected_etag":etag}),json!(["kind","batch","expected_etag"])),
        object(json!({"kind":{"const":"api_reconcile"},"batch":api_batch,"expected_etag":etag,"result_jsonl":{"type":"string"}}),json!(["kind","batch","expected_etag","result_jsonl"])),
        object(json!({"kind":{"const":"verify_result"},"input_etag":etag,"report":report}),json!(["kind","input_etag","report"])),
        object(json!({"kind":{"const":"tool_schema"}}),json!(["kind"]))]});
    json!({"type":"function", "name":"etag_batch", "description":"Plan and execute versioned Rust batches. Obtain a plan ETag and reuse exactly that expected_etag; operation IDs are stable. API export is local and never submits network requests.",
        "parameters":{"type":"object", "properties":{"protocol":{"type":"string","enum":["etag-for-codex.v1"]},
            "action":actions},
            "required":["protocol","action"],"additionalProperties":false}})
}
struct Host {
    kernel_sha256: String,
    sessions: BTreeMap<String, kernel::Session>,
    journal: Option<etag_for_codex::journal::Journal>,
}
impl Host {
    fn dispatch(&mut self, value: Value) -> etag_for_codex::Result<Value> {
        let request: Request = serde_json::from_value(value)
            .map_err(|_| Error::invalid("unknown/malformed command or fields"))?;
        if request.protocol != "etag-for-codex.v1" {
            return Err(Error::invalid("unknown protocol version"));
        }
        match request.action {
            Action::KernelInfo => Ok(
                json!({"protocol":"etag-for-codex.v1", "kernel_sha256":self.kernel_sha256,
                "state":if self.journal.is_some(){"durable"}else{"process-local"},"journal_enabled":self.journal.is_some(), "scope_authorization":"host-owned"}),
            ),
            Action::KernelPlan { batch } => {
                Ok(json!({"input_etag":batch.etag()?, "members":batch.members.len()}))
            }
            Action::KernelRun {
                batch,
                expected_etag,
            } => {
                batch.etag()?.require(Some(&expected_etag))?;
                if batch.kernel_sha256 != self.kernel_sha256 {
                    return Err(Error::StaleSnapshot);
                }
                if let Some(session) = self.sessions.get(&batch.scope) {
                    return serde_json::to_value(session.execute(&batch, Some(&expected_etag))?)
                        .map_err(|_| Error::Internal);
                }
                if self.sessions.len() >= 32 {
                    return Err(Error::invalid("scope capacity"));
                }
                let session = match &self.journal {
                    Some(journal) => kernel::Session::with_journal(
                        batch.scope.clone(),
                        self.kernel_sha256.clone(),
                        journal.clone(),
                    )?,
                    None => kernel::Session::new(batch.scope.clone(), self.kernel_sha256.clone())?,
                };
                // Load persisted revision first; cache a scope only after accepted execution/recovery.
                let report = session.execute(&batch, Some(&expected_etag))?;
                self.sessions.insert(batch.scope.clone(), session);
                serde_json::to_value(report).map_err(|_| Error::Internal)
            }
            Action::ApiPlan { batch } => Ok(batch.prepare()?.manifest()),
            Action::ApiExport {
                batch,
                expected_etag,
            } => {
                let plan = batch.prepare()?;
                plan.etag().require(Some(&expected_etag))?;
                Ok(
                    json!({"manifest":plan.manifest(), "jsonl":std::str::from_utf8(plan.jsonl()).map_err(|_| Error::Internal)?}),
                )
            }
            Action::ApiReconcile {
                batch,
                expected_etag,
                result_jsonl,
            } => serde_json::to_value(
                batch
                    .prepare()?
                    .reconcile(Some(&expected_etag), result_jsonl.as_bytes())?,
            )
            .map_err(|_| Error::Internal),
            Action::VerifyResult { input_etag, report } => {
                report.verify(&input_etag)?;
                Ok(json!({"verified":true,"etag":report.etag}))
            }
            Action::ToolSchema => Ok(tool_schema()),
        }
    }
}
fn envelope(result: etag_for_codex::Result<Value>) -> Value {
    match result {
        Ok(value) => json!({"ok":true,"result":value}),
        Err(error) => json!({"ok":false,"http_status":error.http_status(),"result":error}),
    }
}
fn mcp(host: &mut Host, v: Value) -> Option<Value> {
    if let Value::Array(messages) = v {
        if messages.is_empty() {
            return Some(
                json!({"jsonrpc":"2.0","id":null,"error":{"code":-32600,"message":"Empty batch"}}),
            );
        }
        let replies: Vec<_> = messages.into_iter().filter_map(|m| mcp(host, m)).collect();
        return if replies.is_empty() {
            None
        } else {
            Some(Value::Array(replies))
        };
    }
    let valid_id = v
        .get("id")
        .is_none_or(|id| id.is_string() || id.as_i64().is_some() || id.as_u64().is_some());
    let id = if valid_id {
        v.get("id").cloned().unwrap_or(Value::Null)
    } else {
        Value::Null
    };
    if !v.is_object()
        || v.get("jsonrpc") != Some(&json!("2.0"))
        || !valid_id
        || v.get("method").and_then(Value::as_str).is_none()
    {
        return Some(
            json!({"jsonrpc":"2.0","id":id,"error":{"code":-32600,"message":"Invalid request"}}),
        );
    }
    v.get("id")?; // valid notifications never invoke tools
    if v.get("params").is_some_and(|params| !params.is_object()) {
        return Some(
            json!({"jsonrpc":"2.0","id":id,"error":{"code":-32602,"message":"Expected object params"}}),
        );
    }
    let result = match v.get("method").and_then(Value::as_str) {
        Some("initialize") => Ok(
            json!({"protocolVersion":"2025-03-26", "capabilities":{"tools":{}},
            "serverInfo":{"name":"etag-for-codex","version":env!("CARGO_PKG_VERSION")}}),
        ),
        Some("ping") => Ok(json!({})),
        Some("tools/list") => {
            let s = tool_schema();
            Ok(
                json!({"tools":[{"name":s["name"],"description":s["description"],"inputSchema":s["parameters"]}]}),
            )
        }
        Some("tools/call")
            if v.pointer("/params/name").and_then(Value::as_str) == Some("etag_batch") =>
        {
            let output = envelope(
                host.dispatch(
                    v.pointer("/params/arguments")
                        .cloned()
                        .unwrap_or(Value::Null),
                ),
            );
            Ok(
                json!({"content":[{"type":"text","text":output.to_string()}],"isError":output["ok"]==false}),
            )
        }
        _ => Err(json!({"code":-32601,"message":"Unknown method/tool"})),
    };
    Some(match result {
        Ok(result) => json!({"jsonrpc":"2.0","id":id,"result":result}),
        Err(error) => json!({"jsonrpc":"2.0","id":id,"error":error}),
    })
}
fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--help") {
        println!("etag-for-codex [serve|mcp] [--journal PATH]\nDefault: one bounded JSON request on stdin. Optional SQLite journal persists kernel state and completed batches across same-binary restarts. mcp: stdio MCP2025-03-26. No network submission from CLI.");
        return Ok(());
    }
    let mut mode = None;
    let mut journal_path = None;
    let mut arguments = args.into_iter();
    while let Some(arg) = arguments.next() {
        match arg.as_str() {
            "serve" | "mcp" if mode.is_none() => mode = Some(arg),
            "--journal" if journal_path.is_none() => {
                journal_path = Some(arguments.next().ok_or("missing journal path")?)
            }
            _ => return Err("unknown/duplicate CLI argument".into()),
        }
    }
    let executable = std::fs::read(std::env::current_exe()?)?;
    let tag = Etag::of_bytes(&executable);
    let journal = journal_path
        .map(etag_for_codex::journal::Journal::open)
        .transpose()?;
    let mut host = Host {
        kernel_sha256: tag.as_str()[8..72].to_owned(),
        sessions: BTreeMap::new(),
        journal,
    };
    let mut input = io::stdin().lock();
    let mut output = io::stdout().lock();
    loop {
        let mut bytes = Vec::new();
        let n = if mode.is_none() {
            input
                .by_ref()
                .take((MAX_BYTES + 1) as u64)
                .read_to_end(&mut bytes)?
        } else {
            input
                .by_ref()
                .take((MAX_BYTES + 1) as u64)
                .read_until(b'\n', &mut bytes)?
        };
        if n == 0 {
            break;
        }
        if bytes.len() > MAX_BYTES {
            writeln!(
                output,
                "{}",
                envelope(Err(Error::invalid("request byte limit")))
            )?;
            output.flush()?;
            std::process::exit(1);
        }
        let value = serde_json::from_slice::<Value>(&bytes);
        let response = if mode.as_deref() == Some("mcp") {
            match value {
                Ok(v) => mcp(&mut host, v),
                Err(_) => Some(
                    json!({"jsonrpc":"2.0","id":null,"error":{"code":-32700,"message":"Parse error"}}),
                ),
            }
        } else {
            Some(envelope(
                value
                    .map_err(|_| Error::invalid("request JSON"))
                    .and_then(|v| host.dispatch(v)),
            ))
        };
        if let Some(response) = response {
            writeln!(output, "{response}")?;
            output.flush()?;
            if mode.is_none() && response.get("ok") == Some(&json!(false)) {
                std::process::exit(1);
            }
        }
        if mode.is_none() {
            break;
        }
    }
    Ok(())
}

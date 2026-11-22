# Protocol v1

## Encoding and identity

`Etag` is exactly `"sha256-<64 lowercase hex>"`, including the quotes. `W/`, wildcard,
lists and alternate digests are rejected. SHA-256 is unkeyed identity, not authorization.

`canonical` recursively sorts JSON object keys lexicographically by their UTF-8 strings,
retains array order, emits no whitespace and uses serde_json scalar encoding. Numbers
`1` and `1.0` may differ; Unicode is not normalized. This is a private versioned JSON
encoding, not RFC 8785. Cross-language clients must use the exact published golden
vectors or retain the tag from this producer instead of inventing another encoder.

Input tags cover the complete selected descriptor. API plan identity additionally
covers exact JSONL file ETag, fixed `24h` creation window and `custom_id-reconcile.v1`
output contract. Authentication headers are not descriptor fields. Never place API
keys/passwords in bodies, metadata, manifests or reports.

Result ETag hashes canonical `["amqic.etag.result.v1", input_etag, payload]`. Its own
`etag` field is excluded. `Report::verify` recomputes it and checks the expected input.
Use this validator only for the canonical result representation; do not advertise the
same strong HTTP ETag for a separately formatted/redacted representation.

## CLI request envelope

```json
{"protocol":"etag-for-codex.v1","action":{"kind":"kernel_info"}}
```

| `action.kind` | Required fields | Effect |
|---|---|---|
| `kernel_info` | none | Actual CLI executable SHA-256 |
| `kernel_plan` | `batch` | Validate/hash descriptor |
| `kernel_run` | `batch`, `expected_etag` | Execute closed synthetic kernel in process-local session |
| `api_plan` | `batch` | Manifest with input/file ETags, count and bytes |
| `api_export` | `batch`, `expected_etag` | Manifest and provider JSONL string; no network |
| `api_reconcile` | `batch`, `expected_etag`, `result_jsonl` | Match output/error records by ID |
| `verify_result` | `input_etag`, `report` | Recompute result validator |
| `tool_schema` | none | Responses function schema, also used by MCP discovery |

Normal CLI output is `{"ok":true,"result":...}` or
`{"ok":false,"http_status":412,"result":{"error":"stale_snapshot"}}`.
One-shot errors exit 1; `serve` returns an error line and continues. Input parsing is
bounded at 8 MiB; each persistent-mode request occupies one UTF-8 JSON line.

## Kernel batch

```json
{
  "schema":"amqic.etag.kernel.v1",
  "scope":"demo",
  "kernel_sha256":"ACTUAL_64_LOWERCASE_HEX",
  "operation_id":"sum-001",
  "expected_revision":0,
  "members":[{"id":"sum","command":{"kind":"sum_i64","values":[1,2,3]}}]
}
```

Commands are `sha256{text}`, `sum_i64{values}` and `increment_counter{amount}`.
Unknown command tags and fields reject. IDs are 1–128 bytes without control characters;
member IDs are unique. Array order is identity. Integer overflow is a typed member
refusal and does not wrap.

The session is bound to one scope and independently supplied actual kernel pin. The
mutex covers expected tag, actual revision, operation ledger and counter changes. An
identical completed operation is recovered before current revision comparison; this
is intentional recovery, not a second admission. Changed input under the same operation
ID refuses. Every newly admitted batch advances revision once, including an all-refused
batch, because its operation/result was recorded. There is no rollback of successful
members when a later member refuses.

Memory constructors do not persist across crashes. The optional SQLite journal and
`Session::with_journal` commit synthetic state/revision/report together; `--journal PATH`
enables that mode in the CLI/MCP. There is no generic callback worker or cancellation
mechanism. A poisoned lock refuses calls; it does not infer an external outcome.

## API batch and submission

See `fixtures/api-batch.json`. The API descriptor is schema/scope/operation_id/endpoint/
requests. Only `custom_id`, `method:"POST"`, `url`, `body` are exported per JSONL line.
The requests require one endpoint and model, unique custom IDs and object bodies.
Full endpoint body/model/account validation remains with OpenAI.

`Plan` is constructed through `Batch::prepare`, not deserialized unchecked. Exact
upload bytes are immutable. `ApiSession` is a single shared owner per scope; its mutex
binds operation ID, input tag and lifecycle:

```text
prepared -> uploaded{file_id} -> submitted{file_id,batch_id}
         -> rejected_upload{code}
         -> unknown_upload
uploaded -> rejected_create{file_id,code}
         -> unknown_create{file_id}
```

Each `advance` call performs at most one upload or create. Submitted, rejected and
unknown states are terminal for automated sending. Another Plan clone with the same
operation ID recovers/progresses the same session entry; a different tag conflicts.
`ApiSession::new` is process-local. `ApiSession::with_journal` commits an Unknown
intent before entering the external callback and records its outcome in a separate
transaction. Reopen preserves Uploaded/Submitted/Unknown and never blindly replays
uncertain effects. Exporting a report afterwards cannot supply crash atomicity.

`TransportOutcome::Rejected` means the transport has proven zero effect; custom
transports must not classify ambiguous timeouts/5xx/malformed responses that way. The
included HTTPS transport conservatively returns unknown for all non-2xx/send/parse
failures. It does not automatically retry even a definite rejection.

## Results

Combine the actual output and error JSONL contents, retaining all records. Reconcile
by custom ID, reject foreign/duplicate IDs, preserve explicit error/non-2xx rows as
failed and absent rows as unknown. Result order follows original request order.
Malformed rows refuse the report; a missing record is never silently converted to
success or automatically resubmitted. Both all-failed and missing-member reports use
the conservative `partial` aggregate, with precise member states.

An HTTP response status 2xx is a completed transport record, not a claim that model
text is correct, safe, or free of refusal. Inspect the nested provider body separately.

## MCP subset

Stdio JSON-RPC 2.0 with newline framing; negotiate protocol `2025-03-26`. Supported
requests: `initialize`, `ping`, `tools/list`, `tools/call` for `etag_batch`. JSON-RPC
batches are handled. Notifications produce no tool effects. Unknown methods and
malformed request identities/parameters refuse before dispatch. Tool schema declares
the closed action variants; native serde parsing remains the executable validator.
This is tools-only stdio, without Streamable HTTP, resources, prompts or task queues.

# Typed commands, results and recovery states


Rust enums define the operations the executor understands and the outcomes a caller must handle. Each command carries its own typed parameters; callers cannot substitute an arbitrary command name or attach unrelated fields. An exhaustive Rust `match` makes a newly added variant visible to code that handles these enums. Serde converts the same closed types to JSON at the CLI, MCP and journal boundaries.

The [kernel types](../src/kernel.rs) have these command and result variants:

| Rust command | JSON command | Successful `MemberResult` | Meaning |
|---|---|---|---|
| `Command::Sha256 { text }` | `{"kind":"sha256","text":"hello"}` | `Sha256 { etag }` | Hashes the UTF-8 bytes of `text` and returns a strong ETag |
| `Command::SumI64 { values }` | `{"kind":"sum_i64","values":[10,20,30]}` | `SumI64 { value: 60 }` | Adds signed 64-bit integers with checked arithmetic |
| `Command::IncrementCounter { amount }` | `{"kind":"increment_counter","amount":1}` | `Counter { value }` | Changes the synthetic session counter under its lock or journal transaction |

Checked arithmetic can instead produce `MemberResult::Refused { reason: Refusal::IntegerOverflow }`, serialized as `{"kind":"refused","reason":"integer_overflow"}`. A refusal belongs to that member; it does not erase successful members. Kernel `Status` is `completed` when all members succeed, `partial` when only some succeed, and `refused` when none succeed. An admitted batch records its report and advances the revision even if every member is refused. Admission errors such as a stale ETag, stale revision or conflicting operation ID return a package `Error` before new member execution.

For example, an increment by one in a new session produces this payload inside a `Report<kernel::Output>`:

```json
{
  "operation_id": "counter-001",
  "status": "completed",
  "revision": 1,
  "counter": 1,
  "members": [{"id":"increment","result":{"kind":"counter","value":1}}]
}
```

The surrounding report also contains `schema`, `input_etag` and the result `etag`. Verify those fields with `Report::verify` or the CLI `verify_result` action; interpreting a member result alone does not verify the report's binding to an accepted input.

The [API adapter](../src/api.rs) uses separate enums because a remote submission has different recovery rules:

| Type | Variants and wire values | Caller responsibility |
|---|---|---|
| `Endpoint` | `Responses` → `/v1/responses`; `ChatCompletions` → `/v1/chat/completions`; `Embeddings` → `/v1/embeddings` | Select an endpoint supported by the actual model and account |
| `ApiMemberResult` | `Completed { record }`, `Failed { record }`, `Unknown`; tagged by `status` as `completed`, `failed`, `unknown` | Inspect the preserved provider record; a missing row stays unknown |
| `ApiStatus` | `Completed`, `Partial`; JSON strings `completed`, `partial` | A batch is completed only when every reconciled member is completed |
| `TransportOutcome` | `Confirmed { id }`, `Rejected { code }`, `Unknown`; tagged by `outcome` | Report rejection only for a proven zero-effect outcome; preserve uncertainty otherwise |
| `SubmissionState` | `Prepared`, `Uploaded { file_id }`, `Submitted { file_id, batch_id }`, `RejectedUpload { code }`, `RejectedCreate { file_id, code }`, `UnknownUpload`, `UnknownCreate { file_id }`; tagged by `state` in snake case | Continue only the permitted lifecycle transition or reconcile the original remote operation |

Example serialized lifecycle values are `{"state":"uploaded","file_id":"file-demo"}` and `{"state":"unknown_create","file_id":"file-demo"}`. `Submitted` means the provider batch was created; it does not mean its requests have finished. `ApiMemberResult::Completed` means a reconciled row has a 2xx response and a body, without an explicit error; it does not certify the generated content. `UnknownUpload` and `UnknownCreate` are not permission to retry: a timeout, crash or malformed response can follow an actual remote effect.

Serde's `kind`, `status`, `outcome` and `state` tags make these distinctions explicit in JSON. Unknown enum variants and unknown fields in the typed envelopes are rejected. Provider request bodies and preserved response records intentionally remain `serde_json::Value`; the package does not impose a closed schema on every nested provider field. Schema identifiers version the package boundary. A new variant or schema requires compatible producers and consumers.

Enums constrain what can be requested and how outcomes are represented. ETags bind the accepted content; revision checks protect current mutable state; transactions and the journal preserve the local effect and its report. These mechanisms work together. Enums alone do not provide idempotency, and an ETag alone does not preserve history after a restart.


[Usage guide](USAGE.md) · [Project overview](../README.md)

# Usage guide

Build, CLI, restart recovery, Rust integration, API adapter, MCP and synthetic laboratories. Run all commands from the repository root, including when reading this file from `docs/`. Paths inside command blocks are repository-root paths.

See the [project overview](../README.md) and [typed enum reference](ENUMS.md).

## Build from source


Requirements:

- Rust and Cargo. The declared minimum Rust version is **1.89**. CI checks that minimum and uses **Rust 1.99.1** for the main test matrix; consult the exact revision's Actions run for its validation status.
- A C compiler for the selected target, because `rusqlite` builds bundled SQLite. The checked Windows MSVC environment used C++ Build Tools.
- Dependencies resolved from `Cargo.lock`. After fetching them, Cargo commands can also use `--offline`.

From the repository root:

```sh
cargo test --locked --all-targets
cargo test --locked --all-targets --all-features
cargo build --release --locked --all-features
cargo run --locked --example api_adapter
```

The API example uses a fake transport and makes zero network requests. The `openai-http` feature compiles the real HTTPS transport; building it does not contact OpenAI. Initial local validation used Windows; the exact-commit CI records Linux, Windows and macOS results.

The release executables are `etag-for-codex`, `etag-lab`, `lab-scenarios` and `lab-restart-scenarios` under `target/release/`. On Windows their filenames end in `.exe`.

## Quick start: plan, then execute

The CLI accepts one bounded JSON request on stdin by default. In PowerShell:

```powershell
$exe = './target/release/etag-for-codex.exe'
$info = Get-Content ./fixtures/kernel-info.json -Raw | & $exe | ConvertFrom-Json
$batch = @{
  schema = 'amqic.etag.kernel.v1'
  scope = 'demo'
  kernel_sha256 = $info.result.kernel_sha256
  operation_id = 'sum-001'
  expected_revision = 0
  members = @(
    @{ id = 'sum'; command = @{ kind = 'sum_i64'; values = @(10, 20, 30) } }
    @{ id = 'hash'; command = @{ kind = 'sha256'; text = 'hello' } }
  )
}
$plan = @{protocol='etag-for-codex.v1';action=@{
  kind='kernel_plan';batch=$batch
}} | ConvertTo-Json -Depth 20 -Compress | & $exe | ConvertFrom-Json
@{protocol='etag-for-codex.v1';action=@{
  kind='kernel_run';batch=$batch;expected_etag=$plan.result.input_etag
}} | ConvertTo-Json -Depth 20 -Compress | & $exe
```

This example executes pure calculations in a one-shot process. Without a journal, a later CLI process starts a new session. For persistent in-process JSONL input use `serve`; for restart recovery select a journal as described below.

Keep the accepted descriptor and expected tag. Changing the member order, scope, revision, command parameters or kernel pin changes input identity. Computing a new tag immediately before execution does not establish that the changed plan was accepted.

ETags cover the data actually included in the descriptor. The host must include relevant file contents, hashes or revisions and check real mutable state at its effect owner; this package does not watch the filesystem automatically.

## ETag identity and operation history

An ETag has the strict form `"sha256-<64 lowercase hexadecimal digits>"`, including the quotes. Weak validators, wildcard conditions and validator lists are outside this package's application contract. The gateway uses `428` for a missing precondition, `412` for a stale tag, `400` for malformed input and `409` for a conflicting operation ID.

Input and result identity are separate. A result ETag binds the accepted input ETag and full report payload. `verify_result` checks that binding. The canonical JSON format sorts object keys and preserves array order; it is a versioned package format, not RFC 8785. See [Protocol v1](PROTOCOL.md) for exact encoding and request schemas.

A digest is not authentication, authorization, encryption, a digital signature or durable storage. The CLI checks its own executable hash. An embedded host must independently bind its actual kernel pin and caller permissions.

The journal answers a different question: what was recorded under this operation ID? Recovery requires the original `scope`, `operation_id`, descriptor and expected input ETag. Changed input under the same ID conflicts. A new ID means a new operation.

A completed operation is recovered before checking the current revision for new admission. Consequently an operation originally accepted at revision 0 can be recovered after state advances to revision 1. New work must still use the current revision.

A partial report is also the original operation's result. Repeating that operation returns the same successes and refusals. Correct a failed member through a separate new operation against current state; do not replay successful effects from the original batch.

## Persistent CLI sessions and restart recovery

Without `--journal`, CLI, `serve`, MCP and laboratory state belongs only to its current process. To persist kernel state, choose a private journal path whose parent directory already exists:

```powershell
$exe = './target/release/etag-for-codex.exe'
$journal = 'C:/path/to/private-state/etag.sqlite'
& $exe serve --journal $journal
# Or start the MCP server:
& $exe mcp --journal $journal
```

`serve` accepts one UTF-8 JSON request per line and returns one response per line. One-shot mode also accepts `--journal PATH`; use the same selected journal for planning, execution and recovery.

For the synthetic kernel, the counter, revision and original operation report commit together in SQLite. A success response follows commit. A connection loss after commit therefore does not require a new effect. This atomicity covers state owned by that transaction; an arbitrary remote request or shell command does not become part of the SQLite transaction.

After a restart:

1. Recover the original operation ID, accepted descriptor, expected ETag and journal path from the host's saved manifest.
2. Open the same journal with the same compatible executor and actual kernel pin.
3. Resubmit the original request to recover its stored report.
4. Verify input/result ETags, individual member results and current state.
5. Create a new ID only for genuinely new work.

Preserve the journal when diagnosing an error. Clearing its ledger, choosing a new empty database or restoring an older copy can remove the available deduplication history. An open failure does not prove that an earlier effect never occurred.

Storage uses `journal_mode=DELETE`, `synchronous=FULL` and `BEGIN IMMEDIATE`, with a two-second busy timeout. It checks application identity, format/schema, snapshot size and checksums. Incompatible or corrupt state fails closed. Snapshots are bounded to 64 MiB and completed history is not automatically evicted.

Journals contain inputs and results in plaintext. Keep them outside Git and source distributions, restrict access and define your own backup/retention policy. Preserve pending rollback-journal files during recovery. Changes to schema or binary pin require an explicit compatibility/migration decision; no general migration tool or automatic pin rewriting is supplied. Read the [durability contract](DURABILITY.md) before relying on disk recovery.

## Embed the Rust library in an application

The package is not published to crates.io (`publish = false`). Use an obtained source checkout as a path dependency in your application's `Cargo.toml`, replacing the path with your actual checkout:

```toml
[dependencies]
etag-for-codex = { path = "../etag_for_codex" }
```

Enable `features = ["openai-http"]` only if your host needs the optional HTTPS transport. Rust imports use the crate name `etag_for_codex`. Obtain the applicable written license and any required source access before commercial or organizational integration; source availability does not grant those rights.

The runnable [typed recovery example](../examples/typed_recovery.rs) demonstrates the local SDK contract without provider calls. From this repository, create the private state directory once, then run:

```sh
mkdir private-state
cargo run --locked --example typed_recovery -- ./private-state/typed.sqlite3 example-op-001
cargo run --locked --example typed_recovery -- ./private-state/typed.sqlite3 example-op-001
cargo run --locked --example typed_recovery -- ./private-state/typed.sqlite3 example-op-002
```

The first command performs one fixed `IncrementCounter { amount: 1 }` operation and verifies an immediate duplicate execution returns the same report. The second invocation starts a new process and recovers operation `example-op-001` before planning new work, so the counter stays at one. The new ID in the third invocation requests another increment; the counter becomes two. These values assume a new journal and the same executable bytes. Reusing a journal from another run or rebuilding to a different binary pin changes those conditions.

The example hashes its actual executable, opens `Journal`, creates `Session::with_journal`, and calls `Session::lookup` before reading the current revision and creating a new `kernel::Batch`. It verifies `Report` bindings and handles every `MemberResult` variant with an exhaustive `match`:

```rust
match &member.result {
    MemberResult::Sha256 { etag } => println!("hash: {etag}"),
    MemberResult::SumI64 { value } => println!("sum: {value}"),
    MemberResult::Counter { value } => println!("counter: {value}"),
    MemberResult::Refused { reason } => println!("refused: {reason:?}"),
}
```

This fragment belongs inside a loop over `report.payload.members`, with `kernel::MemberResult` imported. Use the complete example for a buildable program. Its operation ID always represents the fixed increment-by-one intent. A general application must also retain and check the original intent, accepted descriptor and ETag; a lookup by ID alone does not establish that a newly supplied request is the same operation. Keep that manifest outside transient chat context. Include relevant source hashes and current state in your own descriptor, and route real effects through their actual transaction or reconciliation owner.

## OpenAI Batch API integration

The local Rust batch and the provider Batch API are different execution layers. The API adapter supports `/v1/responses`, `/v1/chat/completions` and `/v1/embeddings`. Endpoint body validity, model support and account access remain provider decisions.

The CLI supports planning, export and reconciliation; it does **not** upload or create remote batches. Try the supplied planning fixture:

```sh
target/release/etag-for-codex < fixtures/api-batch.json
cargo run --locked --example api_adapter
```

On Windows use the `.exe` name; in PowerShell pipe `Get-Content ./fixtures/api-batch.json -Raw` into that executable instead of shell input redirection.

For a custom host:

1. Build an `api::Batch` with schema, scope, stable operation ID, endpoint and uniquely identified requests. The fixture's `YOUR_BATCH_CAPABLE_MODEL` is a placeholder; select a model your provider/account actually supports.
2. Call `Batch::prepare()` to create an immutable `api::Plan`. Retain the accepted `plan.etag()` independently of the mutable working inputs.
3. Upload the exact `plan.jsonl()` bytes, with LF after each row. BOM, CRLF conversion or reserialization changes the file ETag.
4. Use one shared `ApiSession` for the scope. Each `advance` call performs at most one upload or create. `ApiSession::new` is memory-only; `ApiSession::with_journal` provides the durable lifecycle.
5. When a `batch_id` is known, retrieve its available output/error files through the host and reconcile the combined rows by `custom_id`.
6. Preserve missing rows as unknown and inspect nested provider responses separately. A transport 2xx does not certify model-output correctness.

`api_export` takes `batch` and `expected_etag`; it returns the manifest and provider JSONL without network access. `api_reconcile` takes those fields plus `result_jsonl`. Duplicate or foreign IDs reject; output order is restored to original request order. Missing members are never silently marked successful or automatically resubmitted.

### External unknown states

A durable API session commits `UnknownUpload` or `UnknownCreate` **before entering the transport callback**. It then sends once and records a confirmed outcome in a separate transaction. A crash may leave unknown even if the request had not yet left the process; absence of confirmation cannot establish absence of a remote effect.

After reopening, `Uploaded` can advance create without uploading again; `Submitted` returns its original report. Unknown upload/create states never trigger automatic POST retries. Provider lookup and reconciliation remain host-owned. There is no universal distributed exactly-once guarantee.

The optional `openai_http::OpenAiHttp` transport uses fixed routes, bounded response reads and no transport retries or redirects. It treats ambiguous send, status and parse failures conservatively as unknown. Live credential/provider acceptance has not been performed. A custom transport may report definite rejection only when it has established zero effect.

The host supplies authorization through a protected consumer. Never put secrets in chat, command arguments, environment passed through an agent, JSONL bodies, metadata, manifests, ETag descriptors, logs or MCP configuration. Provider metadata carrying a local tag does not make OpenAI enforce the package's precondition.

## MCP and custom agent hosts

The stdio server exposes `etag_batch` using JSON-RPC 2.0 and MCP protocol `2025-03-26`. It supports initialization, ping, tool discovery and tool calls. It is a tools-only subset without Streamable HTTP, resources, prompts or task queues. The Rust parser validates the closed action variants before dispatch.

Example configuration for a host that supports this MCP configuration format:

```toml
[mcp_servers.amqic_etag]
command = "C:/path/to/etag-for-codex.exe"
args = ["mcp", "--journal", "C:/path/to/private-state/etag.sqlite"]
cwd = "C:/path/to/etag_for_codex"
```

Replace the example paths with your own absolute paths. On Linux/macOS use the appropriate executable name. Preserve the host's existing configuration. This is a configuration example, not an installed or end-to-end accepted Codex integration.

For Codex, merge the Windows stanza above into the existing MCP configuration using the [durable configuration example](../integrations/codex-durable-config.example.toml). A corresponding Unix path example is:

```toml
[mcp_servers.amqic_etag]
command = "/absolute/path/to/etag-for-codex"
args = ["mcp", "--journal", "/absolute/private-state/etag.sqlite3"]
cwd = "/absolute/path/to/etag_for_codex"
```

Build the binary first and create the selected private parent directory. After the host loads the configuration, check that it discovers `etag_batch`. Explicitly direct the agent to use that tool for the selected batch: first `kernel_info`, then `kernel_plan` with a stable operation ID and the returned actual kernel pin, then `kernel_run` with the separately accepted `input_etag`. Save the original descriptor, operation ID, accepted tag and selected journal path in a host-owned manifest. On response loss or restart, recover the original request using that manifest and the same compatible journal; inspect the returned member statuses and verify its result binding. Do not silently generate a new operation ID to work around uncertainty.

For example, an agent instruction can say: “Use `amqic_etag`'s `etag_batch` to plan this batch, show its input ETag for acceptance, then execute only the accepted descriptor. Save the descriptor and operation identity for recovery.” The caller still needs to implement that acceptance and persistence policy. Merely adding the server does not cause every Codex action to pass through it. The supplied MCP server exposes the synthetic kernel and local API planning/reconciliation actions; it is not a general worker for existing Codex shell or editing operations.

The server inherits the launching user's OS permissions and creates no sandbox. Network upload/create remains library-only. To integrate a custom function-calling host, use the `tool_schema` action or `integrations/responses-tool.json`, validate arguments, execute the request and return the result with the original function call ID. Persist the mapping from call ID to operation ID, input ETag and result ETag.

### Capacity, stream loss and chat context loss

A host can keep an accepted manifest and journal outside the chat. If the local operation commits but the response is lost, a later client can recover the same ID after service availability returns.

| Event | Host action | Boundary |
|---|---|---|
| Model service returns Capacity | Retain the plan and wait under the host's policy | ETags do not create model capacity or quota |
| Local admission refuses | Inspect the refusal and retain the same request | The synthetic lab does not dispatch the refused kernel request |
| Reply is lost after commit | Recover the original ID and report | A missing reply does not prove zero effect |
| Lab SSE connection breaks | Retain the last fully processed event ID and reconnect with `Last-Event-ID` | Replay is bounded by retained event history |
| Chat loses context | Reload the manifest, reports and required artifacts | Hidden model reasoning is not recovered |
| Executor restarts | Reopen the same compatible journal | A new socket is created; a live connection is not preserved |
| Remote upload/create remains unknown | Reconcile the original remote operation | Do not automatically resend |

These are explicit application integration patterns. Built-in shell/edit tools, other MCP servers and stock Codex activity are not automatically wrapped. Real Codex token savings, cost reduction, quota changes and total latency improvements have not been measured.

## Synthetic laboratories

The loopback HTTP laboratory and finite terminal clients use synthetic local effects and zero provider calls. They require no OpenAI credentials and provide no web UI.

To run the original scenario client, start a dedicated lab in one terminal:

```powershell
./target/release/etag-lab.exe --port 8787
```

In another terminal, use the actual address printed in the `listening` event:

```powershell
New-Item -ItemType Directory -Force ./out | Out-Null
./target/release/lab-scenarios.exe 127.0.0.1:8787 --report ./out/lab-run-001.json
```

Use a fresh report path. If the port is occupied, start with `--port 0` and use the assigned address from the output. Do not mutate the lab concurrently while the finite consumer runs. Afterwards stop that exact owned lab process and verify its listener has closed.

For durable restart scenarios, the consumer starts, identifies, terminates and restarts its own child processes:

```powershell
./target/release/lab-restart-scenarios.exe `
  --lab-bin ./target/release/etag-lab.exe `
  --work-dir ./out/restart-run-001
```

The work directory must not already exist. The consumer retains the journal, `report.json` and lifecycle evidence there. A different work directory creates a new experiment; it does not resume an earlier one.

Verify your own original report without new effects:

```powershell
$report = Get-Content ./out/restart-run-001/report.json -Raw | ConvertFrom-Json
@{protocol='etag-for-codex.v1';action=@{
  kind='verify_result';input_etag=$report.input_etag;report=$report
}} | ConvertTo-Json -Depth 100 -Compress | ./target/release/etag-for-codex.exe
```

The process-restart scenarios cover graceful/abrupt restart, completed and partial replay, drop-after-commit, conflicting input, new work on restored state, persisted fake upload/create unknown states and two-process admission. They compare reports, ETags and counters. Their scope is process/application recovery, not physical power loss or a real provider test.

## Validation status

The recorded Windows validation completed **32 Rust tests with zero failures** across all targets/features. One subprocess helper was ignored by the main harness and invoked twice by its parent test. The release restart consumer passed **7 scenarios**, with **12 child processes** finished and **5 hard kills**.

These results do not establish hardware power-cut durability, network-filesystem behavior, Linux/macOS runtime acceptance, real OpenAI submission or normal installed Codex acceptance. See [Validation](VALIDATION.md) for test scope and reproduction; no scan badge, signature or performance guarantee is implied.

To measure the included synthetic process-batching example on your own machine:

```powershell
cargo run --release --locked --example benchmark -- ./target/release/etag-for-codex.exe
```

It compares 16 individual CLI processes with one 16-member batch across five rounds, making zero provider calls. Input-tag construction and request serialization are excluded equally from the child-process timing and tag cost is measured separately. It does not measure ETag versus no ETag, journal performance, model tokens or end-to-end Codex speed.

## Library layout and extension

| Module/path | Purpose |
|---|---|
| `src/lib.rs` | Strong ETags, canonical encoding, result verification and compatibility helper |
| `src/kernel.rs` | Closed kernel commands/results, revisions and operation ledger |
| `src/journal.rs` | Checked transactional disk snapshots |
| `src/api.rs` | Plans, manifests, API lifecycle and result reconciliation |
| `src/openai_http.rs` | Optional real HTTPS transport |
| `src/main.rs` | One-shot JSON, JSONL service and stdio MCP |
| `src/lab.rs`, `src/bin/` | Synthetic HTTP lab and finite consumers |
| `examples/` | Typed journal recovery, fake API adapter and process-batching benchmark |
| `tests/`, `fixtures/` | Contract, failure, concurrency and restart cases |

To extend the kernel, add explicit command/result variants and exhaustive matches, update limits and wire/tool schemas, bind the actual executor pin and put mutable state/ledger under its real transaction owner. Test valid calls, unknown variants, stale refusal without effects, revision races, partial results and original-operation recovery. A numeric FFI ABI is not implemented; an eventual ABI needs checked conversion and explicit buffer ownership.

Local limits include 1,024 members, 8 MiB input/result files, 1,024 recorded operations per session and 32 CLI scopes. The lab has narrower scenario-specific limits. There is no unsafe automatic history eviction; storage exhaustion requires an explicit owner policy. Hashing, serialization and FULL-sync disk writes have costs that should be measured on the intended workload.

`qcalculator_v1_etag` preserves one existing normalized typed-job encoding. It does not execute Q-Calculator algorithms or establish full backend parity. See [Compatibility](COMPATIBILITY.md).


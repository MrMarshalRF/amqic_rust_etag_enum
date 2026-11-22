# ETag for Codex

Codex lost a response? Recover recorded results by operation ID.  
Inputs changed after approval? Reject the changed snapshot before execution.  
Remote outcome unknown? Reconcile it; never blindly resend.

## Where it helps

| Situation | Package behavior |
|---|---|
| Inputs change after plan acceptance | Rejects a mismatched expected ETag before execution |
| A reply is lost after a local commit | Returns the recorded report for the same operation ID and input |
| One member fails | Preserves successful results and explicit refusals in a partial report |
| Clients compete for one revision | Checks admission and records local state under a lock or SQLite transaction |
| An upload or create has an uncertain outcome | Preserves unknown state and prevents blind automatic retransmission |
| The executor restarts | Reopens the selected compatible journal and validates recorded history |

## What you get

A strong ETag binds the selected input descriptor. A separate result ETag binds that input and the full report. The journal preserves local state and original reports together, allowing callers to distinguish new work from recovery. Keep the accepted descriptor, tag, operation ID and journal path outside transient chat context.

The reference kernel offers SHA-256, checked integer addition and a synthetic counter. Closed Rust enums define commands, results and recovery states; exhaustive matches make callers handle each supported outcome. It does not run arbitrary shell commands. See the [enum reference](docs/ENUMS.md) for wire values and examples.

An optional HTTPS transport supports a host application's [OpenAI Batch API integration](docs/USAGE.md#openai-batch-api-integration). Local kernel batches and provider batches have separate lifecycles. Remote unknown outcomes require host reconciliation; there is no universal distributed exactly-once guarantee.

## Build and try it

Use Rust/Cargo and a C compiler for bundled SQLite. The declared minimum Rust version is **1.89**. From the repository root:

```sh
cargo test --locked --all-targets --all-features
cargo build --release --locked --all-features
cargo run --locked --example api_adapter
```

The API example uses a fake transport and makes zero network requests. The release CLI is `target/release/etag-for-codex` (`.exe` on Windows). Building the optional transport does not submit provider work.

The [usage guide](docs/USAGE.md) contains the complete build instructions, plan/run example, journal recovery sequence, Rust SDK example, API lifecycle, MCP configuration and synthetic laboratory commands. Start there before selecting persistent state. The package is source-delivered and is not published to crates.io.

Try the [synthetic laboratories](docs/USAGE.md#synthetic-laboratories) for local failure and restart scenarios with zero provider calls. They require no OpenAI credentials; use the documented commands and preserve their original reports.

## Integration boundaries

**Recover recorded work without repeating its completed local effects, when the original request and compatible journal are retained.** This applies only to operations admitted and recorded by this package. Adding its MCP server does not intercept built-in shell/edit tools, wrap every Codex action or activate a normal installed Codex integration.

ETags cover only data included in the descriptor. The host must bind relevant source hashes, current state and actual caller authority. Digests do not authenticate callers or encrypt journals. A local transaction does not make arbitrary remote effects atomic.

Model Capacity, quota, hidden reasoning and live sockets are outside this recovery mechanism. Real Codex token savings, cost reduction and total latency improvements have not been measured. See [Protocol](docs/PROTOCOL.md), [Durability](docs/DURABILITY.md) and the scoped [validation record](docs/VALIDATION.md). Synthetic process-restart results do not establish power-loss durability or real provider acceptance.

## Source and license

The [publication workflow](docs/PUBLICATION.md) describes the closed source allowlist, inventory and archive checks. [Actions runs](https://github.com/MrMarshalRF/amqic_rust_etag_enum/actions) show checks tied to exact commits; a workflow file alone does not establish a passing run.

Copyright © 2026 Aleksei Sergeyevich Musienko, an individual entrepreneur trading as AlexMus Quantum Information Consortium (AMQIC). Personal noncommercial use is free under [LICENSE](LICENSE). Commercial and organizational use require a separate written license agreement signed by the Licensor. See [commercial licensing](COMMERCIAL-LICENSE.md) and the full public owner particulars in LICENSE. This is source-available software under a custom license.

[VirusTotal observations](VIRUSTOTAL.md) describe exact file hashes and recorded scan status, including missing results. The [publication workflow](docs/PUBLICATION.md) explains the human-operated scan procedure; changed bytes require matching observations, and a completed scan is not proof of safety.

## Optional support

Voluntary support is optional and grants no commercial or organizational license, SLA, financial return or investment interest. The exact asset/network/address triples are also in [wallets.txt](wallets.txt).

| Asset | Network | Address |
|---|---|---|
| USDT | TRC20 | `TTu2KEobZW3hv8RaX2VaWz4Fkj8hQ1Wjqx` |
| BTC | Bitcoin | `19BvhiToixuGvVsVw1fZTiDdAPnXhHJzG3` |
| ETH | Ethereum | `0xf26b834094b9509d5ec3131759ecb64fb45f7405` |

**It does not make Codex smarter. It makes recorded work recoverable.**

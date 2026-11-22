# Security policy

## Private reporting

Report suspected vulnerabilities privately to [cyber@amqic.ai](mailto:cyber@amqic.ai).
Use a subject such as `Security report: amqic_rust_etag_enum`.

Include the affected commit/files, operating system and toolchain, expected and
observed behavior, prerequisites, impact and a minimal reproduction using synthetic
data. For journal issues identify the crash point, memory/disk mode and transaction
stage. Do not send API keys, authorization headers, real journals, database exports,
customer payloads or personal diagnostics. A fresh synthetic fixture is preferable.

Keep exploit details out of public issues while the report is assessed. Reports are
evaluated by evidence and impact; no paid bounty, guaranteed response time or support
service is established by this policy. Coordinate disclosure with the maintainer.
This repository grants no permission to scan or attack maintainer or third-party systems.

## Scope and supported source

Report against the current maintained source and identify the exact revision. No
published release, signed registry or deployed service is implied by repository files.
Useful report areas include snapshot identity, operation/revision admission, persisted
replay protection, external-effect uncertainty, corruption handling, path/file handling,
CLI/MCP parsing, lab lifecycle and misleading security claims.

The reference kernel and loopback lab are examples, not an authentication gateway,
OS sandbox, production HTTP server or distributed transaction manager. ETags do not
authorize callers. A local SQLite journal does not make an arbitrary remote effect
atomic, restore a live socket, raise model capacity or preserve hidden Codex reasoning.
Applications remain responsible for actual authorization, tenant boundaries, resource
budgets, credential custody, deployment and provider reconciliation.

## Data and credentials

Journal snapshots can contain request bodies and results in plaintext. Use a private
state directory; exclude databases, rollback journals, logs, backups and credentials
from Git and source archives. Never reset/delete a journal to bypass unknown outcomes.
An old backup or new path is not evidence that a previous effect did not occur.

Existing transports accept protected authorization headers from the host; the CLI
does not collect OpenAI keys. Do not paste credentials into issues/chat, pass them in
arguments or environment variables, or place them in examples, manifests or reports.
The optional VirusTotal runner uses operator-only hidden terminal input. It is not a
registered secret-store broker or enforceable isolation from the same OS user.

## Publication and VirusTotal

Use the source allowlist and verifier described in [publication](docs/PUBLICATION.md).
`.gitignore` is a convenience, not a content-security boundary. Inspect the exact
inventory, file hashes and generated archive; local agent work and execution evidence
are excluded from the source payload. No release signature is claimed: SHA256SUMS is
an unsigned integrity inventory and cannot authenticate its own publisher.

VirusTotal hash queries send identifiers outside the machine. Standard uploads send
full contents and may disclose them to VirusTotal and its partners. Submit only the
exact reviewed payload with explicit disclosure permission. Never submit private
credentials, journals, customer data or sensitive diagnostics.

The [file register](VIRUSTOTAL.md) and [JSON observations](VIRUSTOTAL.json), when present,
are tied to exact SHA-256/size and retrieval/analysis times. An observation applies only to its recorded bytes; changed bytes need a new matching observation. `not_queried`, `not_found`, failures, unknown outcomes, unsupported
types and engine timeouts are not passed checks. Even a completed analysis with no
detections is not proof of source safety, correct authorization, CVE clearance or
production suitability. Generated metadata is not recursively self-scanned.

Read the official [upload policy](https://docs.virustotal.com/reference/files-scan)
and [analysis definitions](https://docs.virustotal.com/reference/analyses-object).
Reports and local tests provide different evidence; neither is a security certification.

## Source inventory and validation records


GitHub Actions checks formatting, Clippy with warnings denied, both feature
configurations on Linux/Windows/macOS, the declared minimum Rust version,
documentation, optimized restart tests, source-package integrity and RustSec
advisories. The `CI complete` check requires every job to succeed. The source
artifact contains verified source and a manifest; it contains no runtime journals
or binaries. See the [Actions runs](https://github.com/MrMarshalRF/amqic_rust_etag_enum/actions)
for results tied to an exact commit. Changes invalidate earlier VirusTotal
observations for the changed file hashes; unqueried rows remain explicit.

The publication profile retains source, examples, tests, English documentation and CI. It excludes databases, journals, logs, builds, local paths and agent work. Use the closed allowlist producer and inspect its inventory/archive readback; `.gitignore` alone is not a security boundary. See [publication workflow](docs/PUBLICATION.md).

VirusTotal reports are matched by exact file SHA-256 and size. New or changed file hashes remain `not_queried` until the operator runs the reviewed terminal workflow; prior observations may be retained only when their exact SHA-256 and size still match. A completed report is not proof of safety; unsupported/error/timeout engines remain visible. Hash queries and file uploads disclose data externally. Keys are entered only through hidden human terminal input, never chat, argv, environment or piped stdin.

The included CI definition does not imply a passing GitHub Actions run, a signed release, an upload or publication.


## Voluntary support address verification

The addresses in [wallets.txt](wallets.txt) were copied from the maintainer-provided sibling project's wallet file. That comparison establishes source equality for the address strings; it is not independent proof of wallet ownership. Check the exact asset, network and address before any use. A transfer on the wrong network may not be recoverable.

No current QSM signature is supplied for this project's changed source inventory or wallet file. An unsigned SHA-256 inventory can detect differences against a separately trusted copy; it cannot authenticate the publisher by itself. Do not treat an older inventory or a signature from another project as covering these bytes. The maintainer's [verification page](https://amqic.ai/pages/request-verification) is a separate verification channel, not evidence that a current signature has already been issued.

Voluntary support does not grant a commercial or organizational license, a service-level agreement, a financial return or an investment interest. The [license](LICENSE) and any separate written agreement govern software use.

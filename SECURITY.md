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
are tied to exact SHA-256/size and retrieval/analysis times. A changed file invalidates
its observation. `not_queried`, `not_found`, failures, unknown outcomes, unsupported
types and engine timeouts are not passed checks. Even a completed analysis with no
detections is not proof of source safety, correct authorization, CVE clearance or
production suitability. Generated metadata is not recursively self-scanned.

Read the official [upload policy](https://docs.virustotal.com/reference/files-scan)
and [analysis definitions](https://docs.virustotal.com/reference/analyses-object).
Reports and local tests provide different evidence; neither is a security certification.

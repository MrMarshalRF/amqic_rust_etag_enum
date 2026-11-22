# Disk journal and restart recovery

The optional journal is a local SQLite database. Open it with `Journal::open`, then
use `kernel::Session::with_journal` or `api::ApiSession::with_journal`. Existing `new`
constructors remain memory-only. CLI/MCP and the lab accept `--journal PATH`.
Parent directories must already exist. The lab restart consumer creates a new work
directory and retains its database and evidence there.

## Local and external effects

The synthetic kernel commits counter, revision and original operation report in one
SQLite transaction. An identical operation ID/input ETag recovers that report before
new-admission revision checks. Different input under the same ID conflicts. Recovery
validates stored identities, report tags and state invariants without dispatching the
completed commands again.

For external API effects, the transaction commits UnknownUpload/UnknownCreate before
calling transport; a separate transaction records a confirmed result. A process may
die before or after the request leaves the machine. Reopening Unknown never sends it
again automatically. Uploaded may advance create once without uploading again;
Submitted returns the original report. Provider lookup/rebind remains host-owned.
There is no distributed transaction or universal remote exactly-once guarantee.

The laboratory's fake provider has only local synthetic effects. Kernel/API memory
checkpoints, reports, fake-provider observations and retained events are captured in
one LabSnapshot transaction. No external HTTP callback is run inside that transaction.
The lab responds or intentionally drops the connection only after commit. SSE captures
an owned committed event snapshot before sending bytes, preventing uncommitted events
from escaping through a concurrent reader.

## Storage contract

- `journal_mode=DELETE`, `synchronous=FULL`, `BEGIN IMMEDIATE`; busy wait is bounded
  to two seconds. Independent connections/processes use SQLite writer admission.
- Application ID, format version, exact schema and pragma readback are checked.
  Foreign, existing empty, malformed or incompatible files are not silently replaced.
- Each versioned snapshot is checksummed with namespace/scope/version coverage and
  bounded to64MiB. Metadata/length checks precede payload allocation. A checksum is
  corruption detection, not authentication against a writer who can replace the DB.
- Storage failures return `Storage`/HTTP503. No accepted new result is returned before
  commit. Established missing rows fail closed rather than rebuilding from a cache.
- Completed history is not evicted to make room. Limits remain explicit; use an
  operator-approved retention/migration policy instead of clearing deduplication state.
- Lab request handlers reload committed state inside the same gate/transaction as
  admission. After a failed commit, subsequent operations reload authoritative disk
  state before doing work; no dirty cache is advertised as a committed result.

The database contains operation inputs and results in plaintext, potentially including
API request bodies. Authorization headers are not part of it. Choose an access-controlled
path, exclude live journals from Git/source releases, and do not put keys or passwords
in batch payloads. This feature does not configure backups or retention automatically.

## Compatibility and recovery boundaries

Reuse the same database, original operation ID, accepted descriptor and expected ETag.
Kernel/lab recovery requires the same actual binary pin. An upgrade changing that pin
fails closed until an explicit compatible migration/namespace decision. No automatic
pin rewriting, journal reset or migration tool is supplied. Memory checkpoint import
is not supported for disk sessions and cannot be used to roll back their ledger.

Journal deletion, replacement with an older valid backup, or choosing a new empty path
loses the history available at that path. A local database cannot prove history that
has been externally removed; preserve the original database and pending rollback
journal files. Recovery from missing files is an operator action, not permission to
repeat an unknown external effect.

The tested boundary is process termination/restart on local Windows storage, including
explicit hard kills. It does not simulate power loss, failed hardware, broken fsync or
network filesystems. SQLite's own durability conditions apply; FULL in DELETE mode is
not a promise to survive every power failure. See [SQLite synchronous settings](https://www.sqlite.org/pragma.html#pragma_synchronous)
and [atomic commit](https://www.sqlite.org/atomiccommit.html).

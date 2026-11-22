# Preparing a GitHub source snapshot

This repository is source-available for personal noncommercial use. Commercial and
organizational use requires a prior written agreement: read [LICENSE](../LICENSE)
and [commercial terms](../COMMERCIAL-LICENSE.md). Do not describe it as MIT or
OSI-approved open source. Third-party dependencies retain their own licenses.

## Closed source inventory

The producer copies only approved Rust sources, examples, tests, synthetic fixtures,
publication scripts, selected English documentation, public CI/configuration examples
and project policies. It does not recursively copy the working directory.

Databases, rollback journals, logs, historical execution reports, agent/editor work,
local configuration, credentials, builds, Git internals and temporary artifacts are
excluded. Source files are checked for links/hardlinks, unsafe names, byte bounds,
secret-like literals and private machine paths. A finding reports path/rule/line,
never the matched value. These checks are bounded controls, not universal secret
detection or enforceable protection against a hostile same-user concurrent writer.

Selected text becomes UTF-8 without BOM and LF before hashing. `.gitattributes`
preserves that representation in Git. VirusTotal hashes must refer to these final
bytes, not the original working copy with possibly different line endings.

With Python3.11+ from the repository root:

```sh
python -B scripts/prepare_repository.py plan --source .
python -B scripts/prepare_repository.py stage --source . --destination NEW_PAYLOAD --expected-etag ACCEPTED_PLAN_ETAG
python -B scripts/prepare_repository.py verify --payload NEW_PAYLOAD
python -B scripts/prepare_repository.py archive --payload NEW_PAYLOAD --output NEW_SOURCE.zip --expected-etag ACCEPTED_PAYLOAD_ETAG
```

Use the exact quoted strong tag returned by the separately inspected plan. Stage
uses `plan.etag`, not `plan.inputEtag`. Archive uses the verified payload `etag`.
Do not recalculate a new tag to bypass a refusal. Destinations/archives must be new;
the producer never deletes or overwrites an old payload to make a check pass.

`FILES.json` inventories ordinary files and exposes stable `ordinaryEtag`/`inputEtag`.
Generated verification metadata is classified separately; `SHA256SUMS` covers the
payload except itself. The final payload tag covers the actual selected files.
ZIP verification reads back every member and exact bytes, not just archive creation.
These hashes are unsigned integrity evidence, not maintainer authentication.

## VirusTotal review

Before any query/upload, inspect the exact file inventory and disclosure scope.
Hash-only lookup sends SHA-256 identifiers; VirusTotal may retain queried indicators.
Standard uploads send complete files and can disclose them to VirusTotal/partners.
A private GitHub repository does not make a VirusTotal upload private.

The scanner must run from inside the sealed payload, with its accepted payload tag.
Only a human uses the hidden-input terminal prompt. Never pass a key in chat, argv,
environment variables, stdin pipes, reports or source. Nonterminal execution and
getpass echo fallback are refused. This is an operator workflow, not a registered
stored-secret consumer or an OS isolation boundary.

Create private output directories outside the payload. Example command shape:

```sh
python -B NEW_PAYLOAD/scripts/virustotal_review.py --payload NEW_PAYLOAD --expected-etag ACCEPTED_PAYLOAD_ETAG --output PRIVATE_METADATA/VIRUSTOTAL.json --ledger PRIVATE_STATE/vt-operations.jsonl
```

This defaults to hash-only lookup. Add `--upload-approved` only after the owner has
approved disclosure of the exact ordinary snapshot; the human also confirms the
printed batch in the terminal. The key is entered separately with hidden input.
The client uses one fixed HTTPS origin, no redirects or inherited proxies, bounded
responses/timeouts and at most one request per15.1seconds. Auth/quota failures stop
the batch. No live VirusTotal test is implied by offline unit-test success.

Keep the same append-only upload ledger across attempts. It is locked and records
intent with fsync before POST. Unknown sends or lost recording outcomes prohibit a
blind re-upload. Later hash/analysis GETs may reconcile a confirmed upload, with exact
SHA-256/size checks. If report files are inconsistent after interruption, preserve
them and choose a new report directory while reusing the original upload ledger.
Never create a new ledger merely to bypass an unknown operation.

The metadata directory contains exactly `VIRUSTOTAL.json` and `VIRUSTOTAL.md`; keep
the upload ledger/lock elsewhere. Restage to a new destination using
`--metadata-dir PRIVATE_METADATA`, inspect the new plan and use its new plan ETag.
Ordinary hashes remain the scan identity, while metadata changes the final payload
tag. Regenerate checksums/archive and inspect final Git blob bytes afterward.

`known` reports retain analysis/retrieval dates and per-engine outcomes. `not_queried`,
`not_found`, `unsupported`, `failed`, `unknown`, engine timeouts and failures are not
passed checks. Undetected does not prove safety or CVE clearance. Generated FILES,
VirusTotal reports and checksum metadata are not recursively self-scanned.

Scanner `complete`/exit0 means every ordinary file has a matching known report. It
does not mean clean, that every engine supported the type, or that release is approved.
Inspect all retained malicious/suspicious/unsupported/failure/timeout counts separately.

## GitHub handoff

Inspect the final inventory, archive readback, license, security policy and checks.
Copy only the sealed payload into a separately reviewed checkout of
`MrMarshalRF/amqic_rust_etag_enum`; preserve existing remote changes. Inspect staged
paths/diff and commit with the human author and the contributor trailer. Do not add
the source working directory, private metadata, journals or test output wholesale.

Preparation does not change repository visibility, push commits, publish a Release,
upload to VirusTotal or create a maintainer signature. Those actions need their own
actual authority and result verification. The supplied CI is a definition, not an
observed passing GitHub Actions run; it never uploads to VirusTotal or publishes.

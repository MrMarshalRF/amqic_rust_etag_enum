# Validation scope

The Rust implementation was locally checked on Windows x86_64 MSVC with Rust/Cargo
1.99.1 before preparing this source publication profile:32 tests passed,0 failed;
one subprocess helper marked ignored by the ordinary harness was actually invoked
twice by its passing parent. A release restart run passed7 oracles across12 child
lifecycles, including5 hard kills. These are recorded local implementation results,
not a GitHub Actions run or an external security certification.

The restart oracles compare original reports/input-result ETags, counter/revision and
fake-provider call counts. They cover completed/partial/drop-after-commit restart,
unknown upload/create stages, changed input conflicts, restored revision and two
live processes sharing one journal. API callback-death fixtures independently check
that Unknown was committed before callback and no second callback is sent on reopen.

Reproduce on the exact source revision you intend to use:

```sh
cargo test --locked --all-targets --all-features
cargo build --release --locked --all-features
python -B scripts/test_prepare_repository.py
python -B scripts/test_virustotal_review.py
```

The publication producer's current Windows unit run had16 cases:15 passed and1
explicit symlink-creation-denied skip. The hardlink, connected CLI, changed-input,
secret/path denial and exact archive readback cases executed. The VirusTotal runner's
16 offline tests passed using synthetic clients/keys only: no live query/upload or
real key was used. Offline success is not VirusTotal clearance.

| File family | Validation method |
|---|---|
| Rust src/examples/tests/fixtures | Locked all-target/all-feature Cargo tests; actual child restart integration |
| Journal/kernel/API protocols | Reopen, conflict, concurrency, corruption/write-failure and durable intent tests |
| Publication producer | Positive and relevant negative Python tests; closed inventory, exact hashes and ZIP readback |
| VirusTotal runner | Fake lookup/upload/poll and actual request-construction/error-path tests; terminal/input and sidecar protection |
| Docs/license/security/policies/templates | Human-intent and technical review, generic paths, local links and content scanner |
| CI workflow | Pinned official action refs, read-only permissions and static scope review; first real hosted run remains separate |
| FILES.json / SHA256SUMS | Recomputed against exact normalized payload; unsigned integrity, not authentication |
| VIRUSTOTAL.json / VIRUSTOTAL.md | Exact ordinary inventory/size/SHA binding; pending/errors/engine limits remain explicit |

The declared Rust minimum1.89 was not independently executed. Linux/macOS native,
physical power failure, network-filesystem failure, live OpenAI credentials/provider,
normal Codex activation and real VirusTotal service behavior are not established by
these checks. No historical runtime logs, raw journals, local machine paths or agent
receipts are included in this source repository. Consumers can reproduce their own
reports locally; keep generated data outside version control.

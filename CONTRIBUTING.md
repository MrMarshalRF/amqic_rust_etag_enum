# Contributing

Read [LICENSE](LICENSE) and [commercial terms](COMMERCIAL-LICENSE.md) before use.
Public source availability is not permission for commercial or organizational use.
Do not add third-party material without its required license and attribution.

Keep public documentation and contributor-facing text in English. Preserve protocol
identifiers and exact source data. Keep changes scoped and describe the behavior,
reason, executed checks and remaining limits. Do not equate a build, local commit or
synthetic test with deployment, live provider validation or security certification.

Preserve closed command/state/result enums, strong input/result validators, stable
operation IDs, current revision checks and original-operation reconciliation. Unknown
external effects must not be automatically replayed. Journal changes require restart,
conflict, corruption/write-failure and concurrent-admission tests. Do not clear a
database to obtain a passing check. Binary/schema migration is an explicit contract.

Use synthetic data and purpose-built loopback fixtures. Own each spawned process,
record its actual identity/address, stop the exact child, wait and verify listener
absence. Do not test third-party services or use real credentials in the lab.

Typical checks:

```sh
cargo test --locked --all-targets --all-features
python -B scripts/test_prepare_repository.py
python -B scripts/test_virustotal_review.py
```

Use the publication producer for a clean source snapshot. Do not commit `target`,
`out`, databases, logs, local paths, secret/config stores, editor/agent work, receipts,
internal plans or old execution reports. Publish only intentional verification
metadata; preserve not-run and partial states. Recompute hashes after final bytes,
and never copy scan results from a different revision or repository.

Maintain CHANGELOG under Unreleased until a publication actually occurs. Git hooks
and ordinary review remain active. Preserve the human author; commits containing
material OpenAI Codex assistance should include this trailer after a blank line:

```text
Co-authored-by: Codex <noreply@openai.com>
```

For sensitive vulnerabilities use [private reporting](SECURITY.md), not public issues.

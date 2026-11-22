# Existing AMQIC/Q-Calculator compatibility

Reference inspected 2026-11-21: `Q-Calculator/src/kernel.rs`, SHA-256
`cb666a6bcc79396c406508588dd369084faeca99abb98b15df02e73a6de5f10e`.
The original source was not changed or imported as a build dependency.

At lines 134–139 the current implementation serializes the tuple
`(BATCH_SCHEMA, executable_sha256, jobs)` with serde_json, hashes it with SHA-256 and
formats a quoted `sha256-...` validator. `BATCH_SCHEMA` is `q-calculator.batch.v1`.
Its typed jobs have already passed normalization/validation, including defaults.

`qcalculator_v1_etag<T: Serialize>` implements that encoding. Call it with the exact
already-normalized typed jobs and executable digest from the original producer.
Arbitrary `serde_json::Value` objects may reorder keys; another serializer or absent
defaults may produce different bytes. The helper does not validate Q16 circuits,
normalize job defaults, execute QRACE or provide the original Q-Calculator backend.
It is an encoding compatibility bridge, not a replacement for those algorithms.

Independent golden vector:

```text
["q-calculator.batch.v1","0000000000000000000000000000000000000000000000000000000000000000",[{"operation":"pi","digits":50}]]
SHA256 = 72bfb4cb9e7046380a457088777a90455f43c625f63151f5530eece00a814eb0
```

The expected digest was independently calculated with Node crypto; the Rust test uses
a typed `Pi` struct preserving the original field order. This checks the formula and
one normalized job, not a complete original-kernel replay/parity suite.

The original result at line 194 echoes the input `etag`. Treat that field as input
identity. New `Report<T>` deliberately has separate `input_etag` and `etag` and hashes
actual result content. Do not silently reinterpret an old result as the new schema.

The new snapshot families are `amqic.etag.kernel.v1`, `amqic.etag.openai-batch.v1` and
`amqic.etag.api-plan.v1`; new reports are `amqic.etag.result.v1`. Their canonical
sorted-object encoding is distinct from the legacy typed tuple encoding. Migration
requires choosing the matching constructor and schema at each actual caller.

No existing Q-Calculator, DOCTENSA, OpenAI or Codex caller was migrated/activated in
this delivery. The standalone wrapper is the delivered integration surface.

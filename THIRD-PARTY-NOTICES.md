# Third-party notices

The project license covers first-party source and documentation distributed by
Aleksei Sergeevich Musienko, individual entrepreneur operating as AlexMus
Quantum Information Consortium (AMQIC). It does
not relicense dependencies or restrict rights granted by their own licensors.

Direct dependencies pinned through Cargo.lock include:

| Component | Declared upstream license |
|---|---|
| serde / serde_json | MIT OR Apache-2.0 |
| sha2 | MIT OR Apache-2.0 |
| rusqlite | MIT |
| reqwest (optional) | MIT OR Apache-2.0 |

rusqlite's bundled SQLite is separately upstream public-domain software; its Rust
wrapper and other bundled dependencies retain their own notices. Transitive crates,
the Rust standard library, compilers and CI tools have separate terms. Inspect the
exact Cargo.lock versions and their packaged license files before distributing a
binary or vendoring their source. This source payload does not vendor dependencies
or ship built executables; a future binary distribution needs its own full notices.

The legacy Q-Calculator helper reproduces a documented tuple-hash encoding and one
golden vector; no Q-Calculator backend is imported or represented as part of this
crate. Publication controls draw on the same owner's security starter-kit policies.
This does not copy that project's MIT commercial grant into this project.

Codex and OpenAI names identify integration targets and their respective owners.
This project is not an official OpenAI product and does not imply endorsement.

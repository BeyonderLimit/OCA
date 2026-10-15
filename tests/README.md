# Workspace integration tests

- Rust unit tests live next to code (`src/*.rs`), CLI end-to-end tests in
  `crates/oca-cli/tests/`.
- This directory is reserved for cross-crate / workspace-level integration tests
  (sample project fixtures, `oca new` → write → reopen flows) starting Phase 1.
  Workspace-level tests must attach to a package to run under `cargo test`, so
  Phase 1+ will either add an integration-test target in a crate that points here
  or introduce a dedicated test harness crate — decided at Phase 1, recorded here.

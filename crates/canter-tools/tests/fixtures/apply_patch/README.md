# Codex apply_patch fixtures

The `patch.txt`, `input/`, `expected/`, and `.gitattributes` files in this directory were copied from openai/codex, tag `rust-v0.163.0`, full commit `83086188ae0614bd882818ff1aa23f3194962012`, source directory `codex-rs/apply-patch/tests/fixtures/scenarios/`. The files are licensed under Apache-2.0.

Except for the local change below, the files remain byte-for-byte fixtures, so they cannot carry attribution headers without changing the behavior under test. This file is the attribution for each copied data file. The local test runner replaces Codex's standalone executable with canter's in-memory parser and controlled executor.

Local change: `015_failure_after_partial_success_leaves_changes/expected/created.txt` is removed. Its missing update target fails canter's required whole-patch preflight, so no file is written. A separate executor-level test retains coverage of a genuine failure after the first write.

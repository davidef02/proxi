# Learned rules

- When editing an existing file for a task: change only the lines the task needs; leave unrelated formatting, comments and banners byte-identical to the base branch.
- When porting parsing or scanning code from Node to Rust, copy the original's failure boundary and pin it with a malformed-input test (a port that drops the error diverges silently on malformed input).
- When building JSON in Rust that must match Node's key order: build a fresh map in the Node order, emit array-index keys (`"0"`–`"4294967294"`) first in ascending numeric order, and assert the serialised string (V8 enumerates array-index keys before string keys; `Map::remove`/`insert` reorder too).
- When several Rust integration-test binaries need the same seed or golden helpers, define them once in `crates/proxi/tests/common/` and call them from each test file (per-binary copies drift silently, so editing one leaves the others untested).
- When porting Node code into a new Rust module and the crate already has the same helper (e.g. `esc`): make the existing one `pub(crate)` and call it, never copy the function (a byte-identical copy drifts from the original and review sends the change back).

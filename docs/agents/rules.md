# Learned rules

- When editing an existing file for a task: change only the lines the task needs; leave unrelated formatting, comments and banners byte-identical to the base branch.
- When porting parsing or scanning code from Node to Rust, copy the original's failure boundary and pin it with a malformed-input test (a port that drops the error diverges silently on malformed input).
- When several Rust integration-test binaries need the same seed or golden helpers, define them once in `crates/proxi/tests/common/` and call them from each test file (per-binary copies drift silently, so editing one leaves the others untested).

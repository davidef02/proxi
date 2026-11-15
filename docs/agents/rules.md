# Learned rules

- When porting parsing or scanning code from Node to Rust, copy the original's failure boundary and pin it with a malformed-input test (a port that drops the error diverges silently on malformed input).
- When a request handler checks shared state and then acts on it, even after an await: do the check, the decision and the write under the same lock (a lock released between them lets two concurrent requests both pass and duplicate the work).

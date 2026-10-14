# `batching` resolver: `MissingKey<K>` → `BatchError<K, E>`

The `batching` resolver now awaits its fetch (no `run_blocking`, safe on a `current_thread`
runtime) and no longer panics when the fetch fails. Entries fail with `BatchError<K, E>`:

```rust
// 0.6
let d: Deferred<V, MissingKey<K>> = /* … */;
// Err(Cause::Fail(MissingKey(k)))

// 0.7
let d: Deferred<V, BatchError<K, E>> = /* … */;
// Err(Cause::Fail(BatchError::Missing(k)))  — key absent from the fetch map
// Err(Cause::Fail(BatchError::Fetch(e)))    — fetch failed; every waiting entry gets `e`
```

# The Resource Problem — Cleanup in Async

RAII in synchronous code:

```rust,ignore
{
    let file = File::open("data.txt")?;
    process(&file)?;
}  // file.drop() runs here, always, unconditionally
```

Reliable. Simple. The drop happens when the scope ends.

## The Async Complication

```rust,ignore
async fn process_data() -> Result<(), Error> {
    let conn = open_connection().await?;
    let data = fetch_data(&conn).await?;  // What if this is cancelled?
    transform_and_save(data).await?;      // Or this?
    conn.close().await?;                  // May never reach here
    Ok(())
}
```

Three problems:

1. **Cancellation**: if the future is dropped mid-execution, `conn.close()` never runs.
2. **Panic**: if `transform_and_save` panics, the task is torn down and `conn.close()` is skipped.
3. **Async Drop**: `impl Drop for Connection` can only do synchronous cleanup. If closing a connection requires `.await`, you can't do it in `Drop`.

## The Root Cause

RAII relies on `Drop` running synchronously when a value goes out of scope. In async code, "going out of scope" and "running cleanup" are decoupled — by cancellation, by executor scheduling, or because a future is a state machine that might never reach certain states.

## The Solution Preview

id_effect solves this with:

- **`Scope`** / **`Effect::scoped`** — a region where finalizers are registered and guaranteed to run, even on interruption or panic;
- **`ensuring`** — attach one finalizer to one effect;
- **`acquire_use_release`** — pair acquisition with its release, with the right interruptibility;
- **`Pool`** / **`KeyedPool`** — long-lived resources with controlled reuse, checked back in by a scope finalizer.

All of them run cleanup *effects* (not just synchronous `Drop`), and all of them run them however the effect ends: success, failure, defect or interrupt.

# Resources & Scopes — Deterministic Cleanup

RAII works beautifully in synchronous Rust: resources are released when they fall out of scope, `Drop` runs deterministically. In async code, the picture gets complicated.

This chapter shows why RAII breaks down in async contexts, introduces `Scope` and finalizers (`Effect::scoped`, `ensuring`) as the solution, covers `acquire_use_release` for bracket-style resource management, and concludes with `Pool` / `KeyedPool` for reusing expensive connections.

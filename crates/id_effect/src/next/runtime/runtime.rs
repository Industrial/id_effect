//! `Runtime` (executor and root fiber) and its edges: `run`, `try_block_on`, `block_on` (parks a
//! real thread waker), `block_on_future`, `fork`, and `BlockOnInAsync`. Filled by `k-edge`.

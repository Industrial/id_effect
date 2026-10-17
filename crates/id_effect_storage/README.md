# `id_effect_storage`

Object storage for id_effect services: one store interface over the local file system, in-memory
and S3-compatible backends (ADR 0010 Amendment 6).

The crate grows over the 0.10 release. Today it defines `ObjectKey`, a validated object key that
means the same on every backend: 1 to 1024 bytes of `/`-separated segments, none empty, `.` or
`..`, and no control characters, so a key can never escape a file-system store's root.

```rust
use id_effect_storage::{KeyError, ObjectKey};

let key = ObjectKey::new("invoices/2026/10/inv-0042.pdf").unwrap();
assert_eq!(key.file_name(), "inv-0042.pdf");
assert_eq!(ObjectKey::new("../etc/passwd"), Err(KeyError::DotSegment));
```

Tier `integration`.

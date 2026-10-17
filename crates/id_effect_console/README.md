# `id_effect_console`

The operator console for id_effect services: live fibers from the core `fiber-registry` feature,
jobs and queues, behind `id_effect_authz` guards (ADR 0010 Amendment 6).

The crate grows over the 0.10 release. Today it defines `ConsoleConfig`, where the console listens.
A console bound to a loopback address serves the local machine only; on any other address it
requires an authorization guard:

```rust
use id_effect_console::ConsoleConfig;

let local = ConsoleConfig::loopback(9090);
assert_eq!(local.bind().to_string(), "127.0.0.1:9090");
assert!(!local.requires_guard());

let public = ConsoleConfig::new("0.0.0.0:9090".parse().unwrap());
assert!(public.requires_guard());
```

Tier `integration`.

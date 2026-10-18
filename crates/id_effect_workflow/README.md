# `id_effect_workflow`

Durable workflows for id_effect services (ADR 0010 Amendment 7). The engine, a history journal
replayed on resume, on `id_effect_sql` (Postgres) with activities as `id_effect_jobs` jobs, is
being built in this crate for 0.11; so far it holds the workflow name. It replaces the unpublished
`labs/id_effect_workflow` crate of the same name.

`WorkflowName` is the validated name a workflow is registered and started under: 1 to 128 ASCII
letters, digits, `.`, `_` or `-`.

```rust
use id_effect_workflow::{WorkflowName, WorkflowNameError};

let name = WorkflowName::new("close-account").unwrap();
assert_eq!(name.as_str(), "close-account");
assert_eq!(WorkflowName::new(""), Err(WorkflowNameError::Empty));
```

`cargo run -p id_effect_workflow --example 010_names` validates a few names.

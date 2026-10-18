---
title: 'CO — 0.9 carry-over fixes: r10-co-sql-errors'
slug: r10-co-sql-errors
mode: heavy
work_type: spec-slice
risk_class: medium
version: 1
acceptance_criteria:
  - '`SqlError` gains `#[non_exhaustive]` variants for connection and TLS failures and a typed SQLSTATE on plain `query`/`execute` (unique violation detectable without message parsing — used by E7.1 and E7.2); `PgAdvisoryLease::from_pool_config` honours explicit TLS settings. Tests `unique_violation_has_sqlstate_23505`, `tls_required_against_plain_server_is_tls_error`, `advisory_lease_uses_pool_tls`.'
  - 'Gate: `<pg> -p id_effect_sql -p id_effect_node --all-features`'
  - 'Owns: `crates/id_effect_sql/src/{error.rs,pg/error.rs,pg/client.rs}`, `crates/id_effect_node/src/singleton.rs`'
---

# r10-co-sql-errors

Leaf of `.maestro/specs/releases/0.10.0.md`, section "CO — 0.9 carry-over fixes (S, wave 1)". The release spec's row is
binding; this file exists so Maestro can track the leaf as its own task.

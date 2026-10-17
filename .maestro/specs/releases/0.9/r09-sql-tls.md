---
title: 'id_effect_sql: TLS for PostgreSQL and MySQL connections'
slug: r09-sql-tls
mode: heavy
work_type: spec-slice
risk_class: high
version: 1
acceptance_criteria:
  - 'sqlx is built with rustls TLS for the pg and mysql features (aws-lc-rs provider, matching the rustls / jsonwebtoken already in the tree; no OpenSSL). `sslmode` (pg) and `ssl-mode` (mysql) in the URL are honoured, including verify-full / VERIFY_IDENTITY with a custom root CA.'
  - 'PgPoolConfig / MySqlPoolConfig expose the TLS mode and root certificate explicitly (not only through the URL), with a documented default. The default must not silently downgrade: document what `prefer` means and recommend `require` / verify modes for production.'
  - 'Live tests against TLS-enabled PostgreSQL and MySQL containers (self-signed CA generated in the test): a verify-full connection succeeds, a wrong CA fails with a typed error, and a server without TLS fails under `require`. SKIP without Docker; fail under ID_EFFECT_REQUIRE_DOCKER=1.'
  - 'Book ch28-01 Backends: the plaintext note is replaced by a TLS section; CHANGELOG; MSRV 1.94; licences MIT/Apache/ISC.'
---

# r09-sql-tls

Found while integrating the MySQL driver: id_effect_sql builds sqlx without a TLS feature, so
PostgreSQL and MySQL connections were plaintext (already true in 0.8.0). Recorded as a 0.9 exit item
in `.maestro/missions/release-0-9.execution.md`.

---
title: 'E7.3 — Mail, `id_effect_mail`: r10-e7-3-transports'
slug: r10-e7-3-transports
mode: heavy
work_type: spec-slice
risk_class: medium
version: 1
acceptance_criteria:
  - '`Mailer` capability; `SmtpMailer` (lettre per D9, pooled, rustls) and `SendGridMailer` over `HttpClient` (#1); attachments and inline `cid:` images (#8); `TestMailbox` with `assert_sent_to/subject/contains` (#4). Tests: wiremock SendGrid request shape and error mapping; MIME structure of attachments/inline; `cargo deny check` green.'
  - 'Gate: `cargo nextest run -p id_effect_mail --all-features`; `cargo deny check`'
  - 'Owns: `crates/id_effect_mail/src/{message.rs,mailer.rs,smtp.rs,sendgrid.rs,testing.rs}`, `crates/id_effect_mail/Cargo.toml`'
---

# r10-e7-3-transports

Leaf of `.maestro/specs/releases/0.10.0.md`, section "E7.3 — Mail, `id_effect_mail` (L, wave 2) · msg.email #1 #2 #3 #4 #5 #8". The release spec's row is
binding; this file exists so Maestro can track the leaf as its own task.

---
title: 'E7.3 — Mail, `id_effect_mail`: r10-e7-3-deliver-later'
slug: r10-e7-3-deliver-later
mode: heavy
work_type: spec-slice
risk_class: medium
version: 1
acceptance_criteria:
  - '`deliver_later(mail)` / `deliver_later_in(&SqlTransaction, mail)` enqueue a `SendMail` job in `id_effect_jobs` with retries (#5). Test `deliver_later_sends_after_commit_only`. Chapter + example + integration test.'
  - 'Gate: `<pg> -p id_effect_mail --all-features`; `bash scripts/tier-conformance.sh id_effect_mail`'
  - 'Owns: `crates/id_effect_mail/src/later.rs` (feature `jobs`), `book/src/part6/ch31-03-mail.md`, `crates/id_effect_mail/examples/010_mail.rs`, `crates/id_effect_mail/tests/mail.rs`'
---

# r10-e7-3-deliver-later

Leaf of `.maestro/specs/releases/0.10.0.md`, section "E7.3 — Mail, `id_effect_mail` (L, wave 2) · msg.email #1 #2 #3 #4 #5 #8". The release spec's row is
binding; this file exists so Maestro can track the leaf as its own task.

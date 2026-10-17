---
title: 'E7.3 — Mail, `id_effect_mail`: r10-e7-3-templates-preview'
slug: r10-e7-3-templates-preview
mode: heavy
work_type: spec-slice
risk_class: medium
version: 1
acceptance_criteria:
  - 'minijinja HTML + text templates with layouts and partials (#2); `MailPreviews` router at `/_mail/previews` lists and renders registered previews, re-reading templates from disk in the dev profile (#3); Mailpit container (`axllent/mailpit` pinned, SMTP 1025, API 8025). Tests `layout_wraps_html_and_text`, `preview_index_lists_registered_mails`, container test `smtp_delivery_reaches_mailpit`.'
  - 'Gate: `<docker> -p id_effect_mail -p id_effect_test --all-features`'
  - 'Owns: `crates/id_effect_mail/src/{template.rs,preview.rs}`, `crates/id_effect_test/src/containers/mailpit.rs` (+ one `mod` line)'
---

# r10-e7-3-templates-preview

Leaf of `.maestro/specs/releases/0.10.0.md`, section "E7.3 — Mail, `id_effect_mail` (L, wave 2) · msg.email #1 #2 #3 #4 #5 #8". The release spec's row is
binding; this file exists so Maestro can track the leaf as its own task.

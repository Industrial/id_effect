---
title: Platform Application Host
slug: platform-application
mode: heavy
work_type: initiative
risk_class: medium
version: 1
acceptance_criteria:
  - "id_effect_axum::server: lifecycle, graceful shutdown, config bootstrap"
  - "Session and JWT/OAuth trait surfaces; CSRF/CSP security middleware"
  - "Part VI ch30 book chapter"
  - "Workspace tests clippy coverage book pass"
non_goals:
  - Full identity provider implementation
  - Django-admin parity
---

# Platform Application Host

Application shell in id_effect_axum::server; auth traits in id_effect_platform::auth.

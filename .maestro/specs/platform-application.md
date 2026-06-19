---
title: Platform Application Host
slug: platform-application
mode: heavy
work_type: initiative
risk_class: medium
version: 1
acceptance_criteria:
  - "id_effect_host crate: lifecycle, graceful shutdown, config bootstrap"
  - "Session and JWT/OAuth trait surfaces; CSRF/CSP security middleware"
  - "Part VI ch30 book chapter"
  - "Workspace tests clippy coverage book pass"
non_goals:
  - Full identity provider implementation
  - Django-admin parity
---

# Platform Application Host

Application shell, auth, and security middleware for Axum hosts.

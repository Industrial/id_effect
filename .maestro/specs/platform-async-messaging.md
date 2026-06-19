---
title: Platform Async Messaging
slug: platform-async-messaging
mode: heavy
work_type: initiative
risk_class: medium
version: 1
acceptance_criteria:
  - "id_effect_jobs runner; transactional outbox pattern"
  - "Kafka adapter stub; id_effect_events SQL journal hardening"
  - "Part VI ch31 book chapter"
  - "Workspace tests clippy coverage book pass"
non_goals:
  - Managed queue SaaS integrations
  - Full RabbitMQ production adapter in v1
---

# Platform Async Messaging

Jobs, queues, outbox, and production event journal. Depends on platform-data.

---
title: 'E3.4 — `id_effect_redis`: r09-e3-4-redis'
slug: r09-e3-4-redis
mode: heavy
work_type: spec-slice
risk_class: medium
version: 1
acceptance_criteria:
  - '`RedisLayer` (pooled connection manager); typed KV/hash/JSON store API (D6); `CacheBackend` implemented by the in-process cache and by Redis so `Cache` code runs on either (#2); `RedisSessionStore: SessionStore`; rate-limit store for E4.1b; pub/sub as `Stream`. Tests against a Redis container. Book `ch28-04-redis.md`, example.'
  - 'Gate: `ID_EFFECT_REQUIRE_DOCKER=1 cargo nextest run -p id_effect_redis`; tier-conformance'
  - 'Owns: `crates/id_effect_redis/**`; `crates/id_effect/src/resource/cache.rs` (additive `CacheBackend` trait)'
---

# r09-e3-4-redis

Leaf of `.maestro/specs/releases/0.9.0.md`, section "E3.4 — `id_effect_redis` (M, wave 2, stretch) · data.cache #2, data.backends #8". The release spec's row is
binding; this file exists so Maestro can track the leaf as its own task.

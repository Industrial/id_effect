# id_effect_resilience

Resilience primitives for [`id_effect`](https://github.com/Industrial/id_effect) programs:

- **Circuit breaker** — fail fast after repeated errors
- **Rate limiter** — token-bucket admission control
- **Bulkhead** — cap concurrent in-flight work
- **Hedged requests** — race a delayed backup against a primary effect

See the mdBook chapter *Runtime resilience* (`part5/ch21-00-runtime-resilience.md`).

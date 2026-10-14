# capability — dependency injection

Trait-first DI: [`Env`](env.rs), [`ProviderSpec`](provider.rs), [`CapabilityGraph`](graph.rs), [`run_with`](run.rs).

## Author flow

1. Declare a service type (trait or struct) — no generated key types.
2. `#[derive(ProviderSpecDerive)]` + `#[provides(MyService, with = || MyService::new())]` on a provider struct
3. `fn app() -> Effect<_, _, caps!(MyService)>` with `require!(MyService)` or `~MyService` inside `effect!`
   (name a multi-service list once: `type AppCaps = caps!(MyService, EffectLogger);`)
4. `run_with([provide!(MyLive)], app())`

Missing capabilities are reported with context: lookups (`Env::get`, `Env::try_get`, `Needs::need`,
`~Key`, `~inner()` narrowing) are `#[track_caller]` and name the caller's `file:line`
([`CapabilityError::MissingAt`](error.rs)); capability traits carry `#[diagnostic::on_unimplemented]`
messages for compile-time mistakes.

Typed capability sets use [`CapList`](set.rs) via the [`caps!`](../../id_effect_macro/src/capability/caps.rs) macro.
Scoped overrides: [`Env::scoped`](env.rs). Test doubles: [`mock_capability!`](../../id_effect_macro/src/capability/mock.rs).

See `examples/040_capability_app.rs` and ADRs 0002–0003.
See also [ADR 0004](../../../docs/adrs/0004-provider-parity-and-cap-subtyping.md) for optional/refreshable/shared providers and cap subtyping.

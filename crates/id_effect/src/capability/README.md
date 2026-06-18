# capability — dependency injection

Trait-first DI: [`Env`](env.rs), [`ProviderSpec`](provider.rs), [`CapabilityGraph`](graph.rs), [`run_with`](run.rs).

## Author flow

1. `#[capability(T)]` on a struct or trait → `{Name}Key`
2. `#[derive(ProviderSpecDerive)]` + `#[provides(MyKey)]` on a provider struct
3. `fn app() -> Effect<_, _, caps!(MyKey)>` with `require!(MyKey)` inside `effect!`
4. `run_with([provide!(MyLive)], app())`

Typed capability sets use [`CapList`](set.rs) via the [`caps!`](../../id_effect_macro/src/capability/caps.rs) macro.
Scoped overrides: [`Env::scoped`](env.rs). Test doubles: [`mock_capability!`](../../id_effect_macro/src/capability/mock.rs).

See `examples/040_capability_app.rs` and ADRs 0002–0003.
See also [ADR 0004](../../../docs/adrs/0004-provider-parity-and-cap-subtyping.md) for optional/refreshable/shared providers and cap subtyping.

# Providers — Building Your Dependency Graph

You've seen how `R` encodes what an effect needs and how [`Env`](../../src/capability/env.rs) holds values at runtime. But who *builds* the environment?

In small programs you can call `Env::insert` by hand. In real applications you declare **providers**: types with [`#[derive(ProviderSpecDerive)]`](../../src/capability/provider.rs) and [`#[provides(Key)]`](../../src/capability/provider.rs) that know how to construct each capability, optionally reading other capabilities from a partially-built `Env`.

Pass providers to [`run_with`](../../src/capability/run.rs) and [`CapabilityGraph`](../../src/capability/graph.rs) topologically sorts them from each provider's `requires()` metadata. No manual ordering.

This chapter covers `ProviderSpec`, dependent providers, and composing provider lists for production and tests.

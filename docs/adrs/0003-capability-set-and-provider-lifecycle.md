# ADR 0003 — CapabilitySet, provider lifecycle, and advanced DI

## Status

Accepted, amended 2026-10-15 for 0.7 (see [Amendment 1](#amendment-1-2026-10-15-for-07)):
superseded in part by [ADR 0011](0011-fiber-runtime-send-typed-r.md) (typed R).

## Context

ADR 0002 shipped v2 runtime DI (`Env`, `ProviderSpec`, `run_with`) but left `caps!` as a stub expanding to `Env`, and `CapabilitySet::verify` as a no-op. Authors still pass explicit `env` to `require!` and platform keys remain public.

## Decision

### CapabilitySet encoding

Use **wrapper types** `CapEnvN<K1, …, Kn>` as the `Effect` `R` parameter:

- `caps!()` → `Env` (no required-cap verification beyond provider graph)
- `caps!(K1, K2, …)` → `CapEnvN<…>` wrapping built `Env`, implementing `Deref<Target=Env>`
- `CapabilitySet::verify(&Env)` checks all required keys present before `run_with` executes the app effect

Const-generic cap arrays deferred; wrapper types falsify quickly and integrate with existing `Effect::new(|env| …)`.

### Named variants

`CapabilityId` gains optional `variant: Option<&'static str>`. Providers declare `fn variant() -> Option<&'static str>`. Graph allows duplicate `TypeId` when variants differ.

### Non-object-safe traits

`Env::insert_any` / `get_any` store `Arc<dyn Any>` for generic cells; compile_fail tests document the exception path.

### Effectful providers

Optional `ProviderSpec::provide_effect` returns `Effect<T, E, Env>`. `run_with` runs effectful providers in topo order before the app effect.

### Scoped env and lifecycle

`Env::scoped(providers)` builds a child env layered on a parent clone. Providers may implement `on_shutdown` invoked reverse-topo after `run_with` completes.

## Consequences

- Semver 2.1.0 for `id_effect`; `id_effect_platform` 0.3.0 when hiding `HttpClientService`.
- `EffectInterface` in `algebra/interface.rs` removed in favor of `Needs<K>` + `ProviderSpec`.

## Amendment 1 (2026-10-15, for 0.7)

Records the 0.7 capability model (mission `id-effect-runtime-0-7`, task
`w2-adr-0003-amend`; brief D5, human gate H1). Where earlier sections, or ADRs 0002,
0004 and 0005, disagree with this one, this one wins. The kernel decision itself is
[ADR 0011](0011-fiber-runtime-send-typed-r.md); the design is
`.maestro/specs/0.7/kernel-design-a-hlist.md` §2.

### What changes

The `R` parameter becomes a **typed HList** (`HCons<H, T>` / `HNil`), and capability
presence is checked by the compiler. Nothing in the 0.7 kernel verifies or looks up a
capability at run time, so `need` can never panic.

| 0.6 (this ADR, 0002, 0004, 0005) | 0.7 |
|---|---|
| `CapList<Ks>` wrapping a runtime `Env` (`capability/set.rs`), `CapKeys` unrolled to 16 (`set.rs:85-115`) | `caps!(A, B, …)` = `HCons<A, HCons<B, …HNil>>`, recursive, no arity limit |
| `Needs<T>` blanket impl for any `Deref<Target = Env>` (`capability/needs.rs:40-49`); a missing key panics (`capability/env.rs:146-155`) | `R: Selector<T, I>` with an inferred index; `~T` and `need::<T>()` resolve at compile time. A missing capability is a compile error with a custom `on_unimplemented` message |
| `HasCap<K>` blanket impl for any `Deref<Target = Env>` (`set.rs:179-183`) | deleted; the bound is `Selector<T, I>` |
| `CapabilitySet::verify` before the app runs (`capability/run.rs:55`) | deleted: only `Effect<_, _, HNil>` is runnable |
| `CapWiden` prefix projection (ADR 0004; `set.rs:192-203`) and `project_at_0` … `project_at_7` (ADR 0005; `set.rs:213-440`) | `Embed<Sub, Is>`: any subset, in any order; `~` narrows automatically |
| `CapBind` runtime verify, then panic (`capability/cap_bind.rs:122-123`) | `narrow::<Wide, _>()`, checked at compile time |
| `fiber_caps::with_override` thread-local overlay (`capability/fiber_caps.rs:53`) | `provide` on a sub-effect (lexical scope) |
| `ProviderSpec::provide(&Env)` / `provide_effect` / sync `on_shutdown` (`capability/provider.rs:46,48,70`) and `run_with` | `Layer<Out, E, In>` values: `merge`, `provide_to`, `fresh`, `scoped` with async finalizers. `#[derive(ProviderSpecDerive)]` keeps its syntax and generates a `fn layer()` |

### Sections of this ADR

- **CapabilitySet encoding:** superseded. The `CapEnvN` wrappers were already replaced by
  `CapList` (ADR 0004); both give way to the HList.
- **"Const-generic cap arrays deferred"** (CapabilitySet encoding): **closed, not doing
  it.** The HList covers what a const-generic array was meant to provide (any arity,
  compile-time membership) without a const-eval set encoding on stable Rust.
- **Named variants:** not part of typed R. A capability is identified by its type, so two
  instances of one service need two types (newtypes). `CapabilityId::variant` survives
  only with `DynEnv` and its planner.
- **Non-object-safe traits:** no exception path is needed. HList cells hold `Arc<T>` for any
  `T: Send + Sync + 'static`, so the `insert_any` / `get_any` route stays only on `DynEnv`.
- **Effectful providers:** become layers built from an effect (`Layer::from_effect`).
- **Scoped env and lifecycle:** become `Layer::scoped`. Shutdown hooks are scope
  finalizers and may be async; they run in reverse acquisition order.

### `DynEnv` survives as a fallible opt-in

The 0.6 `Env` (a `HashMap<TypeId, Arc<dyn Any + Send + Sync>>`, `capability/env.rs:15`)
stays as **`DynEnv`**, an explicit escape hatch for plugin-style wiring whose keys are not
known at compile time. Its lookups are fallible only (`try_need -> Result<_,
CapabilityError>`); it has no panicking `get`. The dynamic provider planner
(`CapabilityGraph`) stays only for `DynEnv`.

### Known costs

- Ambiguity: a capability that appears twice in `R` (for example two merged layers that
  need the same input) is E0283 with no custom message. ADR 0011 records the mitigations
  (H1 condition 2: `Layer::merge` dedupes or rejects with a custom diagnostic).
- Generic helpers over `R` carry the index parameter (`R: Selector<Logger, I>`). H1
  condition 3 asks for a documented pattern or a macro.

### Implementation

The typed-R kernel lands in `id_effect::next` (`k-rebuild`). The 0.6 surface in the
left-hand column stays in place, unchanged, until `k-retire-v1` deletes it, so the earlier
sections still describe the v1 kernel during the transition.

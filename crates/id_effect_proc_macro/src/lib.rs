//! Procedural macros for the workspace `effect` crate.
//!
//! Doc links cannot use the `id_effect::…` prefix here: this crate defines an `effect` function, which
//! shadows the `effect` crate name in rustdoc link resolution.
#![allow(rustdoc::broken_intra_doc_links)]
#![deny(missing_docs)]

mod derive_fsm;
mod derive_optics;
mod derive_schema_parser;
mod distributed_behavior;
mod effect_data;
mod effect_macro;
mod effect_tagged;
mod match_effect;
mod provider_spec;

use proc_macro::TokenStream;

/// Compiles and runs the README examples as doctests.
#[cfg(doctest)]
#[doc = include_str!("../README.md")]
struct ReadmeDoctests;

/// Derive macro: structural [`PartialEq`], [`Eq`], and [`Hash`] for Effect.ts-style data types.
///
/// Types implementing these impls automatically satisfy [`id_effect::schema::data::EffectData`] via the
/// blanket implementation in the `effect` crate.
#[proc_macro_derive(EffectData)]
pub fn derive_effect_data(input: TokenStream) -> TokenStream {
  effect_data::derive_effect_data(input)
}

/// Injects `pub _tag: &'static str`, an [`id_effect::match_::HasTag`] impl, and
/// `EFFECT_TAGGED_TAG` on the struct (see generated inherent associated const).
///
/// Only supports structs with **named fields**. Place **above** `#[derive(EffectData, …)]`.
#[proc_macro_attribute]
pub fn effect_tagged(attr: TokenStream, item: TokenStream) -> TokenStream {
  effect_tagged::expand(attr, item)
}

/// Derive an inherent `layer()` (an `id_effect::Layer` providing the service) from
/// `#[provides(Service)]`. Re-exported by `id_effect` as `ProviderSpecDerive`.
///
/// `Struct::layer::<E>()` is a `Layer<HCons<Service, HNil>, E>` that needs nothing and never
/// fails. The service value comes from, in order of precedence:
///
/// - `#[provides(Service, with = <constructor>)]` — `|| expr`, or a path such as `Self::build`
///   called with no arguments. This is the recommended form: no `fn new() -> Service` (which
///   trips `clippy::new_ret_no_self`) is needed.
/// - `Self::default()` when the struct derives `Default` (the provider is its own service).
/// - `Self::new()` otherwise.
///
/// `#[provides(Service, release = <finalizer>)]` makes it an `acquire_release` layer whose
/// finalizer `release(Arc<Service>)` (an `Effect<(), Never, HNil>`) runs when the layer's scope
/// closes. The 0.6 forms `|deps: &Env| ..` and `#[named("..")]` are compile errors.
///
/// ```ignore
/// #[derive(Clone)]
/// struct Counter(u32);
///
/// #[derive(ProviderSpecDerive)]
/// #[provides(Counter, with = || Counter(42))]
/// struct CounterLive;
///
/// let program = app().provide_layer(CounterLive::layer());
/// ```
#[proc_macro_derive(ProviderSpec, attributes(provides, named))]
pub fn derive_provider_spec(input: TokenStream) -> TokenStream {
  provider_spec::derive(input)
}

/// Enum match helper — prefix variant patterns with `EnumPath` for exhaustiveness checking.
///
/// ```ignore
/// match_effect!(Color, value, {
///     Red(n) => n,
///     Green => 0,
///     Blue => 1,
/// })
/// ```
#[proc_macro]
pub fn match_effect(input: TokenStream) -> TokenStream {
  match_effect::expand(input)
}

/// Do-notation over an async body, building an `id_effect::Effect`.
///
/// `effect!({ body })` takes `R` from the expected type; `effect!(|r: &Caps| { body })` fixes
/// `R = Caps` and binds `r: &Caps`. In the body, `~Cap` reads a capability (`&Arc<Cap>`),
/// `~expr` runs a sub-effect over a part of `R` on the same fiber, widening its error (`Never`
/// to anything, else `E: From<E2>`), and `~(a, b, ..)` joins up to 8 effects in parallel on
/// child fibers. `?` on a `Result` fails the effect with its error. The body must be `Send`.
///
/// ```
/// use id_effect::{Effect, Exit, Never, caps, effect, succeed};
///
/// struct Greeting(&'static str);
///
/// fn answer() -> Effect<u32> {
///   succeed(42)
/// }
///
/// fn hello() -> Effect<String, Never, caps!(Greeting)> {
///   effect!({ format!("{}:{}", ~Greeting.0, ~answer()) })
/// }
///
/// let out = hello().provide(Greeting("hi")).block_on();
/// assert!(matches!(out, Exit::Success(ref s) if s == "hi:42"));
/// ```
#[proc_macro]
pub fn effect(input: TokenStream) -> TokenStream {
  effect_macro::effect::expand(input, false)
}

/// Like `effect!`, but builds an `id_effect::LocalEffect`, whose body may hold `!Send` values.
/// `~expr` binds a `LocalEffect` or a `Send` `Effect` over a part of `R`.
#[proc_macro]
pub fn effect_local(input: TokenStream) -> TokenStream {
  effect_macro::effect::expand(input, true)
}

/// `caps!(A, B, C)` is the environment type `HCons<A, HCons<B, HCons<C, HNil>>>`. A capability
/// listed twice (compared by token) is one compile error at the repeat; the de-duplicated list
/// is still emitted so no further errors cascade.
#[proc_macro]
pub fn caps(input: TokenStream) -> TokenStream {
  effect_macro::caps::expand(input)
}

/// Derive field lenses and enum prisms for [`id_effect_optics`].
#[proc_macro_derive(Optics, attributes(lens, prism, optional, optics))]
pub fn derive_optics(input: TokenStream) -> TokenStream {
  derive_optics::derive_optics(input)
}

/// Derive `id_effect_fsm::Fsm` (initial state + transition table) for a fieldless state enum.
///
/// Use it through the re-export `id_effect_fsm::Fsm`; the generated impl names
/// `::id_effect_fsm` unless overridden with `crate = path`.
///
/// - Container: `#[fsm(event = EventType, initial = Variant, crate = path)]` — `event` is
///   required; `initial` defaults to the first variant.
/// - Variant: `#[fsm(on(Event => Target, ..))]`, repeatable. A bare `Event` resolves to
///   `EventType::Event`; a multi-segment path is used as written.
///
/// Unknown targets, unknown `initial`, data-carrying variants, generic enums, and duplicate
/// `(state, event)` edges are compile errors.
///
/// ```ignore
/// use id_effect_fsm::Fsm;
///
/// #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
/// enum DoorEvent { Open, Close }
///
/// #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Fsm)]
/// #[fsm(event = DoorEvent, initial = Closed)]
/// enum Door {
///   #[fsm(on(Open => Opened))]
///   Closed,
///   #[fsm(on(Close => Closed))]
///   Opened,
/// }
///
/// let mut door = Door::state_machine();
/// door.step(DoorEvent::Open).unwrap();
/// assert_eq!(door.state(), Door::Opened);
/// ```
#[proc_macro_derive(Fsm, attributes(fsm))]
pub fn derive_fsm(input: TokenStream) -> TokenStream {
  derive_fsm::derive(input)
}

/// Derive an inherent `schema()` constructor and an [`id_effect::schema::HasSchema`] impl for a
/// struct with named fields (JSON-shaped [`id_effect::schema::Unknown`] wire).
///
/// Rename a field on the wire with `#[schema("wire_name")]`.
///
/// The schema carries a `SchemaAst` (`schema().ast()`), so JSON Schema export (`schema-serde`)
/// describes the struct: every field is `required` except `Option<_>` fields (missing or `null`),
/// `i32` fields carry their range, and nested `HasSchema` fields are `$ref`s into `$defs` keyed by
/// type path (recursive types terminate). Enums are not supported.
///
/// ```
/// use id_effect::Schema;
/// use id_effect::schema::{HasSchema, Unknown};
///
/// #[derive(Clone, Debug, PartialEq, Schema)]
/// struct User {
///   #[schema("user_name")]
///   name: String,
///   age: i64,
/// }
///
/// let wire = User::schema().encode(User { name: "Ada".into(), age: 36 });
/// let back = <User as HasSchema>::schema().decode_unknown(&wire).unwrap();
/// assert_eq!(back.name, "Ada");
/// assert!(matches!(wire, Unknown::Object(_)));
/// ```
#[proc_macro_derive(Schema, attributes(schema))]
pub fn derive_schema(input: TokenStream) -> TokenStream {
  derive_schema_parser::derive_schema(input)
}

/// Mark a process struct as a remotely spawnable behavior.
///
/// ```ignore
/// #[distributed_behavior("counter")]
/// struct Counter { ... }
/// // Counter::BEHAVIOR_NAME == "counter"
/// // Counter::register_behavior(&mut registry);
/// ```
#[proc_macro_attribute]
pub fn distributed_behavior(attr: TokenStream, item: TokenStream) -> TokenStream {
  distributed_behavior::expand(attr, item)
}

//! Procedural macros for the workspace `effect` crate.
//!
//! Doc links cannot use the `id_effect::…` prefix here: this crate defines an `effect` function, which
//! shadows the `effect` crate name in rustdoc link resolution.
#![allow(rustdoc::broken_intra_doc_links)]
#![deny(missing_docs)]

mod capability_attr;
mod derive_fsm;
mod derive_optics;
mod derive_schema_parser;
mod distributed_behavior;
mod effect_data;
mod effect_graph;
mod effect_tagged;
mod expand;
mod infer_caps;
mod match_effect;
mod parse;
mod provider_spec;
mod transform;

use proc_macro::TokenStream;

/// Compiles and runs the README examples as doctests.
#[cfg(doctest)]
#[doc = include_str!("../README.md")]
struct ReadmeDoctests;

/// Derive macro: structural [`PartialEq`], [`Eq`], and [`Hash`] for Effect.ts-style data types.
///
/// Types implementing these impls automatically satisfy [`id_effect::data::EffectData`] via the
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

/// **Deprecated no-op** (since 0.3; removed in 0.7): returns the item unchanged.
///
/// Capability keys are no longer generated — use the service type itself in `caps!(Service)`,
/// `Needs::<Service>` and `#[provides(Service)]`. Delete the attribute. (Attribute macros
/// cannot carry `#[deprecated]`, so there is no compiler warning.)
#[proc_macro_attribute]
pub fn capability(attr: TokenStream, item: TokenStream) -> TokenStream {
  capability_attr::expand(attr, item)
}

/// Derive [`id_effect::ProviderSpec`] when paired with `#[provides(Capability)]`.
///
/// The service value comes from, in order of precedence:
///
/// - `#[provides(Service, with = <constructor>)]` — `|| expr`, `|deps: &Env| expr` (reads
///   already-built capabilities), or a path such as `Self::build` called with no arguments.
///   This is the recommended form: no `fn new() -> Service` (which trips
///   `clippy::new_ret_no_self`) is needed.
/// - `Self::default()` when the struct derives `Default` (the provider is its own service).
/// - `Self::new()` otherwise.
///
/// Add `#[named("variant")]` for a named variant of the same capability.
///
/// ```ignore
/// #[derive(Clone)]
/// struct Counter(u32);
///
/// #[derive(ProviderSpecDerive)]
/// #[provides(Counter, with = || Counter(42))]
/// struct CounterLive;
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

/// Procedural do-notation macro for [`id_effect::Effect`].
///
/// See the `effect` crate documentation for usage.
#[proc_macro]
pub fn effect(input: TokenStream) -> TokenStream {
  let input = proc_macro2::TokenStream::from(input);
  let kind = match parse::parse_effect_input(input) {
    Ok(k) => k,
    Err(e) => return e.to_compile_error().into(),
  };
  match expand::expand(kind) {
    Ok(ts) => ts.into(),
    Err(e) => e.to_compile_error().into(),
  }
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

/// Deprecated alias of [`macro@Schema`]; expands to exactly the same code.
///
/// Earlier releases also generated a `parser()` method backed by the unpublished `id_effect_parse`
/// crate; that method is gone. Build a text parser with
/// `id_effect_parse::SchemaBridge::parser_for::<T>()` instead.
///
/// Derive macros cannot carry `#[deprecated]`, so the compiler will not warn on use — migrate to
/// `#[derive(Schema)]`. This alias will be removed in a future minor release.
#[proc_macro_derive(SchemaParser, attributes(schema, parser))]
pub fn derive_schema_parser(input: TokenStream) -> TokenStream {
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

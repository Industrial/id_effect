//! `#[derive(Fsm)]` — generate an `id_effect_fsm::Fsm` impl (initial state + transition table)
//! from attributes on a fieldless state enum.
//!
//! ```text
//! #[fsm(event = EventPath, initial = Variant, crate = path)]   // container; initial/crate optional
//! enum State {
//!   #[fsm(on(Event => Target, Other => Target2))]               // repeatable per variant
//!   Variant,
//! }
//! ```
//!
//! A single-identifier event (`Open`) resolves to `EventPath::Open`; a multi-segment path is
//! used verbatim. Targets must be variants of the deriving enum.

use std::collections::HashSet;

use proc_macro::TokenStream;
use proc_macro2::{Span, TokenStream as TokenStream2};
use quote::quote;
use syn::parse::{Parse, ParseStream};
use syn::punctuated::Punctuated;
use syn::{Data, DeriveInput, Fields, Ident, Path, Token, parse_macro_input};

pub(crate) fn derive(input: TokenStream) -> TokenStream {
  let input = parse_macro_input!(input as DeriveInput);
  match expand(input) {
    Ok(ts) => ts.into(),
    Err(e) => e.to_compile_error().into(),
  }
}

/// One `Event => Target` edge inside `on(...)`.
struct Edge {
  event: Path,
  target: Ident,
}

impl Parse for Edge {
  fn parse(input: ParseStream<'_>) -> syn::Result<Self> {
    let event: Path = input.parse()?;
    input.parse::<Token![=>]>()?;
    let target: Ident = input.parse()?;
    Ok(Self { event, target })
  }
}

#[derive(Default)]
struct ContainerAttrs {
  event: Option<Path>,
  initial: Option<Ident>,
  krate: Option<Path>,
}

fn parse_container(input: &DeriveInput) -> syn::Result<ContainerAttrs> {
  let mut attrs = ContainerAttrs::default();
  for attr in input.attrs.iter().filter(|a| a.path().is_ident("fsm")) {
    attr.parse_nested_meta(|meta| {
      if meta.path.is_ident("event") {
        attrs.event = Some(meta.value()?.parse()?);
      } else if meta.path.is_ident("initial") {
        attrs.initial = Some(meta.value()?.parse()?);
      } else if meta.path.is_ident("crate") {
        attrs.krate = Some(meta.value()?.parse()?);
      } else {
        return Err(meta.error("expected `event = ..`, `initial = ..` or `crate = ..`"));
      }
      Ok(())
    })?;
  }
  Ok(attrs)
}

fn parse_edges(variant: &syn::Variant) -> syn::Result<Vec<Edge>> {
  let mut edges = Vec::new();
  for attr in variant.attrs.iter().filter(|a| a.path().is_ident("fsm")) {
    attr.parse_nested_meta(|meta| {
      if meta.path.is_ident("on") {
        let content;
        syn::parenthesized!(content in meta.input);
        let parsed = Punctuated::<Edge, Token![,]>::parse_terminated(&content)?;
        edges.extend(parsed);
        Ok(())
      } else {
        Err(meta.error("expected `on(Event => Target, ..)`"))
      }
    })?;
  }
  Ok(edges)
}

pub(crate) fn expand(input: DeriveInput) -> syn::Result<TokenStream2> {
  let name = &input.ident;
  let Data::Enum(data) = &input.data else {
    return Err(syn::Error::new(
      Span::call_site(),
      "#[derive(Fsm)] only supports enums (one variant per state)",
    ));
  };
  if !input.generics.params.is_empty() {
    return Err(syn::Error::new_spanned(
      &input.generics,
      "#[derive(Fsm)] does not support generic state enums",
    ));
  }
  let attrs = parse_container(&input)?;
  let event = attrs.event.ok_or_else(|| {
    syn::Error::new(
      Span::call_site(),
      "#[derive(Fsm)] needs `#[fsm(event = YourEventType)]` on the enum",
    )
  })?;
  let krate = attrs
    .krate
    .unwrap_or_else(|| syn::parse_quote!(::id_effect_fsm));

  let mut variants = HashSet::new();
  for v in &data.variants {
    if !matches!(v.fields, Fields::Unit) {
      return Err(syn::Error::new_spanned(
        v,
        "#[derive(Fsm)] states must be fieldless variants",
      ));
    }
    variants.insert(v.ident.to_string());
  }

  let initial = match attrs.initial {
    Some(initial) => {
      if !variants.contains(&initial.to_string()) {
        return Err(syn::Error::new_spanned(
          &initial,
          format!("`initial = {initial}` is not a variant of `{name}`"),
        ));
      }
      initial
    }
    None => match data.variants.first() {
      Some(v) => v.ident.clone(),
      None => {
        return Err(syn::Error::new(
          Span::call_site(),
          "#[derive(Fsm)] needs at least one state variant",
        ));
      }
    },
  };

  let mut seen = HashSet::new();
  let mut ons = Vec::new();
  for v in &data.variants {
    let from = &v.ident;
    for edge in parse_edges(v)? {
      let target = &edge.target;
      if !variants.contains(&target.to_string()) {
        return Err(syn::Error::new_spanned(
          target,
          format!("transition target `{target}` is not a variant of `{name}`"),
        ));
      }
      let ev = &edge.event;
      // Compare by the variant (final path segment) so `Go` and `E::Go`
      // count as the same event; otherwise the later edge would silently
      // overwrite the earlier one in the generated table.
      let variant = ev
        .segments
        .last()
        .map(|s| s.ident.to_string())
        .unwrap_or_default();
      let key = (from.to_string(), variant);
      if !seen.insert(key) {
        return Err(syn::Error::new_spanned(
          ev,
          format!("duplicate transition from `{from}` on `{}`", quote!(#ev)),
        ));
      }
      let event_expr = match ev.get_ident() {
        Some(ident) => quote!(#event::#ident),
        None => quote!(#ev),
      };
      ons.push(quote!(.on(Self::#from, #event_expr, Self::#target)));
    }
  }

  Ok(quote! {
    #[automatically_derived]
    impl #krate::Fsm for #name {
      type Event = #event;

      fn initial() -> Self {
        Self::#initial
      }

      fn transition_table() -> #krate::TransitionTable<Self, #event> {
        #krate::TransitionTable::new() #(#ons)*
      }
    }
  })
}

#[cfg(test)]
mod tests {
  use super::*;

  fn expand_str(src: &str) -> Result<String, String> {
    let input: DeriveInput = syn::parse_str(src).expect("valid item");
    expand(input)
      .map(|ts| ts.to_string())
      .map_err(|e| e.to_string())
  }

  #[test]
  fn generates_table_with_resolved_event_paths() {
    let out = expand_str(
      "#[fsm(event = DoorEvent, initial = Closed)]
       enum Door {
         #[fsm(on(Open => Opened))] Closed,
         #[fsm(on(Close => Closed), on(other::Slam => Closed))] Opened,
       }",
    )
    .unwrap();
    assert!(
      out.contains("impl :: id_effect_fsm :: Fsm for Door"),
      "{out}"
    );
    assert!(out.contains("type Event = DoorEvent"), "{out}");
    assert!(
      out.contains("Self :: Closed , DoorEvent :: Open , Self :: Opened"),
      "{out}"
    );
    assert!(
      out.contains("Self :: Opened , other :: Slam , Self :: Closed"),
      "{out}"
    );
  }

  #[test]
  fn initial_defaults_to_first_variant_and_crate_is_overridable() {
    let out = expand_str(
      "#[fsm(event = E, crate = my::fsm)]
       enum S { A, B }",
    )
    .unwrap();
    assert!(out.contains("impl my :: fsm :: Fsm for S"), "{out}");
    assert!(out.contains("fn initial () -> Self { Self :: A }"), "{out}");
  }

  #[test]
  fn rejects_missing_event() {
    let err = expand_str("enum S { A }").unwrap_err();
    assert!(err.contains("event = YourEventType"), "{err}");
  }

  #[test]
  fn rejects_structs_and_generics() {
    assert!(
      expand_str("#[fsm(event = E)] struct S;")
        .unwrap_err()
        .contains("only supports enums")
    );
    assert!(
      expand_str("#[fsm(event = E)] enum S<T> { A(T) }")
        .unwrap_err()
        .contains("generic")
    );
  }

  #[test]
  fn rejects_data_carrying_variants() {
    let err = expand_str("#[fsm(event = E)] enum S { A(u8) }").unwrap_err();
    assert!(err.contains("fieldless"), "{err}");
  }

  #[test]
  fn rejects_unknown_target_and_initial() {
    let err = expand_str("#[fsm(event = E)] enum S { #[fsm(on(Go => Nowhere))] A }").unwrap_err();
    assert!(err.contains("`Nowhere` is not a variant"), "{err}");
    let err = expand_str("#[fsm(event = E, initial = Z)] enum S { A }").unwrap_err();
    assert!(err.contains("`initial = Z`"), "{err}");
  }

  #[test]
  fn rejects_duplicate_edges() {
    let err =
      expand_str("#[fsm(event = E)] enum S { #[fsm(on(Go => A), on(Go => B))] A, B }").unwrap_err();
    assert!(err.contains("duplicate transition from `A`"), "{err}");
  }

  #[test]
  fn rejects_duplicate_edges_spelled_with_different_paths() {
    let err =
      expand_str("#[fsm(event = E)] enum S { #[fsm(on(Go => A, E::Go => B))] A, B }").unwrap_err();
    assert!(err.contains("duplicate transition from `A`"), "{err}");
    let err =
      expand_str("#[fsm(event = E)] enum S { #[fsm(on(crate::E::Go => A), on(Go => B))] A, B }")
        .unwrap_err();
    assert!(err.contains("duplicate transition from `A`"), "{err}");
  }

  #[test]
  fn same_event_from_different_states_is_not_a_duplicate() {
    let out =
      expand_str("#[fsm(event = E)] enum S { #[fsm(on(Go => B))] A, #[fsm(on(E::Go => A))] B }")
        .unwrap();
    assert!(out.contains("Self :: B , E :: Go , Self :: A"), "{out}");
  }

  #[test]
  fn rejects_unknown_keys() {
    let err = expand_str("#[fsm(event = E, colour = Red)] enum S { A }").unwrap_err();
    assert!(err.contains("expected `event"), "{err}");
    let err = expand_str("#[fsm(event = E)] enum S { #[fsm(off(Go => A))] A }").unwrap_err();
    assert!(err.contains("expected `on("), "{err}");
  }

  #[test]
  fn rejects_empty_enum() {
    let err = expand_str("#[fsm(event = E)] enum S {}").unwrap_err();
    assert!(err.contains("at least one state"), "{err}");
  }
}

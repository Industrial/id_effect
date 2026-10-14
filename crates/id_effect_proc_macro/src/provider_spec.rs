//! `#[derive(ProviderSpec)]` with `#[provides(Service)]`.

use proc_macro::TokenStream as ProcTokenStream;
use proc_macro2::TokenStream;
use quote::quote;
use syn::parse::{Parse, ParseStream};
use syn::{DeriveInput, Expr, Path, Token, Type, punctuated::Punctuated, token::Comma};

pub fn derive(input: ProcTokenStream) -> ProcTokenStream {
  derive2(input.into()).into()
}

fn derive2(input: TokenStream) -> TokenStream {
  let input = match syn::parse2::<DeriveInput>(input) {
    Ok(v) => v,
    Err(e) => return e.to_compile_error(),
  };
  let struct_name = &input.ident;

  let ProvidesArgs {
    service: service_ty,
    with,
  } = match find_provides_service(&input) {
    Ok(args) => args,
    Err(e) => return e.to_compile_error(),
  };

  let variant = find_named_variant(&input);
  let provider_id = provider_id_from_ident(struct_name);
  let variant_impl = match &variant {
    Some(lit) => quote! { Some(#lit) },
    None => quote! { None },
  };
  let construct = match with {
    Some(expr) => construct_with(expr),
    None if has_derive_default(&input.attrs) => quote! { Self::default() },
    None => quote! { Self::new() },
  };

  quote! {
    impl ::id_effect::ProviderSpec for #struct_name {
      type Key = ::id_effect::Cap<#service_ty>;
      type Output = #service_ty;

      fn provider_id() -> &'static str {
        #provider_id
      }

      fn variant() -> Option<&'static str> {
        #variant_impl
      }

      fn provide(_deps: &::id_effect::Env) -> ::core::result::Result<Self::Output, ::id_effect::ProviderError> {
        Ok(#construct)
      }
    }
  }
}

/// `#[provides(Service)]` or `#[provides(Service, with = <constructor>)]`.
struct ProvidesArgs {
  service: Type,
  with: Option<Expr>,
}

impl Parse for ProvidesArgs {
  fn parse(input: ParseStream<'_>) -> syn::Result<Self> {
    let service = input.parse::<Type>()?;
    let mut with = None;
    if input.parse::<Option<Token![,]>>()?.is_some() && !input.is_empty() {
      let key = input.parse::<syn::Ident>()?;
      if key != "with" {
        return Err(syn::Error::new_spanned(
          key,
          "expected `with = <constructor>` (e.g. `#[provides(Counter, with = || Counter(0))]`)",
        ));
      }
      input.parse::<Token![=]>()?;
      with = Some(input.parse::<Expr>()?);
      let _ = input.parse::<Option<Token![,]>>()?;
    }
    if !input.is_empty() {
      return Err(input.error("unexpected tokens after `#[provides(Service, with = …)]`"));
    }
    Ok(Self { service, with })
  }
}

fn find_provides_service(input: &DeriveInput) -> syn::Result<ProvidesArgs> {
  for attr in &input.attrs {
    if attr.path().is_ident("provides") {
      return attr.parse_args::<ProvidesArgs>();
    }
  }
  Err(syn::Error::new_spanned(
    &input.ident,
    "derive(ProviderSpec) requires #[provides(Service)] on the struct",
  ))
}

/// Expression building the service from a `with = …` constructor.
///
/// - `|| expr` → `expr` (inlined, so no closure call is generated)
/// - `|deps| expr` / `|deps: &Env| expr` → `{ let deps = _deps; expr }` (reads already-built deps)
/// - any other expression (`Self::build`, `make_counter`) → called with no arguments
fn construct_with(expr: Expr) -> TokenStream {
  match expr {
    Expr::Closure(closure) if closure.inputs.is_empty() => {
      let body = closure.body;
      quote! { #body }
    }
    Expr::Closure(closure) if closure.inputs.len() == 1 => {
      let pat = &closure.inputs[0];
      let body = &closure.body;
      quote! { { let #pat = _deps; #body } }
    }
    Expr::Closure(closure) => syn::Error::new_spanned(
      &closure.inputs,
      "`with` closure takes no arguments or one `&Env` argument",
    )
    .to_compile_error(),
    other => quote! { (#other)() },
  }
}

fn has_derive_default(attrs: &[syn::Attribute]) -> bool {
  attrs.iter().any(|attr| {
    if !attr.path().is_ident("derive") {
      return false;
    }
    attr
      .parse_args_with(Punctuated::<Path, Comma>::parse_terminated)
      .map(|paths| {
        paths
          .iter()
          .any(|p| p.is_ident("Default") || p.segments.last().is_some_and(|s| s.ident == "Default"))
      })
      .unwrap_or(false)
  })
}

fn provider_id_from_ident(ident: &syn::Ident) -> String {
  let raw = ident.to_string();
  let base = raw.strip_suffix("Live").unwrap_or(&raw);
  let mut snake = String::new();
  for (i, ch) in base.chars().enumerate() {
    if ch.is_uppercase() && i > 0 {
      snake.push('-');
    }
    snake.push(ch.to_ascii_lowercase());
  }
  if raw.ends_with("Live") {
    format!("{snake}-live")
  } else {
    snake
  }
}

fn find_named_variant(input: &DeriveInput) -> Option<syn::LitStr> {
  for attr in &input.attrs {
    if attr.path().is_ident("named")
      && let Ok(lit) = attr.parse_args::<syn::LitStr>()
    {
      return Some(lit);
    }
  }
  None
}

#[cfg(test)]
mod tests {
  use super::*;
  use quote::quote;
  use syn::parse_quote;

  #[test]
  fn provider_id_strips_live_suffix() {
    assert_eq!(
      provider_id_from_ident(&parse_quote!(CounterLive)),
      "counter-live"
    );
    assert_eq!(provider_id_from_ident(&parse_quote!(DbPool)), "db-pool");
  }

  #[test]
  fn derive_emits_provider_spec_impl() {
    let input = quote! {
      #[provides(Counter)]
      #[derive(Default)]
      struct CounterLive;
    };
    let out = derive2(input);
    let out = out.to_string();
    assert!(out.contains("impl :: id_effect :: ProviderSpec for CounterLive"));
    assert!(out.contains("type Key = :: id_effect :: Cap < Counter >"));
    assert!(out.contains("type Output = Counter"));
    assert!(out.contains("counter-live"));
    assert!(out.contains("Self :: default ()"));
  }

  #[test]
  fn missing_provides_attr_errors() {
    let input = quote! {
      struct CounterLive;
    };
    let out = derive2(input);
    assert!(out.to_string().contains("requires"));
    assert!(out.to_string().contains("provides"));
  }

  #[test]
  fn derive_with_named_variant() {
    let input = quote! {
      #[provides(Db)]
      #[named("replica")]
      struct DbReplicaLive;
    };
    let out = derive2(input).to_string();
    assert!(out.contains("replica"));
  }

  #[test]
  fn with_zero_arg_closure_inlines_body() {
    let input = quote! {
      #[provides(Counter, with = || Counter(42))]
      struct CounterLive;
    };
    let out = derive2(input).to_string();
    assert!(out.contains("Ok (Counter (42))"), "{out}");
    assert!(!out.contains("Self :: new"), "{out}");
  }

  #[test]
  fn with_deps_closure_binds_env() {
    let input = quote! {
      #[provides(Repo, with = |deps: &Env| Repo::new(deps))]
      struct RepoLive;
    };
    let out = derive2(input).to_string();
    assert!(out.contains("let deps : & Env = _deps"), "{out}");
  }

  #[test]
  fn with_path_is_called() {
    let input = quote! {
      #[provides(Counter, with = Self::build,)]
      struct CounterLive;
    };
    let out = derive2(input).to_string();
    assert!(out.contains("(Self :: build) ()"), "{out}");
  }

  #[test]
  fn provides_rejects_unknown_key_and_multi_arg_closure() {
    let bad_key = quote! {
      #[provides(Counter, make = Self::build)]
      struct CounterLive;
    };
    assert!(derive2(bad_key).to_string().contains("expected `with"));
    let bad_closure = quote! {
      #[provides(Counter, with = |a, b| a)]
      struct CounterLive;
    };
    assert!(
      derive2(bad_closure)
        .to_string()
        .contains("no arguments or one")
    );
    let trailing = quote! {
      #[provides(Counter, with = Self::build, extra)]
      struct CounterLive;
    };
    assert!(derive2(trailing).to_string().contains("unexpected tokens"));
  }
}

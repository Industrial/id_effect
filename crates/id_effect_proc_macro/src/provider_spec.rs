//! `#[derive(ProviderSpec)]` with `#[provides(Key)]`.

use proc_macro::TokenStream as ProcTokenStream;
use proc_macro2::TokenStream;
use quote::quote;
use syn::{DeriveInput, Path, Type, punctuated::Punctuated, token::Comma};

pub fn derive(input: ProcTokenStream) -> ProcTokenStream {
  derive2(input.into()).into()
}

fn derive2(input: TokenStream) -> TokenStream {
  let input = match syn::parse2::<DeriveInput>(input) {
    Ok(v) => v,
    Err(e) => return e.to_compile_error(),
  };
  let struct_name = &input.ident;

  let key_ty = match find_provides_key(&input) {
    Ok(key) => key,
    Err(e) => return e.to_compile_error(),
  };

  let variant = find_named_variant(&input);
  let provider_id = provider_id_from_ident(struct_name);
  let variant_impl = match &variant {
    Some(lit) => quote! { Some(#lit) },
    None => quote! { None },
  };
  let construct = if has_derive_default(&input.attrs) {
    quote! { Self::default() }
  } else {
    quote! { Self::new() }
  };

  quote! {
    impl ::id_effect::ProviderSpec for #struct_name {
      type Key = #key_ty;
      type Output = <#key_ty as ::id_effect::CapabilityKey>::Value;

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

fn find_provides_key(input: &DeriveInput) -> syn::Result<Type> {
  for attr in &input.attrs {
    if attr.path().is_ident("provides") {
      return attr.parse_args::<Type>();
    }
  }
  Err(syn::Error::new_spanned(
    &input.ident,
    "derive(ProviderSpec) requires #[provides(Key)] on the struct",
  ))
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

fn find_named_variant(input: &syn::DeriveInput) -> Option<syn::LitStr> {
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
      #[provides(CounterKey)]
      #[derive(Default)]
      struct CounterLive;
    };
    let out = derive2(input);
    let out = out.to_string();
    assert!(out.contains("impl :: id_effect :: ProviderSpec for CounterLive"));
    assert!(out.contains("type Key = CounterKey"));
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
}

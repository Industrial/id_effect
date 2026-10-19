//! Expansion of `#[service]`.

use proc_macro2::TokenStream;
use quote::ToTokens;
use syn::ItemTrait;

/// Parses the annotated item as a trait and emits it unchanged.
///
/// The attribute arguments (`version = N`) are read by leaf `r12-e12-1-macro`; they are
/// accepted as-is until then.
pub(crate) fn expand(_attr: TokenStream, item: TokenStream) -> syn::Result<TokenStream> {
  let service: ItemTrait = syn::parse2(item).map_err(|err| {
    syn::Error::new(
      err.span(),
      "#[service] goes on a trait\n  help: declare the procedures as trait methods: `trait Greeter { fn greet(&self, name: String) -> String; }`",
    )
  })?;
  Ok(service.into_token_stream())
}

#[cfg(test)]
mod tests {
  use super::*;
  use quote::quote;

  #[test]
  fn a_trait_expands_to_itself() {
    let item = quote! { trait Greeter { fn greet(&self, name: String) -> String; } };
    let out = expand(TokenStream::new(), item.clone()).expect("a trait is accepted");
    assert_eq!(out.to_string(), item.to_string());
  }

  #[test]
  fn a_struct_is_rejected_with_a_help_line() {
    let err = expand(TokenStream::new(), quote! { struct Greeter; }).expect_err("not a trait");
    assert!(err.to_string().contains("help:"));
  }
}

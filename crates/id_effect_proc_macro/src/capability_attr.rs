//! Deprecated: `#[capability]` is a no-op. Use service types directly with `caps!` and `#[provides]`.

use proc_macro::TokenStream;
use proc_macro2::TokenStream as TokenStream2;

pub fn expand(_attr: TokenStream, item: TokenStream) -> TokenStream {
  expand_item_tokens(item.into()).into()
}

/// Passthrough body (testable without `proc_macro::TokenStream`).
pub(crate) fn expand_item_tokens(item: TokenStream2) -> TokenStream2 {
  item
}

#[cfg(test)]
mod tests {
  use super::*;
  use quote::quote;

  #[test]
  fn expand_item_is_passthrough() {
    let item = quote! { struct LegacyMarker; };
    let out = expand_item_tokens(item.clone());
    assert_eq!(out.to_string(), item.to_string());
  }
}

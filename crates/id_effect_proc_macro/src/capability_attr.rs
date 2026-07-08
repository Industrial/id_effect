//! Deprecated: `#[capability]` is a no-op. Use service types directly with `caps!` and `#[provides]`.

use proc_macro::TokenStream;

pub fn expand(_attr: TokenStream, item: TokenStream) -> TokenStream {
  item
}

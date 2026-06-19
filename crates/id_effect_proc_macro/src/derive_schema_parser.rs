//! Stub `#[derive(SchemaParser)]` — reserved for future schema-driven parser codegen.

use proc_macro::TokenStream;
use quote::quote;
use syn::{DeriveInput, parse_macro_input};

/// Expand a minimal stub impl so callers can opt in before full codegen lands.
pub fn derive_schema_parser(input: TokenStream) -> TokenStream {
  let input = parse_macro_input!(input as DeriveInput);
  let ident = &input.ident;
  let (impl_generics, ty_generics, where_clause) = input.generics.split_for_impl();

  quote! {
    impl #impl_generics #ident #ty_generics #where_clause {
      #[doc(hidden)]
      #[allow(dead_code)]
      pub const __SCHEMA_PARSER_DERIVE_STUB: () = ();
    }
  }
  .into()
}

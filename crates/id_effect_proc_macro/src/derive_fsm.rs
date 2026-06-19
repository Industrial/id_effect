//! Stub `#[derive(Fsm)]` — reserved for future `id_effect_fsm` transition tables.

use proc_macro::TokenStream;
use quote::quote;
use syn::{DeriveInput, parse_macro_input};

/// Expand a minimal stub impl so callers can opt in before full codegen lands.
pub fn derive_fsm(input: TokenStream) -> TokenStream {
  let input = parse_macro_input!(input as DeriveInput);
  let ident = &input.ident;
  let (impl_generics, ty_generics, where_clause) = input.generics.split_for_impl();

  quote! {
    impl #impl_generics #ident #ty_generics #where_clause {
      #[doc(hidden)]
      #[allow(dead_code)]
      pub const __FSM_DERIVE_STUB: () = ();
    }
  }
  .into()
}

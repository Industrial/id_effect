//! `#[derive(EffectData)]` — field-wise `PartialEq`, `Eq`, and `Hash`.

use proc_macro::TokenStream;
use quote::{format_ident, quote};
use syn::{
  Data, DeriveInput, Fields, GenericParam, Ident, Index, TypeParamBound, parse_macro_input,
};

fn add_effect_data_bounds(generics: &mut syn::Generics) {
  for param in &mut generics.params {
    if let GenericParam::Type(tp) = param {
      if !tp
        .bounds
        .iter()
        .any(|b| matches!(b, TypeParamBound::Trait(t) if t.path.is_ident("Hash")))
      {
        tp.bounds.push(syn::parse_quote!(::core::hash::Hash));
      }
      if !tp
        .bounds
        .iter()
        .any(|b| matches!(b, TypeParamBound::Trait(t) if t.path.is_ident("Eq")))
      {
        tp.bounds.push(syn::parse_quote!(::core::cmp::Eq));
      }
      if !tp.bounds.iter().any(|b| {
        matches!(
          b,
          TypeParamBound::Trait(t) if t.path.is_ident("PartialEq")
        )
      }) {
        tp.bounds.push(syn::parse_quote!(::core::cmp::PartialEq));
      }
    }
  }
}

pub fn derive_effect_data(input: TokenStream) -> TokenStream {
  let mut input = parse_macro_input!(input as DeriveInput);
  add_effect_data_bounds(&mut input.generics);
  let ident = &input.ident;
  let (impl_generics, ty_generics, where_clause) = input.generics.split_for_impl();

  let (partial_eq, hash) = match &input.data {
    Data::Struct(data) => (
      gen_struct_partial_eq(&data.fields),
      gen_struct_hash(&data.fields),
    ),
    Data::Enum(data) => (gen_enum_partial_eq(ident, data), gen_enum_hash(ident, data)),
    Data::Union(u) => {
      return syn::Error::new_spanned(u.union_token, "EffectData does not support unions")
        .to_compile_error()
        .into();
    }
  };

  quote! {
    impl #impl_generics ::core::cmp::PartialEq for #ident #ty_generics #where_clause {
      #[inline]
      fn eq(&self, other: &Self) -> bool {
        #partial_eq
      }
    }

    impl #impl_generics ::core::cmp::Eq for #ident #ty_generics #where_clause {}

    impl #impl_generics ::core::hash::Hash for #ident #ty_generics #where_clause {
      #[inline]
      fn hash<H: ::core::hash::Hasher>(&self, state: &mut H) {
        #hash
      }
    }
  }
  .into()
}

fn gen_struct_partial_eq(fields: &Fields) -> proc_macro2::TokenStream {
  match fields {
    Fields::Named(named) => {
      let names: Vec<&Ident> = named
        .named
        .iter()
        .map(|f| f.ident.as_ref().expect("named field must have ident"))
        .collect();
      quote! {
        #( self.#names == other.#names )&&*
      }
    }
    Fields::Unnamed(unnamed) => {
      let idx: Vec<Index> = (0..unnamed.unnamed.len()).map(Index::from).collect();
      quote! {
        #( self.#idx == other.#idx )&&*
      }
    }
    Fields::Unit => quote! { true },
  }
}

fn gen_enum_partial_eq(ident: &syn::Ident, data: &syn::DataEnum) -> proc_macro2::TokenStream {
  let arms = data.variants.iter().map(|v| {
    let var = &v.ident;
    match &v.fields {
      Fields::Named(named) => {
        let names: Vec<&Ident> = named
          .named
          .iter()
          .map(|f| f.ident.as_ref().expect("named field must have ident"))
          .collect();
        let lb: Vec<Ident> = names.iter().map(|n| format_ident!("__l_{}", n)).collect();
        let rb: Vec<Ident> = names.iter().map(|n| format_ident!("__r_{}", n)).collect();
        // Edition 2024: avoid `ref` here — matching `&Self` uses binding modes; explicit `ref` conflicts.
        quote! {
          (#ident::#var { #( #names: #lb, )* }, #ident::#var { #( #names: #rb, )* }) => {
            true #( && (#lb == #rb) )*
          }
        }
      }
      Fields::Unnamed(unnamed) => {
        let n = unnamed.unnamed.len();
        let lb: Vec<_> = (0..n).map(|i| format_ident!("__l{}", i)).collect();
        let rb: Vec<_> = (0..n).map(|i| format_ident!("__r{}", i)).collect();
        quote! {
          (#ident::#var(#( #lb, )*), #ident::#var(#( #rb, )*)) => {
            true #( && (#lb == #rb) )*
          }
        }
      }
      Fields::Unit => quote! {
        (#ident::#var, #ident::#var) => true,
      },
    }
  });

  quote! {
    match (self, other) {
      #( #arms )*
      _ => false,
    }
  }
}

fn gen_struct_hash(fields: &Fields) -> proc_macro2::TokenStream {
  match fields {
    Fields::Named(named) => {
      let names: Vec<&Ident> = named
        .named
        .iter()
        .map(|f| f.ident.as_ref().expect("named field must have ident"))
        .collect();
      quote! {
        #( ::core::hash::Hash::hash(&self.#names, state); )*
      }
    }
    Fields::Unnamed(unnamed) => {
      let idx: Vec<Index> = (0..unnamed.unnamed.len()).map(Index::from).collect();
      quote! {
        #( ::core::hash::Hash::hash(&self.#idx, state); )*
      }
    }
    Fields::Unit => quote! {},
  }
}

fn gen_enum_hash(ident: &syn::Ident, data: &syn::DataEnum) -> proc_macro2::TokenStream {
  let arms = data.variants.iter().map(|v| {
    let var = &v.ident;
    match &v.fields {
      Fields::Named(named) => {
        let names: Vec<&Ident> = named
          .named
          .iter()
          .map(|f| f.ident.as_ref().expect("named field must have ident"))
          .collect();
        quote! {
          #ident::#var { #( #names, )* } => {
            #( ::core::hash::Hash::hash(#names, state); )*
          }
        }
      }
      Fields::Unnamed(unnamed) => {
        let n = unnamed.unnamed.len();
        let binds: Vec<Ident> = (0..n).map(|i| format_ident!("__v{}", i)).collect();
        quote! {
          #ident::#var(#( #binds, )*) => {
            #( ::core::hash::Hash::hash(#binds, state); )*
          }
        }
      }
      Fields::Unit => quote! {
        #ident::#var => {}
      },
    }
  });

  quote! {
    ::core::mem::discriminant(self).hash(state);
    match self {
      #( #arms )*
    }
  }
}

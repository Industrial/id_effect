//! `#[capability]` — generate a capability key and [`CapabilityKey`](::id_effect::CapabilityKey) impl.

use proc_macro::TokenStream as ProcTokenStream;
use proc_macro2::TokenStream;
use quote::{format_ident, quote};
use syn::parse::{Parse, ParseStream};
use syn::{Ident, Item, ItemStruct, ItemTrait, Type};

struct CapabilityAttr {
  value_ty: Option<Type>,
}

impl Parse for CapabilityAttr {
  fn parse(input: ParseStream<'_>) -> syn::Result<Self> {
    if input.is_empty() {
      return Ok(CapabilityAttr { value_ty: None });
    }
    Ok(CapabilityAttr {
      value_ty: Some(input.parse()?),
    })
  }
}

fn key_ident(name: &Ident) -> Ident {
  format_ident!("{}Key", name)
}

pub fn expand(attr: ProcTokenStream, item: ProcTokenStream) -> ProcTokenStream {
  expand2(attr.into(), item.into()).into()
}

fn expand2(attr: TokenStream, item: TokenStream) -> TokenStream {
  let CapabilityAttr { value_ty } = match syn::parse2(attr) {
    Ok(v) => v,
    Err(e) => return e.to_compile_error(),
  };
  let item = match syn::parse2(item) {
    Ok(v) => v,
    Err(e) => return e.to_compile_error(),
  };

  match item {
    Item::Trait(trait_item) => expand_trait(trait_item, value_ty),
    Item::Struct(struct_item) => expand_struct(struct_item, value_ty),
    other => syn::Error::new_spanned(other, "#[capability] supports traits and structs only")
      .to_compile_error(),
  }
}

fn expand_trait(trait_item: ItemTrait, value_ty: Option<Type>) -> TokenStream {
  let trait_name = &trait_item.ident;
  let key_name = key_ident(trait_name);
  let value = value_ty.unwrap_or_else(|| syn::parse_quote!(dyn #trait_name));

  quote! {
    #trait_item

    #[allow(missing_docs)]
    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
    pub struct #key_name;

    impl ::id_effect::CapabilityKey for #key_name {
      type Value = #value;
    }
  }
}

fn expand_struct(struct_item: ItemStruct, value_ty: Option<Type>) -> TokenStream {
  let struct_name = &struct_item.ident;
  let key_name = key_ident(struct_name);
  let Some(value) = value_ty else {
    return syn::Error::new_spanned(
      struct_name,
      "#[capability(T)] on a struct requires an explicit value type",
    )
    .to_compile_error();
  };

  quote! {
    #struct_item

    #[allow(missing_docs)]
    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
    pub struct #key_name;

    impl ::id_effect::CapabilityKey for #key_name {
      type Value = #value;
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use quote::quote;

  #[test]
  fn trait_without_args_generates_dyn_value() {
    let item = quote! {
      pub trait Database {
        fn query(&self, sql: &str);
      }
    };
    let out = expand2(TokenStream::new(), item);
    let out = out.to_string();
    assert!(out.contains("struct DatabaseKey"));
    assert!(out.contains("type Value = dyn Database"));
  }

  #[test]
  fn trait_with_named_value_type() {
    let attr = quote! { DatabaseLive };
    let item = quote! {
      pub trait Database {
        fn query(&self, sql: &str);
      }
    };
    let out = expand2(attr, item);
    let out = out.to_string();
    assert!(out.contains("type Value = DatabaseLive"));
  }

  #[test]
  fn struct_requires_value_type() {
    let item = quote! {
      struct Counter;
    };
    let out = expand2(TokenStream::new(), item);
    assert!(out.to_string().contains("requires an explicit value type"));
  }

  #[test]
  fn struct_with_value_type_generates_key() {
    let attr = quote! { u32 };
    let item = quote! {
      struct Counter;
    };
    let out = expand2(attr, item);
    let out = out.to_string();
    assert!(out.contains("struct CounterKey"));
    assert!(out.contains("type Value = u32"));
  }
}

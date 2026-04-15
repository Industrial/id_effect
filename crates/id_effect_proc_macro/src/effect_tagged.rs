//! `#[effect_tagged("name")]` — inject `_tag` field and [`HasTag`](::id_effect::match_::HasTag).

use proc_macro::TokenStream;
use quote::quote;
use syn::parse::{Parse, ParseStream};
use syn::{Fields, ItemStruct, LitStr, parse_macro_input};

struct TagArg(LitStr);

impl Parse for TagArg {
  fn parse(input: ParseStream<'_>) -> syn::Result<Self> {
    Ok(TagArg(input.parse()?))
  }
}

pub fn expand(attr: TokenStream, item: TokenStream) -> TokenStream {
  let TagArg(tag) = parse_macro_input!(attr as TagArg);
  let mut s = parse_macro_input!(item as ItemStruct);
  let ident = &s.ident;

  let named = match &mut s.fields {
    Fields::Named(n) => n,
    _ => {
      return syn::Error::new_spanned(
        ident,
        "effect_tagged only supports structs with named fields",
      )
      .to_compile_error()
      .into();
    }
  };

  named
    .named
    .insert(0, syn::parse_quote! { pub _tag: &'static str });

  quote! {
    #s

    impl #ident {
      pub const EFFECT_TAGGED_TAG: &'static str = #tag;
    }

    impl ::id_effect::match_::HasTag for #ident {
      fn tag(&self) -> &str {
        self._tag
      }
    }
  }
  .into()
}

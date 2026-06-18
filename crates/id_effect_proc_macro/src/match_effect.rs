//! `match_effect!` — enum match with a shared path prefix for exhaustiveness-friendly arms.

use proc_macro::TokenStream;
use quote::quote;
use syn::parse::{Parse, ParseStream};
use syn::{Token, braced, parse_macro_input};

struct MatchEffectInput {
  enum_path: syn::Path,
  _comma: Token![,],
  scrutinee: syn::Expr,
  _comma2: Token![,],
  arms: MatchArms,
}

struct MatchArms {
  _brace_token: syn::token::Brace,
  arms: syn::punctuated::Punctuated<MatchArm, Token![,]>,
}

struct MatchArm {
  pat: syn::Pat,
  _fat_arrow: Token![=>],
  body: syn::Expr,
}

impl Parse for MatchEffectInput {
  fn parse(input: ParseStream<'_>) -> syn::Result<Self> {
    Ok(MatchEffectInput {
      enum_path: input.parse()?,
      _comma: input.parse()?,
      scrutinee: input.parse()?,
      _comma2: input.parse()?,
      arms: input.parse()?,
    })
  }
}

impl Parse for MatchArms {
  fn parse(input: ParseStream<'_>) -> syn::Result<Self> {
    let content;
    Ok(MatchArms {
      _brace_token: braced!(content in input),
      arms: content.parse_terminated(MatchArm::parse, Token![,])?,
    })
  }
}

impl Parse for MatchArm {
  fn parse(input: ParseStream<'_>) -> syn::Result<Self> {
    Ok(MatchArm {
      pat: input.call(syn::Pat::parse_single)?,
      _fat_arrow: input.parse()?,
      body: input.parse()?,
    })
  }
}

fn qualify_pat(enum_path: &syn::Path, pat: &syn::Pat) -> syn::Pat {
  match pat {
    syn::Pat::Ident(ident) => syn::parse_quote! { #enum_path :: #ident },
    syn::Pat::TupleStruct(tuple) if tuple.path.get_ident().is_some() => {
      let variant = tuple.path.get_ident().unwrap();
      let mut qualified = tuple.clone();
      qualified.path = syn::parse_quote!(#enum_path::#variant);
      syn::Pat::TupleStruct(qualified)
    }
    syn::Pat::Struct(strukt) if strukt.path.get_ident().is_some() => {
      let variant = strukt.path.get_ident().unwrap();
      let mut qualified = strukt.clone();
      qualified.path = syn::parse_quote!(#enum_path::#variant);
      syn::Pat::Struct(qualified)
    }
    syn::Pat::Path(path) if path.path.get_ident().is_some() && path.path.segments.len() == 1 => {
      let variant = path.path.get_ident().unwrap();
      syn::parse_quote! { #enum_path :: #variant }
    }
    _ => pat.clone(),
  }
}

pub fn expand(input: TokenStream) -> TokenStream {
  let MatchEffectInput {
    enum_path,
    scrutinee,
    arms,
    ..
  } = parse_macro_input!(input as MatchEffectInput);

  let match_arms = arms.arms.into_iter().map(|arm| {
    let pat = qualify_pat(&enum_path, &arm.pat);
    let body = arm.body;
    quote! { #pat => #body }
  });

  let expanded = quote! {
    match #scrutinee {
      #(#match_arms,)*
    }
  };

  expanded.into()
}

#[cfg(test)]
mod tests {
  use super::*;
  use syn::parse_quote;

  #[test]
  fn qualify_pat_handles_unit_and_struct_forms() {
    let path = parse_quote!(MyEnum);
    let unit = parse_quote!(Variant);
    let tuple = parse_quote!(Tuple(x, y));
    let strukt = parse_quote!(Record { field });
    let wild: syn::Pat = parse_quote!(_);
    assert!(matches!(qualify_pat(&path, &unit), syn::Pat::Path(_)));
    assert!(matches!(
      qualify_pat(&path, &tuple),
      syn::Pat::TupleStruct(_)
    ));
    assert!(matches!(qualify_pat(&path, &strukt), syn::Pat::Struct(_)));
    assert!(matches!(qualify_pat(&path, &wild), syn::Pat::Wild(_)));
    let already: syn::Pat = parse_quote!(MyEnum::Variant);
    assert!(matches!(qualify_pat(&path, &already), syn::Pat::Path(_)));
  }
}

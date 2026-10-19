//! Helpers shared by `#[service]` and `#[derive(RpcErrorUnion)]`.

use proc_macro2::{Span, TokenStream};
use quote::{quote, quote_spanned};
use syn::spanned::Spanned;
use syn::{Attribute, Expr, ExprLit, GenericArgument, Lit, Meta, PathArguments, Type};

/// The reserved failure tags of the 0.12 design note §4 (`id_effect_rpc::wire::RpcFailure`).
pub(crate) const RESERVED_TAGS: [&str; 6] = [
  "InternalError",
  "InvalidInput",
  "DeadlineExceeded",
  "Cancelled",
  "ResourceExhausted",
  "ResumeExpired",
];

/// An error whose message ends with a `help:` line (K4: the first error states the fix).
pub(crate) fn error(span: Span, message: impl AsRef<str>, help: impl AsRef<str>) -> syn::Error {
  syn::Error::new(
    span,
    format!("{}\n  help: {}", message.as_ref(), help.as_ref()),
  )
}

/// `::id_effect_rpc::service`, the runtime support module.
pub(crate) fn rt() -> TokenStream {
  quote!(::id_effect_rpc::service)
}

/// The last path segment of `ty` and its type arguments (`Option<T>` → `("Option", [T])`).
pub(crate) fn segment(ty: &Type) -> Option<(String, Vec<&Type>)> {
  let Type::Path(path) = ty else {
    return None;
  };
  if path.qself.is_some() {
    return None;
  }
  let last = path.path.segments.last()?;
  let args = match &last.arguments {
    PathArguments::AngleBracketed(args) => args
      .args
      .iter()
      .filter_map(|arg| match arg {
        GenericArgument::Type(ty) => Some(ty),
        _ => None,
      })
      .collect(),
    _ => Vec::new(),
  };
  Some((last.ident.to_string(), args))
}

/// The `WireSchema<T>` expression for `ty`: the built-in types by name, `Option` and `Vec`
/// recursively, anything else through `RpcSchema` (spanned at the type, so a missing Schema is
/// reported there).
pub(crate) fn wire_schema(ty: &Type) -> TokenStream {
  let rt = rt();
  match ty {
    Type::Tuple(tuple) if tuple.elems.is_empty() => quote!(#rt::types::unit()),
    Type::Paren(inner) => wire_schema(&inner.elem),
    Type::Group(inner) => wire_schema(&inner.elem),
    _ => match segment(ty) {
      Some((name, args)) => match (name.as_str(), args.as_slice()) {
        ("String", []) => quote!(#rt::types::string()),
        ("bool", []) => quote!(#rt::types::bool()),
        ("i64", []) => quote!(#rt::types::i64()),
        ("f64", []) => quote!(#rt::types::f64()),
        ("Option", [inner]) => {
          let inner = wire_schema(inner);
          quote!(#rt::types::option(#inner))
        }
        ("Vec", [inner]) => {
          let inner = wire_schema(inner);
          quote!(#rt::types::vec(#inner))
        }
        _ => quote_spanned!(ty.span()=> #rt::types::of::<#ty>()),
      },
      None => quote_spanned!(ty.span()=> #rt::types::of::<#ty>()),
    },
  }
}

/// Whether `ty` is an `Option<_>` (an optional error field).
pub(crate) fn is_option(ty: &Type) -> bool {
  segment(ty).is_some_and(|(name, args)| name == "Option" && args.len() == 1)
}

/// The `///` doc comment of an item, one leading space stripped per line; `None` when empty.
pub(crate) fn docs(attrs: &[Attribute]) -> Option<String> {
  let lines: Vec<String> = attrs
    .iter()
    .filter(|attr| attr.path().is_ident("doc"))
    .filter_map(|attr| match &attr.meta {
      Meta::NameValue(nv) => match &nv.value {
        Expr::Lit(ExprLit {
          lit: Lit::Str(s), ..
        }) => Some(s.value()),
        _ => None,
      },
      _ => None,
    })
    .map(|line| line.strip_prefix(' ').unwrap_or(&line).to_owned())
    .collect();
  let text = lines.join("\n").trim().to_owned();
  (!text.is_empty()).then_some(text)
}

/// Whether `name` is a valid service or procedure name: 1 to 64 of `[A-Za-z0-9_-]`.
pub(crate) fn is_valid_name(name: &str) -> bool {
  !name.is_empty()
    && name.len() <= 64
    && name
      .bytes()
      .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

#[cfg(test)]
mod tests {
  use super::*;
  use syn::parse_quote;

  fn schema(ty: Type) -> String {
    wire_schema(&ty).to_string().replace(' ', "")
  }

  #[test]
  fn built_in_types_map_by_name_and_others_go_through_rpc_schema() {
    assert!(schema(parse_quote!(())).ends_with("types::unit()"));
    assert!(schema(parse_quote!((String))).ends_with("types::string()"));
    assert!(schema(parse_quote!(std::string::String)).ends_with("types::string()"));
    assert!(schema(parse_quote!(bool)).ends_with("types::bool()"));
    assert!(schema(parse_quote!(i64)).ends_with("types::i64()"));
    assert!(schema(parse_quote!(f64)).ends_with("types::f64()"));
    assert!(
      schema(parse_quote!(Option<Vec<i64>>))
        .contains("types::option(::id_effect_rpc::service::types::vec(")
    );
    assert!(schema(parse_quote!(User)).ends_with("types::of::<User>()"));
    assert!(schema(parse_quote!(Vec<User, Alloc>)).contains("of::<Vec<User,Alloc>>"));
    assert!(schema(parse_quote!(&'static str)).contains("of::<&'staticstr>"));
    assert!(schema(parse_quote!(<T as Tr>::A)).contains("of::<"));
    assert!(schema(parse_quote!(Box<dyn Fn()>)).contains("of::<"));
    assert!(schema(parse_quote!(Foo<'a>)).contains("of::<Foo<'a>>"));
    assert!(schema(parse_quote!(Foo::<i64>)).contains("of::<"));
  }

  #[test]
  fn reserved_tags_are_the_wire_failures() {
    assert_eq!(
      RESERVED_TAGS,
      id_effect_rpc::wire::RpcFailure::RESERVED_TAGS
    );
  }

  #[test]
  fn segments_options_names_and_docs() {
    assert!(segment(&parse_quote!(&str)).is_none());
    assert!(segment(&parse_quote!(<T as Tr>::A)).is_none());
    assert!(segment(&parse_quote!(Foo<'a>)).is_some_and(|(_, args)| args.is_empty()));
    let group = Type::Group(syn::TypeGroup {
      group_token: Default::default(),
      elem: Box::new(parse_quote!(i64)),
    });
    assert!(wire_schema(&group).to_string().contains("i64"));
    assert!(is_option(&parse_quote!(Option<i64>)));
    assert!(!is_option(&parse_quote!(i64)));
    assert!(is_valid_name("greet_v2-x"));
    assert!(!is_valid_name(""));
    assert!(!is_valid_name("a.b"));
    assert!(!is_valid_name(&"a".repeat(65)));
    let item: syn::ItemFn = parse_quote! {
      /// Greets
      ///  someone.
      #[doc(hidden)]
      #[doc = 1]
      fn f() {}
    };
    assert_eq!(docs(&item.attrs).as_deref(), Some("Greets\n someone."));
    let bare: syn::ItemFn = parse_quote!(
      fn f() {}
    );
    assert!(docs(&bare.attrs).is_none());
    assert!(
      error(Span::call_site(), "m", "h")
        .to_string()
        .contains("\n  help: h")
    );
  }
}

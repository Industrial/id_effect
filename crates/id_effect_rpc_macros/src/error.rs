//! Expansion of `#[derive(RpcErrorUnion)]`: an enum as a typed error union (design note §4).

use std::collections::BTreeMap;

use proc_macro2::{Span, TokenStream};
use quote::quote;
use syn::spanned::Spanned;
use syn::{Attribute, Data, DeriveInput, Fields, Ident, LitInt, LitStr, Type};

use crate::support::{RESERVED_TAGS, error, is_option, rt, wire_schema};

/// `#[rpc(...)]` on the enum or a variant.
#[derive(Default)]
struct RpcAttrs {
  status: Option<u16>,
  tag: Option<(String, Span)>,
  internal: bool,
}

fn parse_status(lit: &LitInt) -> syn::Result<u16> {
  match lit.base10_parse::<u16>() {
    Ok(code) if (400..=599).contains(&code) => Ok(code),
    _ => Err(error(
      lit.span(),
      format!("`{lit}` is not an error status"),
      "use a 4xx or 5xx status, e.g. `#[rpc(status = 404)]` (the default is 400)",
    )),
  }
}

fn rpc_attrs(attrs: &[Attribute], on_variant: bool) -> syn::Result<RpcAttrs> {
  let mut out = RpcAttrs::default();
  for attr in attrs.iter().filter(|a| a.path().is_ident("rpc")) {
    attr.parse_nested_meta(|meta| {
      if meta.path.is_ident("status") {
        out.status = Some(parse_status(&meta.value()?.parse::<LitInt>()?)?);
      } else if on_variant && meta.path.is_ident("tag") {
        let lit = meta.value()?.parse::<LitStr>()?;
        out.tag = Some((lit.value(), lit.span()));
      } else if on_variant && meta.path.is_ident("internal") {
        out.internal = true;
      } else {
        let allowed = if on_variant {
          "`status = N`, `tag = \"…\"` and `internal`"
        } else {
          "`status = N` (the default status of every variant)"
        };
        return Err(error(
          meta.path.span(),
          "unknown `#[rpc(..)]` argument",
          format!("`#[rpc(..)]` here takes {allowed}"),
        ));
      }
      Ok(())
    })?;
  }
  Ok(out)
}

struct Field {
  ident: Ident,
  name: String,
  ty: Type,
}

struct Variant {
  ident: Ident,
  tag: String,
  status: Option<u16>,
  internal: bool,
  fields: Vec<Field>,
}

fn variant(v: &syn::Variant, seen: &mut BTreeMap<String, ()>) -> syn::Result<Variant> {
  let attrs = rpc_attrs(&v.attrs, true)?;
  let (tag, tag_span) = attrs
    .tag
    .unwrap_or_else(|| (v.ident.to_string(), v.ident.span()));
  if RESERVED_TAGS.contains(&tag.as_str()) {
    return Err(error(
      tag_span,
      format!("`{tag}` is a reserved rpc failure tag"),
      format!(
        "rename the variant or give it another tag with `#[rpc(tag = \"…\")]`; the reserved tags are {}",
        RESERVED_TAGS.join(", ")
      ),
    ));
  }
  if tag.is_empty() {
    return Err(error(
      tag_span,
      "an rpc error tag cannot be empty",
      "give the variant a name as its tag, e.g. `#[rpc(tag = \"NotFound\")]`",
    ));
  }
  if seen.insert(tag.clone(), ()).is_some() {
    return Err(error(
      tag_span,
      format!("two variants have the tag `{tag}`"),
      "every variant of an error union needs its own `_tag`; change one `#[rpc(tag = \"…\")]`",
    ));
  }
  let fields = match &v.fields {
    Fields::Unit => Vec::new(),
    Fields::Named(named) => named
      .named
      .iter()
      .map(|f| {
        let ident = f.ident.clone().expect("named fields have names");
        let name = ident.to_string().trim_start_matches("r#").to_owned();
        if name == "_tag" {
          return Err(error(
            ident.span(),
            "a field named `_tag` clashes with the union tag",
            "rename the field; `_tag` holds the variant's name on the wire",
          ));
        }
        Ok(Field {
          ident,
          name,
          ty: f.ty.clone(),
        })
      })
      .collect::<syn::Result<_>>()?,
    Fields::Unnamed(_) => {
      return Err(error(
        v.fields.span(),
        format!("variant `{}` has unnamed fields", v.ident),
        format!(
          "name them, e.g. `{} {{ id: i64 }}`: every field is a member of the wire object",
          v.ident
        ),
      ));
    }
  };
  Ok(Variant {
    ident: v.ident.clone(),
    tag,
    status: attrs.status,
    internal: attrs.internal,
    fields,
  })
}

/// Implements `RpcErrorStatus`, `RpcErrorUnion` and `RpcErrorDecode` for an enum.
pub(crate) fn expand(item: TokenStream) -> syn::Result<TokenStream> {
  let input: DeriveInput = syn::parse2(item)?;
  let name = &input.ident;
  if !input.generics.params.is_empty() {
    return Err(error(
      input.generics.span(),
      "an rpc error union cannot be generic",
      "use concrete field types: the union's schema is part of the service contract",
    ));
  }
  let Data::Enum(data) = &input.data else {
    return Err(error(
      name.span(),
      "`#[derive(RpcErrorUnion)]` goes on an enum",
      "declare the error as an enum of unit or named-field variants, e.g. `enum TodoError { NotFound { id: i64 } }`",
    ));
  };
  let default_status = rpc_attrs(&input.attrs, false)?.status;
  let mut seen = BTreeMap::new();
  let variants = data
    .variants
    .iter()
    .map(|v| variant(v, &mut seen))
    .collect::<syn::Result<Vec<_>>>()?;

  let rt = rt();
  let p = quote!(#rt::__private);

  let status_arms = variants.iter().map(|v| {
    let ident = &v.ident;
    let code = v.status.or(default_status).unwrap_or(400);
    quote!(Self::#ident { .. } => #p::status(#code))
  });
  let wire_arms = variants.iter().map(|v| {
    let ident = &v.ident;
    let tag = &v.tag;
    let bindings = v.fields.iter().map(|f| &f.ident);
    let inserts = v.fields.iter().map(|f| {
      let (ident, name, schema) = (&f.ident, &f.name, wire_schema(&f.ty));
      quote! {
        map.insert(
          #name.to_owned(),
          #p::encode_field(&#schema, ::core::clone::Clone::clone(#ident)),
        );
      }
    });
    quote! {
      Self::#ident { #(#bindings,)* .. } => {
        let mut map = #p::Map::new();
        map.insert("_tag".to_owned(), #p::Value::from(#tag));
        #(#inserts)*
        #p::Value::Object(map)
      }
    }
  });
  let public: Vec<&Variant> = variants.iter().filter(|v| !v.internal).collect();
  let internal: Vec<&Ident> = variants
    .iter()
    .filter(|v| v.internal)
    .map(|v| &v.ident)
    .collect();
  let is_internal = (!internal.is_empty()).then(|| {
    quote! {
      fn is_internal(&self) -> bool {
        ::core::matches!(self, #(Self::#internal { .. })|*)
      }
    }
  });
  let schemas = public.iter().map(|v| {
    let tag = &v.tag;
    let fields = v.fields.iter().map(|f| {
      let (name, schema, required) = (&f.name, wire_schema(&f.ty), !is_option(&f.ty));
      quote!((#name, #schema.json_schema().clone(), #required))
    });
    quote!(#p::variant_schema(#tag, ::std::vec![#(#fields),*]))
  });
  let decode = if public.is_empty() {
    quote! {
      let _ = value;
      ::core::option::Option::None
    }
  } else {
    let arms = public.iter().map(|v| {
      let (ident, tag) = (&v.ident, &v.tag);
      let fields = v.fields.iter().map(|f| {
        let (fident, fname, schema) = (&f.ident, &f.name, wire_schema(&f.ty));
        quote!(#fident: #p::decode_field(value, #fname, &#schema)?)
      });
      quote!(#tag => ::core::option::Option::Some(Self::#ident { #(#fields),* }))
    });
    quote! {
      match #p::tag_of(value)? {
        #(#arms,)*
        _ => ::core::option::Option::None,
      }
    }
  };
  let (status_body, wire_body) = if variants.is_empty() {
    (quote!(match *self {}), quote!(match *self {}))
  } else {
    (
      quote!(match self { #(#status_arms,)* }),
      quote!(match self { #(#wire_arms)* }),
    )
  };

  Ok(quote! {
    impl ::id_effect_rpc::RpcErrorStatus for #name {
      fn status(&self) -> #p::StatusCode {
        #status_body
      }
    }

    impl ::id_effect_rpc::RpcErrorUnion for #name {
      fn to_wire(&self) -> #p::Value {
        #wire_body
      }

      #is_internal

      fn union_schema() -> ::core::option::Option<#p::Value> {
        #p::union_schema(::std::vec![#(#schemas),*])
      }
    }

    impl #rt::RpcErrorDecode for #name {
      fn from_wire(value: &#p::Value) -> ::core::option::Option<Self> {
        #decode
      }
    }
  })
}

#[cfg(test)]
mod tests {
  use super::*;
  use quote::quote;

  fn ok(item: TokenStream) -> String {
    expand(item).expect("expands").to_string()
  }

  fn err(item: TokenStream) -> String {
    let message = expand(item).expect_err("refused").to_string();
    assert!(message.contains("\n  help: "), "no help line: {message}");
    message
  }

  #[test]
  fn an_enum_becomes_a_union_with_statuses_tags_and_decoding() {
    let out = ok(quote! {
      #[rpc(status = 409)]
      enum TodoError {
        #[rpc(status = 404)]
        NotFound { id: i64, note: Option<String> },
        #[rpc(tag = "Busy")]
        Locked,
        #[rpc(internal)]
        Broken { r#why: String },
      }
    });
    assert!(out.contains("status (404u16)"));
    assert!(out.contains("status (409u16)"));
    assert!(out.contains("\"Busy\""));
    assert!(out.contains("fn is_internal"));
    assert!(out.contains("\"why\""));
    assert!(out.contains("(\"note\""));
    assert!(
      out.contains("false)"),
      "optional fields are not required: {out}"
    );
    assert!(out.contains("fn from_wire"));
  }

  #[test]
  fn empty_and_internal_only_unions_expand() {
    let empty = ok(quote!(
      enum Nothing {}
    ));
    assert!(empty.contains("match * self { }"));
    let internal = ok(quote! {
      enum Fault {
        #[rpc(internal, status = 500)]
        Down,
      }
    });
    assert!(internal.contains("let _ = value"));
  }

  #[test]
  fn reserved_tags_are_refused() {
    assert!(
      err(quote!(
        enum E {
          InternalError,
        }
      ))
      .contains("reserved rpc failure tag")
    );
    assert!(
      err(quote!(
        enum E {
          #[rpc(tag = "Cancelled")]
          Mine,
        }
      ))
      .contains("`Cancelled`")
    );
  }

  #[test]
  fn duplicate_and_empty_tags_are_refused() {
    assert!(
      err(quote!(
        enum E {
          A,
          #[rpc(tag = "A")]
          B,
        }
      ))
      .contains("two variants")
    );
    assert!(
      err(quote!(
        enum E {
          #[rpc(tag = "")]
          A,
        }
      ))
      .contains("cannot be empty")
    );
  }

  #[test]
  fn only_concrete_enums_of_named_fields_are_unions() {
    assert!(
      err(quote!(
        struct E;
      ))
      .contains("goes on an enum")
    );
    assert!(
      err(quote!(
        enum E<T> {
          A(T),
        }
      ))
      .contains("cannot be generic")
    );
    assert!(
      err(quote!(
        enum E {
          A(i64),
        }
      ))
      .contains("unnamed fields")
    );
    assert!(
      err(quote!(
        enum E {
          A { _tag: String },
        }
      ))
      .contains("clashes")
    );
  }

  #[test]
  fn statuses_and_rpc_arguments_are_checked() {
    assert!(
      err(quote!(
        enum E {
          #[rpc(status = 200)]
          A,
        }
      ))
      .contains("not an error status")
    );
    assert!(
      err(quote!(
        enum E {
          #[rpc(status = 70000)]
          A,
        }
      ))
      .contains("not an error status")
    );
    assert!(
      err(quote!(
        enum E {
          #[rpc(nope)]
          A,
        }
      ))
      .contains("unknown")
    );
    assert!(
      err(quote!(
        #[rpc(tag = "X")]
        enum E {
          A,
        }
      ))
      .contains("default status")
    );
    assert!(
      expand(quote!(
        enum E {
          #[rpc(status = "x")]
          A,
        }
      ))
      .is_err()
    );
    assert!(
      expand(quote!(
        fn f() {}
      ))
      .is_err()
    );
  }
}

//! `#[derive(SchemaParser)]` — schema + JSON text parser codegen for `id_effect_parse`.

use proc_macro::TokenStream;
use quote::{format_ident, quote};
use syn::{Data, DeriveInput, Fields, Ident, LitStr, Type, parse_macro_input};

/// Expand `#[derive(SchemaParser)]` into `HasSchema`, `schema()`, and `parser()`.
pub fn derive_schema_parser(input: TokenStream) -> TokenStream {
  let input = parse_macro_input!(input as DeriveInput);
  let ident = &input.ident;
  let (impl_generics, ty_generics, where_clause) = input.generics.split_for_impl();

  let fields = match &input.data {
    Data::Struct(data) => match &data.fields {
      Fields::Named(named) => &named.named,
      _ => {
        return syn::Error::new_spanned(
          &input.ident,
          "SchemaParser supports structs with named fields only",
        )
        .to_compile_error()
        .into();
      }
    },
    _ => {
      return syn::Error::new_spanned(&input.ident, "SchemaParser supports structs only")
        .to_compile_error()
        .into();
    }
  };

  let mut decode_fields = Vec::new();
  let mut encode_pairs = Vec::new();
  let mut field_idents = Vec::new();

  for field in fields {
    let field_ident = field.ident.as_ref().expect("named field");
    field_idents.push(field_ident.clone());

    let wire_name = field
      .attrs
      .iter()
      .find_map(|attr| {
        if !attr.path().is_ident("schema") {
          return None;
        }
        attr.parse_args::<LitStr>().ok()
      })
      .map(|lit| lit.value())
      .unwrap_or_else(|| field_ident.to_string());

    let wire_lit = LitStr::new(&wire_name, field_ident.span());
    let decode_expr = schema_decode_expr(&field.ty, field_ident, &wire_lit);
    decode_fields.push(decode_expr);

    let encode_expr = schema_encode_expr(&field.ty, field_ident, &wire_lit);
    encode_pairs.push(encode_expr);
  }

  let struct_expr = if field_idents.is_empty() {
    quote! { #ident {} }
  } else {
    quote! { #ident { #( #field_idents ),* } }
  };

  quote! {
    impl #impl_generics #ident #ty_generics #where_clause {
      /// Canonical [`::id_effect::schema::Schema`] for this type (JSON wire).
      #[must_use]
      pub fn schema() -> ::id_effect::schema::Schema<
        Self,
        ::id_effect::schema::Unknown,
        ()
      > {
        ::id_effect::schema::Schema::make(
          |u: ::id_effect::schema::Unknown| {
            let obj = match u {
              ::id_effect::schema::Unknown::Object(map) => map,
              _ => return Err(::id_effect::schema::ParseError::new("", "expected object")),
            };
            #(
              #decode_fields
            )*
            Ok(#struct_expr)
          },
          |value: Self| {
            ::id_effect::schema::Unknown::Object(
              [
                #( #encode_pairs ),*
              ]
              .into_iter()
              .collect::<::std::collections::BTreeMap<_, _>>(),
            )
          },
          |u| {
            let obj = match u {
              ::id_effect::schema::Unknown::Object(map) => map,
              _ => return Err(::id_effect::schema::ParseError::new("", "expected object")),
            };
            #(
              #decode_fields
            )*
            Ok(#struct_expr)
          },
        )
      }

      /// Text parser that accepts JSON objects for this type.
      #[must_use]
      pub fn parser(
      ) -> ::id_effect_parse::Parser<
        String,
        Self,
        ::id_effect::schema::ParseError,
      > {
        ::id_effect_parse::SchemaBridge::parser_for_json(Self::schema())
      }
    }

    impl #impl_generics ::id_effect::schema::HasSchema for #ident #ty_generics #where_clause {
      type A = Self;
      type I = ::id_effect::schema::Unknown;
      type E = ();

      fn schema() -> ::id_effect::schema::Schema<Self, Self::I, Self::E> {
        Self::schema()
      }
    }
  }
  .into()
}

fn schema_decode_expr(
  ty: &Type,
  field_ident: &Ident,
  wire_lit: &LitStr,
) -> proc_macro2::TokenStream {
  if let Type::Path(path) = ty {
    if path.path.is_ident("String") {
      return quote! {
        let #field_ident = ::id_effect::schema::string::<()>()
          .decode_unknown(
            obj.get(#wire_lit)
              .ok_or_else(|| ::id_effect::schema::ParseError::new(#wire_lit, "missing field"))?,
          )
          .map_err(|e| e.prefix(#wire_lit))?;
      };
    }
    if path.path.is_ident("bool") {
      return quote! {
        let #field_ident = ::id_effect::schema::bool_::<()>()
          .decode_unknown(
            obj.get(#wire_lit)
              .ok_or_else(|| ::id_effect::schema::ParseError::new(#wire_lit, "missing field"))?,
          )
          .map_err(|e| e.prefix(#wire_lit))?;
      };
    }
    if path.path.is_ident("i64") {
      return quote! {
        let #field_ident = ::id_effect::schema::i64::<()>()
          .decode_unknown(
            obj.get(#wire_lit)
              .ok_or_else(|| ::id_effect::schema::ParseError::new(#wire_lit, "missing field"))?,
          )
          .map_err(|e| e.prefix(#wire_lit))?;
      };
    }
    if path.path.is_ident("i32") {
      return quote! {
        let #field_ident = ::id_effect::schema::transform(
          ::id_effect::schema::i64::<()>(),
          |n| ::std::convert::TryInto::<i32>::try_into(n)
            .map_err(|_| ::id_effect::schema::ParseError::new(#wire_lit, "i32 overflow")),
          |n| *n as i64,
        )
        .decode_unknown(
          obj.get(#wire_lit)
            .ok_or_else(|| ::id_effect::schema::ParseError::new(#wire_lit, "missing field"))?,
        )
        .map_err(|e| e.prefix(#wire_lit))?;
      };
    }
    if path.path.is_ident("f64") {
      return quote! {
        let #field_ident = ::id_effect::schema::f64::<()>()
          .decode_unknown(
            obj.get(#wire_lit)
              .ok_or_else(|| ::id_effect::schema::ParseError::new(#wire_lit, "missing field"))?,
          )
          .map_err(|e| e.prefix(#wire_lit))?;
      };
    }
    if path.path.is_ident("Option")
      && let Some(inner) = extract_generic(ty, 0)
    {
      let inner_ident = format_ident!("__opt_{}", field_ident);
      let inner_decode = schema_decode_from_unknown(inner, &inner_ident, wire_lit, true);
      return quote! {
        let #field_ident = match obj.get(#wire_lit) {
          None | Some(::id_effect::schema::Unknown::Null) => None,
          Some(raw) => Some({
            #inner_decode
            #inner_ident
          }),
        };
      };
    }
    if path.path.is_ident("Vec")
      && let Some(inner) = extract_generic(ty, 0)
    {
      let inner_schema = schema_for_type(inner);
      return quote! {
        let #field_ident = ::id_effect::schema::array(#inner_schema)
          .decode_unknown(
            obj.get(#wire_lit)
              .ok_or_else(|| ::id_effect::schema::ParseError::new(#wire_lit, "missing field"))?,
          )
          .map_err(|e| e.prefix(#wire_lit))?;
      };
    }
    let ty_tokens = quote! { #path };
    return quote! {
      let #field_ident = <#ty_tokens as ::id_effect::schema::HasSchema>::schema()
        .decode_unknown(
          obj.get(#wire_lit)
            .ok_or_else(|| ::id_effect::schema::ParseError::new(#wire_lit, "missing field"))?,
        )
        .map_err(|e| e.prefix(#wire_lit))?;
    };
  }
  syn::Error::new_spanned(ty, "unsupported SchemaParser field type").to_compile_error()
}

fn schema_decode_from_unknown(
  ty: &Type,
  out_ident: &Ident,
  wire_lit: &LitStr,
  from_var: bool,
) -> proc_macro2::TokenStream {
  let src = if from_var {
    quote! { raw }
  } else {
    quote! {
      obj.get(#wire_lit)
        .ok_or_else(|| ::id_effect::schema::ParseError::new(#wire_lit, "missing field"))?
    }
  };
  if let Type::Path(path) = ty {
    if path.path.is_ident("String") {
      return quote! {
        let #out_ident = ::id_effect::schema::string::<()>()
          .decode_unknown(#src)
          .map_err(|e| e.prefix(#wire_lit))?;
      };
    }
    if path.path.is_ident("bool") {
      return quote! {
        let #out_ident = ::id_effect::schema::bool_::<()>()
          .decode_unknown(#src)
          .map_err(|e| e.prefix(#wire_lit))?;
      };
    }
    if path.path.is_ident("i64") {
      return quote! {
        let #out_ident = ::id_effect::schema::i64::<()>()
          .decode_unknown(#src)
          .map_err(|e| e.prefix(#wire_lit))?;
      };
    }
    if path.path.is_ident("f64") {
      return quote! {
        let #out_ident = ::id_effect::schema::f64::<()>()
          .decode_unknown(#src)
          .map_err(|e| e.prefix(#wire_lit))?;
      };
    }
  }
  let ty_tokens = quote! { #ty };
  quote! {
    let #out_ident = <#ty_tokens as ::id_effect::schema::HasSchema>::schema()
      .decode_unknown(#src)
      .map_err(|e| e.prefix(#wire_lit))?;
  }
}

fn schema_encode_expr(
  ty: &Type,
  field_ident: &Ident,
  wire_lit: &LitStr,
) -> proc_macro2::TokenStream {
  if let Type::Path(path) = ty {
    if path.path.is_ident("String") {
      return quote! {
        (#wire_lit.to_string(), ::id_effect::schema::Unknown::String(value.#field_ident.clone()))
      };
    }
    if path.path.is_ident("bool") {
      return quote! {
        (#wire_lit.to_string(), ::id_effect::schema::Unknown::Bool(value.#field_ident))
      };
    }
    if path.path.is_ident("i64") {
      return quote! {
        (#wire_lit.to_string(), ::id_effect::schema::Unknown::I64(value.#field_ident))
      };
    }
    if path.path.is_ident("i32") {
      return quote! {
        (#wire_lit.to_string(), ::id_effect::schema::Unknown::I64(value.#field_ident as i64))
      };
    }
    if path.path.is_ident("f64") {
      return quote! {
        (#wire_lit.to_string(), ::id_effect::schema::Unknown::F64(value.#field_ident))
      };
    }
    if path.path.is_ident("Option")
      && let Some(inner) = extract_generic(ty, 0)
    {
      return schema_encode_option(inner, field_ident, wire_lit);
    }
    if path.path.is_ident("Vec")
      && let Some(inner) = extract_generic(ty, 0)
    {
      return schema_encode_vec(inner, field_ident, wire_lit);
    }
    let ty_tokens = quote! { #path };
    return quote! {
      (
        #wire_lit.to_string(),
        <#ty_tokens as ::id_effect::schema::HasSchema>::schema().encode(value.#field_ident.clone()),
      )
    };
  }
  syn::Error::new_spanned(ty, "unsupported SchemaParser field type").to_compile_error()
}

fn schema_encode_option(
  inner: &Type,
  field_ident: &Ident,
  wire_lit: &LitStr,
) -> proc_macro2::TokenStream {
  if let Type::Path(path) = inner {
    if path.path.is_ident("String") {
      return quote! {
        (
          #wire_lit.to_string(),
          match &value.#field_ident {
            None => ::id_effect::schema::Unknown::Null,
            Some(v) => ::id_effect::schema::Unknown::String(v.clone()),
          },
        )
      };
    }
    if path.path.is_ident("i64") {
      return quote! {
        (
          #wire_lit.to_string(),
          match value.#field_ident {
            None => ::id_effect::schema::Unknown::Null,
            Some(v) => ::id_effect::schema::Unknown::I64(v),
          },
        )
      };
    }
  }
  let inner_tokens = quote! { #inner };
  quote! {
    (
      #wire_lit.to_string(),
      match &value.#field_ident {
        None => ::id_effect::schema::Unknown::Null,
        Some(v) => <#inner_tokens as ::id_effect::schema::HasSchema>::schema().encode(v.clone()),
      },
    )
  }
}

fn schema_for_type(ty: &Type) -> proc_macro2::TokenStream {
  if let Type::Path(path) = ty {
    if path.path.is_ident("String") {
      return quote! { ::id_effect::schema::string::<()>() };
    }
    if path.path.is_ident("bool") {
      return quote! { ::id_effect::schema::bool_::<()>() };
    }
    if path.path.is_ident("i64") {
      return quote! { ::id_effect::schema::i64::<()>() };
    }
    if path.path.is_ident("f64") {
      return quote! { ::id_effect::schema::f64::<()>() };
    }
  }
  let ty_tokens = quote! { #ty };
  quote! { <#ty_tokens as ::id_effect::schema::HasSchema>::schema() }
}

fn extract_generic(ty: &Type, index: usize) -> Option<&Type> {
  let Type::Path(type_path) = ty else {
    return None;
  };
  let seg = type_path.path.segments.last()?;
  let syn::PathArguments::AngleBracketed(args) = &seg.arguments else {
    return None;
  };
  args.args.iter().nth(index).and_then(|arg| {
    if let syn::GenericArgument::Type(t) = arg {
      Some(t)
    } else {
      None
    }
  })
}

fn schema_encode_vec(
  inner: &Type,
  field_ident: &Ident,
  wire_lit: &LitStr,
) -> proc_macro2::TokenStream {
  if let Type::Path(path) = inner {
    if path.path.is_ident("String") {
      return quote! {
        (
          #wire_lit.to_string(),
          ::id_effect::schema::Unknown::Array(
            value.#field_ident.iter().cloned().map(::id_effect::schema::Unknown::String).collect(),
          ),
        )
      };
    }
    if path.path.is_ident("i64") {
      return quote! {
        (
          #wire_lit.to_string(),
          ::id_effect::schema::Unknown::Array(
            value.#field_ident.iter().copied().map(::id_effect::schema::Unknown::I64).collect(),
          ),
        )
      };
    }
    if path.path.is_ident("bool") {
      return quote! {
        (
          #wire_lit.to_string(),
          ::id_effect::schema::Unknown::Array(
            value.#field_ident.iter().copied().map(::id_effect::schema::Unknown::Bool).collect(),
          ),
        )
      };
    }
    if path.path.is_ident("f64") {
      return quote! {
        (
          #wire_lit.to_string(),
          ::id_effect::schema::Unknown::Array(
            value.#field_ident.iter().copied().map(::id_effect::schema::Unknown::F64).collect(),
          ),
        )
      };
    }
  }
  let inner_tokens = quote! { #inner };
  quote! {
    (
      #wire_lit.to_string(),
      ::id_effect::schema::Unknown::Array(
        value
          .#field_ident
          .iter()
          .cloned()
          .map(|item| {
            let wire = <#inner_tokens as ::id_effect::schema::HasSchema<I = ::id_effect::schema::Unknown>>::schema()
              .encode(item);
            wire
          })
          .collect(),
      ),
    )
  }
}

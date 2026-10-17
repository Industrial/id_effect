//! `#[derive(Schema)]` — `schema()` +
//! `HasSchema` codegen over the JSON-shaped `Unknown` wire, with a `SchemaAst` describing the
//! struct so JSON Schema export sees its fields.

use proc_macro::TokenStream;
use quote::{format_ident, quote};
use syn::{Data, DeriveInput, Fields, Ident, LitStr, Type, parse_macro_input};

/// Expand `#[derive(Schema)]` into an inherent `schema()` and a `HasSchema` impl.
pub fn derive_schema(input: TokenStream) -> TokenStream {
  let input = parse_macro_input!(input as DeriveInput);
  let ident = &input.ident;
  let (impl_generics, ty_generics, where_clause) = input.generics.split_for_impl();

  let fields = match &input.data {
    Data::Struct(data) => match &data.fields {
      Fields::Named(named) => &named.named,
      _ => {
        return syn::Error::new_spanned(
          &input.ident,
          "derive(Schema) supports structs with named fields only",
        )
        .to_compile_error()
        .into();
      }
    },
    _ => {
      return syn::Error::new_spanned(&input.ident, "derive(Schema) supports structs only")
        .to_compile_error()
        .into();
    }
  };

  let mut decode_fields = Vec::new();
  let mut decode_all_fields = Vec::new();
  let mut encode_pairs = Vec::new();
  let mut field_idents = Vec::new();
  let mut ast_fields = Vec::new();

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
    decode_all_fields.push(schema_decode_all_expr(&field.ty, field_ident, &wire_lit));

    let encode_expr = schema_encode_expr(&field.ty, field_ident, &wire_lit);
    encode_pairs.push(encode_expr);

    ast_fields.push(schema_field_ast(&field.ty, &wire_lit));
  }

  let struct_expr = if field_idents.is_empty() {
    quote! { #ident {} }
  } else {
    quote! { #ident { #( #field_idents ),* } }
  };

  // `decode_unknown_all`: decode every field, collect each one's issues, build only when all
  // fields decoded (a field local is `None` exactly when it recorded an issue).
  let decode_all_body = if field_idents.is_empty() {
    quote! { Ok(#struct_expr) }
  } else {
    quote! {
      let mut __issues: ::std::vec::Vec<::id_effect::schema::ParseError> = ::std::vec::Vec::new();
      #(
        #decode_all_fields
      )*
      #(
        let Some(#field_idents) = #field_idents else {
          return Err(::id_effect::schema::ParseErrors::new(__issues));
        };
      )*
      Ok(#struct_expr)
    }
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
        .with_decode_unknown_all(|u: &::id_effect::schema::Unknown| {
          let obj = match u {
            ::id_effect::schema::Unknown::Object(map) => map,
            _ => {
              return Err(::id_effect::schema::ParseErrors::one(
                ::id_effect::schema::ParseError::new("", "expected object"),
              ));
            }
          };
          #decode_all_body
        })
        .with_ast(::id_effect::schema::SchemaAst::new(
          ::id_effect::schema::SchemaKind::Struct(::std::vec![ #( #ast_fields ),* ]),
        ))
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
      let i32_schema = i32_schema_tokens();
      return quote! {
        let #field_ident = #i32_schema
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
    if last_segment_is(path, "Option")
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
    if last_segment_is(path, "Vec")
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
  syn::Error::new_spanned(ty, "unsupported derive(Schema) field type").to_compile_error()
}

/// One field of the accumulating decoder: binds `field_ident` to `Some(value)`, or to `None` after
/// pushing the field's issues (prefixed with its wire name) or a missing-field issue onto
/// `__issues`. `Option<T>` fields accept a missing key or `null` as `None`.
fn schema_decode_all_expr(
  ty: &Type,
  field_ident: &Ident,
  wire_lit: &LitStr,
) -> proc_macro2::TokenStream {
  let Type::Path(path) = ty else {
    // Unsupported; `schema_decode_expr` already reports the error.
    return quote! { let #field_ident = ::std::option::Option::<#ty>::None; };
  };
  let decode = |schema: proc_macro2::TokenStream, ok: proc_macro2::TokenStream| {
    quote! {
      match #schema.decode_unknown_all(raw) {
        Ok(v) => Some(#ok),
        Err(e) => {
          __issues.extend(e.prefix(#wire_lit).issues);
          None
        }
      }
    }
  };
  if last_segment_is(path, "Option")
    && let Some(inner) = extract_generic(ty, 0)
  {
    let inner_decode = decode(schema_for_type(inner), quote! { Some(v) });
    return quote! {
      let #field_ident = match obj.get(#wire_lit) {
        None | Some(::id_effect::schema::Unknown::Null) => Some(None),
        Some(raw) => #inner_decode,
      };
    };
  }
  let schema = if last_segment_is(path, "Vec")
    && let Some(inner) = extract_generic(ty, 0)
  {
    let inner_schema = schema_for_type(inner);
    quote! { ::id_effect::schema::array(#inner_schema) }
  } else {
    schema_for_type(ty)
  };
  let field_decode = decode(schema, quote! { v });
  quote! {
    let #field_ident = match obj.get(#wire_lit) {
      Some(raw) => #field_decode,
      None => {
        __issues.push(::id_effect::schema::ParseError::new(#wire_lit, "missing field"));
        None
      }
    };
  }
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
    if path.path.is_ident("i32") {
      let i32_schema = i32_schema_tokens();
      return quote! {
        let #out_ident = #i32_schema
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
    if last_segment_is(path, "Option")
      && let Some(inner) = extract_generic(ty, 0)
    {
      return schema_encode_option(inner, field_ident, wire_lit);
    }
    if last_segment_is(path, "Vec")
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
  syn::Error::new_spanned(ty, "unsupported derive(Schema) field type").to_compile_error()
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
    if path.path.is_ident("i32") {
      return quote! {
        (
          #wire_lit.to_string(),
          match value.#field_ident {
            None => ::id_effect::schema::Unknown::Null,
            Some(v) => ::id_effect::schema::Unknown::I64(::std::primitive::i64::from(v)),
          },
        )
      };
    }
    if path.path.is_ident("bool") {
      return quote! {
        (
          #wire_lit.to_string(),
          match value.#field_ident {
            None => ::id_effect::schema::Unknown::Null,
            Some(v) => ::id_effect::schema::Unknown::Bool(v),
          },
        )
      };
    }
    if path.path.is_ident("f64") {
      return quote! {
        (
          #wire_lit.to_string(),
          match value.#field_ident {
            None => ::id_effect::schema::Unknown::Null,
            Some(v) => ::id_effect::schema::Unknown::F64(v),
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
    if path.path.is_ident("i32") {
      return i32_schema_tokens();
    }
  }
  let ty_tokens = quote! { #ty };
  quote! { <#ty_tokens as ::id_effect::schema::HasSchema>::schema() }
}

/// `SchemaAst` `Field` for one struct field: `Option<T>` keys may be missing or `null` (not
/// required, nullable), everything else is required.
fn schema_field_ast(ty: &Type, wire_lit: &LitStr) -> proc_macro2::TokenStream {
  if let Type::Path(path) = ty
    && last_segment_is(path, "Option")
    && let Some(inner) = extract_generic(ty, 0)
  {
    let inner_ast = schema_ast_for_type(inner);
    return quote! {
      ::id_effect::schema::Field::optional(
        #wire_lit,
        ::id_effect::schema::SchemaAst::new(::id_effect::schema::SchemaKind::Nullable(
          ::std::boxed::Box::new(#inner_ast),
        )),
      )
    };
  }
  let ast = schema_ast_for_type(ty);
  quote! { ::id_effect::schema::Field::required(#wire_lit, #ast) }
}

/// `SchemaAst` expression for a field type, mirroring `schema_decode_expr`. Other `HasSchema`
/// types become named lazy references (`ast_ref::<T>()`), so recursive types terminate and export
/// via `$defs` / `$ref`.
fn schema_ast_for_type(ty: &Type) -> proc_macro2::TokenStream {
  let Type::Path(path) = ty else {
    // Unsupported; decode/encode codegen already reports the error.
    return quote! { ::id_effect::schema::SchemaAst::any() };
  };
  if path.path.is_ident("i32") {
    return quote! {
      {
        let mut ast = ::id_effect::schema::i64::<()>().ast().clone();
        ast.constraints.minimum = ::std::option::Option::Some(
          ::std::primitive::f64::from(::std::primitive::i32::MIN),
        );
        ast.constraints.maximum = ::std::option::Option::Some(
          ::std::primitive::f64::from(::std::primitive::i32::MAX),
        );
        ast
      }
    };
  }
  if ["String", "bool", "i64", "f64"]
    .iter()
    .any(|name| path.path.is_ident(name))
  {
    let schema = schema_for_type(ty);
    return quote! { #schema.ast().clone() };
  }
  if last_segment_is(path, "Vec")
    && let Some(inner) = extract_generic(ty, 0)
  {
    let inner_ast = schema_ast_for_type(inner);
    return quote! {
      ::id_effect::schema::SchemaAst::new(::id_effect::schema::SchemaKind::Array(
        ::std::boxed::Box::new(#inner_ast),
      ))
    };
  }
  quote! { ::id_effect::schema::ast_ref::<#ty>() }
}

/// `i32` over the `i64` wire: decode range-checks, encode widens losslessly.
fn i32_schema_tokens() -> proc_macro2::TokenStream {
  quote! {
    ::id_effect::schema::transform(
      ::id_effect::schema::i64::<()>(),
      |n: ::std::primitive::i64| ::std::convert::TryInto::<::std::primitive::i32>::try_into(n)
        .map_err(|_| ::id_effect::schema::ParseError::new("", "i32 overflow")),
      |n: ::std::primitive::i32| ::std::primitive::i64::from(n),
    )
  }
}

/// `Option<T>` / `Vec<T>` carry generic args, so `Path::is_ident` never matches them.
fn last_segment_is(path: &syn::TypePath, name: &str) -> bool {
  path
    .path
    .segments
    .last()
    .is_some_and(|seg| seg.ident == name)
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
    if path.path.is_ident("i32") {
      return quote! {
        (
          #wire_lit.to_string(),
          ::id_effect::schema::Unknown::Array(
            value
              .#field_ident
              .iter()
              .map(|n| ::id_effect::schema::Unknown::I64(::std::primitive::i64::from(*n)))
              .collect(),
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
            // Associated-item constraints are not allowed in a qualified path
            // (`<T as HasSchema<I = Unknown>>` is E0229); `Unknown::Array` pins `I`.
            <#inner_tokens as ::id_effect::schema::HasSchema>::schema().encode(item)
          })
          .collect(),
      ),
    )
  }
}

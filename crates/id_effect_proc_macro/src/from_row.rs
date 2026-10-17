//! `#[derive(FromRow)]` — generate an `id_effect_sql::FromRow` impl that decodes a struct with
//! named fields from an `SqlRow`.
//!
//! ```text
//! #[from_row(schema, crate = path)]        // container; both optional
//! struct Todo {
//!   id: i64,                                // column "id"
//!   #[schema("title_text")] title: String,  // column "title_text" (the derive(Schema) rename)
//!   #[from_row(rename = "done_at")] done: Option<Timestamp>, // NULL → None; wins over #[schema]
//!   #[from_row(default)] tags: String,      // missing column → Default::default()
//!   #[from_row(flatten)] audit: Audit,      // Audit: FromRow, decoded from the same row
//! }
//! ```
//!
//! Plain mode is one `row.get::<FieldTy>(column)` per field (spanned at the field type, so a type
//! without `FromSqlValue` is reported there). `schema` mode decodes the whole row with the type's
//! `HasSchema` schema instead (`id_effect_sql::from_row::from_row_with_schema`), so it takes no
//! field attributes: columns are the schema's wire names.

use proc_macro::TokenStream;
use proc_macro2::{Span, TokenStream as TokenStream2};
use quote::{quote, quote_spanned};
use syn::ext::IdentExt;
use syn::spanned::Spanned;
use syn::{Data, DeriveInput, Field, Fields, LitStr, Path, parse_macro_input};

pub(crate) fn derive(input: TokenStream) -> TokenStream {
  let input = parse_macro_input!(input as DeriveInput);
  match expand(input) {
    Ok(ts) => ts.into(),
    Err(e) => e.to_compile_error().into(),
  }
}

#[derive(Default)]
struct ContainerAttrs {
  schema: bool,
  krate: Option<Path>,
}

fn parse_container(input: &DeriveInput) -> syn::Result<ContainerAttrs> {
  let mut attrs = ContainerAttrs::default();
  for attr in input.attrs.iter().filter(|a| a.path().is_ident("from_row")) {
    attr.parse_nested_meta(|meta| {
      if meta.path.is_ident("schema") {
        attrs.schema = true;
      } else if meta.path.is_ident("crate") {
        attrs.krate = Some(meta.value()?.parse()?);
      } else {
        return Err(meta.error("expected `schema` or `crate = ..` on the struct"));
      }
      Ok(())
    })?;
  }
  Ok(attrs)
}

#[derive(Default)]
struct FieldAttrs {
  rename: Option<LitStr>,
  default: bool,
  flatten: bool,
  /// The first `#[from_row(..)]` attribute, for errors in `schema` mode.
  span: Option<Span>,
}

fn parse_field(field: &Field) -> syn::Result<FieldAttrs> {
  let mut attrs = FieldAttrs::default();
  for attr in field.attrs.iter().filter(|a| a.path().is_ident("from_row")) {
    attrs.span.get_or_insert_with(|| attr.span());
    attr.parse_nested_meta(|meta| {
      if meta.path.is_ident("rename") {
        attrs.rename = Some(meta.value()?.parse()?);
      } else if meta.path.is_ident("default") {
        attrs.default = true;
      } else if meta.path.is_ident("flatten") {
        attrs.flatten = true;
      } else {
        return Err(meta.error("expected `rename = \"..\"`, `default` or `flatten`"));
      }
      Ok(())
    })?;
  }
  if attrs.flatten && (attrs.rename.is_some() || attrs.default) {
    return Err(syn::Error::new_spanned(
      field,
      "`flatten` decodes the field from the whole row; it cannot be combined with `rename` or `default`",
    ));
  }
  Ok(attrs)
}

/// The column of a field: `#[from_row(rename)]`, else `#[schema("..")]`, else the field name.
fn column(field: &Field, attrs: &FieldAttrs) -> LitStr {
  if let Some(rename) = &attrs.rename {
    return rename.clone();
  }
  let ident = field.ident.as_ref().expect("named field");
  field
    .attrs
    .iter()
    .filter(|a| a.path().is_ident("schema"))
    .find_map(|a| a.parse_args::<LitStr>().ok())
    .unwrap_or_else(|| LitStr::new(&ident.unraw().to_string(), ident.span()))
}

pub(crate) fn expand(input: DeriveInput) -> syn::Result<TokenStream2> {
  let name = &input.ident;
  let Data::Struct(data) = &input.data else {
    return Err(syn::Error::new(
      Span::call_site(),
      "#[derive(FromRow)] supports structs with named fields only",
    ));
  };
  let fields = match &data.fields {
    Fields::Named(named) => &named.named,
    Fields::Unit => &syn::punctuated::Punctuated::new(),
    Fields::Unnamed(_) => {
      return Err(syn::Error::new_spanned(
        &data.fields,
        "#[derive(FromRow)] supports structs with named fields only (decode tuples with the \
         tuple `FromRow` impls)",
      ));
    }
  };
  let attrs = parse_container(&input)?;
  let krate = attrs
    .krate
    .unwrap_or_else(|| syn::parse_quote!(::id_effect_sql));
  let (impl_generics, ty_generics, where_clause) = input.generics.split_for_impl();

  let body = if attrs.schema {
    for field in fields {
      if let Some(span) = parse_field(field)?.span {
        return Err(syn::Error::new(
          span,
          "#[from_row(schema)] decodes with the type's Schema: rename columns with \
           #[schema(\"..\")], not #[from_row(..)]",
        ));
      }
    }
    quote! { #krate::from_row::from_row_with_schema::<Self>(row) }
  } else {
    let mut inits = Vec::new();
    for field in fields {
      let attrs = parse_field(field)?;
      let ident = field.ident.as_ref().expect("named field");
      let ty = &field.ty;
      let value = if attrs.flatten {
        quote_spanned! { ty.span()=> <#ty as #krate::FromRow>::from_row(row)? }
      } else {
        let col = column(field, &attrs);
        if attrs.default {
          quote_spanned! { ty.span()=> #krate::from_row::get_or_default::<#ty>(row, #col)? }
        } else {
          quote_spanned! { ty.span()=> row.get::<#ty>(#col)? }
        }
      };
      inits.push(quote! { #ident: #value });
    }
    quote! { ::core::result::Result::Ok(Self { #(#inits),* }) }
  };

  Ok(quote! {
    #[automatically_derived]
    impl #impl_generics #krate::FromRow for #name #ty_generics #where_clause {
      fn from_row(
        row: &#krate::SqlRow,
      ) -> ::core::result::Result<Self, #krate::SqlError> {
        #body
      }
    }
  })
}

#[cfg(test)]
mod tests {
  use super::*;

  fn expand_str(src: &str) -> Result<String, String> {
    let input: DeriveInput = syn::parse_str(src).expect("valid item");
    expand(input)
      .map(|ts| ts.to_string())
      .map_err(|e| e.to_string())
  }

  #[test]
  fn plain_mode_gets_each_field_by_column() {
    let out = expand_str(
      "struct Todo {
         id: i64,
         #[schema(\"title_text\")] title: String,
         #[schema(\"ignored\")] #[from_row(rename = \"done_at\")] done: Option<bool>,
         #[from_row(default)] tags: String,
         #[from_row(flatten)] audit: Audit,
         r#type: String,
       }",
    )
    .unwrap();
    assert!(
      out.contains("impl :: id_effect_sql :: FromRow for Todo"),
      "{out}"
    );
    assert!(
      out.contains("id : row . get :: < i64 > (\"id\") ?"),
      "{out}"
    );
    assert!(
      out.contains("title : row . get :: < String > (\"title_text\") ?"),
      "{out}"
    );
    assert!(
      out.contains("done : row . get :: < Option < bool > > (\"done_at\") ?"),
      "{out}"
    );
    assert!(
      out.contains(
        "tags : :: id_effect_sql :: from_row :: get_or_default :: < String > (row , \"tags\") ?"
      ),
      "{out}"
    );
    assert!(
      out.contains("audit : < Audit as :: id_effect_sql :: FromRow > :: from_row (row) ?"),
      "{out}"
    );
    assert!(
      out.contains("r#type : row . get :: < String > (\"type\") ?"),
      "{out}"
    );
  }

  #[test]
  fn schema_mode_and_crate_override() {
    let out = expand_str(
      "#[from_row(schema, crate = my::sql)]
       struct User<T> where T: Clone { #[schema(\"user_name\")] name: String, t: T }",
    )
    .unwrap();
    assert!(
      out.contains("impl < T > my :: sql :: FromRow for User < T > where T : Clone"),
      "{out}"
    );
    assert!(
      out.contains("my :: sql :: from_row :: from_row_with_schema :: < Self > (row)"),
      "{out}"
    );
    assert!(out.contains("row : & my :: sql :: SqlRow"), "{out}");
  }

  #[test]
  fn unit_and_empty_structs_decode_any_row() {
    let out = expand_str("struct Unit;").unwrap();
    assert!(out.contains("Ok (Self { })"), "{out}");
    let out = expand_str("struct Empty {}").unwrap();
    assert!(out.contains("Ok (Self { })"), "{out}");
  }

  #[test]
  fn rejects_enums_and_tuple_structs() {
    let err = expand_str("enum E { A }").unwrap_err();
    assert!(err.contains("named fields only"), "{err}");
    let err = expand_str("struct T(i64);").unwrap_err();
    assert!(err.contains("tuple `FromRow` impls"), "{err}");
  }

  #[test]
  fn rejects_unknown_and_conflicting_keys() {
    let err = expand_str("#[from_row(table = \"x\")] struct S { a: i64 }").unwrap_err();
    assert!(err.contains("expected `schema`"), "{err}");
    let err = expand_str("struct S { #[from_row(skip)] a: i64 }").unwrap_err();
    assert!(err.contains("expected `rename"), "{err}");
    let err = expand_str("struct S { #[from_row(flatten, default)] a: A }").unwrap_err();
    assert!(err.contains("cannot be combined"), "{err}");
    let err = expand_str("struct S { #[from_row(rename = 1)] a: i64 }").unwrap_err();
    assert!(err.contains("expected string literal"), "{err}");
  }

  #[test]
  fn schema_mode_rejects_field_attributes() {
    let err = expand_str("#[from_row(schema)] struct S { #[from_row(rename = \"b\")] a: i64 }")
      .unwrap_err();
    assert!(err.contains("rename columns with #[schema"), "{err}");
    let err = expand_str("#[from_row(schema)] struct S { #[from_row(bogus)] a: i64 }").unwrap_err();
    assert!(err.contains("expected `rename"), "{err}");
  }
}

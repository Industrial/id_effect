//! `#[derive(Entity)]` — generate an `id_effect_sql::repository::Entity` impl mapping a
//! `#[derive(FromRow)]` struct to a table.
//!
//! ```text
//! #[entity(table = "accounts",            // default: the type name in snake case
//!          tenant = "org_id",             // scope every statement to the repository's tenant
//!          soft_delete = "deleted_at",    // hide rows whose column is not NULL
//!          scope = active_only,           // fn() -> impl Into<Condition>; repeatable
//!          crate = path)]                 // all optional
//! struct Account {
//!   #[entity(id, generated)] id: i64,     // primary key; `generated`: assigned by the database
//!   #[from_row(rename = "title")] name: String, // columns are the FromRow columns
//!   #[entity(version)] version: i64,      // optimistic locking
//! }
//! ```
//!
//! The columns follow `FromRow`'s rules (`#[from_row(rename)]`, else `#[schema("..")]`, else
//! the field name) so the two derives always agree; `#[from_row(flatten)]` fields have no single
//! column and are rejected. Each value is `<FieldTy as ToSqlValue>::to_sql_value(&self.field)`,
//! spanned at the field type, so a type without `ToSqlValue` is reported there.

use proc_macro::TokenStream;
use proc_macro2::{Span, TokenStream as TokenStream2};
use quote::{quote, quote_spanned};
use syn::ext::IdentExt;
use syn::spanned::Spanned;
use syn::{Data, DeriveInput, Field, Fields, Ident, LitStr, Path, parse_macro_input};

pub(crate) fn derive(input: TokenStream) -> TokenStream {
  let input = parse_macro_input!(input as DeriveInput);
  match expand(input) {
    Ok(ts) => ts.into(),
    Err(e) => e.to_compile_error().into(),
  }
}

#[derive(Default)]
struct ContainerAttrs {
  table: Option<LitStr>,
  tenant: Option<LitStr>,
  soft_delete: Option<LitStr>,
  scopes: Vec<Path>,
  krate: Option<Path>,
}

fn parse_container(input: &DeriveInput) -> syn::Result<ContainerAttrs> {
  let mut attrs = ContainerAttrs::default();
  for attr in input.attrs.iter().filter(|a| a.path().is_ident("entity")) {
    attr.parse_nested_meta(|meta| {
      if meta.path.is_ident("table") {
        attrs.table = Some(meta.value()?.parse()?);
      } else if meta.path.is_ident("tenant") {
        attrs.tenant = Some(meta.value()?.parse()?);
      } else if meta.path.is_ident("soft_delete") {
        attrs.soft_delete = Some(meta.value()?.parse()?);
      } else if meta.path.is_ident("scope") {
        attrs.scopes.push(meta.value()?.parse()?);
      } else if meta.path.is_ident("crate") {
        attrs.krate = Some(meta.value()?.parse()?);
      } else {
        return Err(meta.error(
          "expected `table = \"..\"`, `tenant = \"..\"`, `soft_delete = \"..\"`, `scope = path` \
           or `crate = path` on the struct",
        ));
      }
      Ok(())
    })?;
  }
  Ok(attrs)
}

#[derive(Default)]
struct FieldAttrs {
  id: bool,
  generated: bool,
  version: bool,
  /// The first `#[entity(..)]` attribute, for duplicate errors.
  span: Option<Span>,
}

fn parse_field(field: &Field) -> syn::Result<FieldAttrs> {
  let mut attrs = FieldAttrs::default();
  for attr in field.attrs.iter().filter(|a| a.path().is_ident("entity")) {
    attrs.span.get_or_insert_with(|| attr.span());
    attr.parse_nested_meta(|meta| {
      if meta.path.is_ident("id") {
        attrs.id = true;
      } else if meta.path.is_ident("generated") {
        attrs.generated = true;
      } else if meta.path.is_ident("version") {
        attrs.version = true;
      } else {
        return Err(meta.error("expected `id`, `generated` or `version`"));
      }
      Ok(())
    })?;
  }
  if attrs.generated && !attrs.id {
    return Err(syn::Error::new_spanned(
      field,
      "`generated` marks a database-assigned primary key: use it as `#[entity(id, generated)]`",
    ));
  }
  if attrs.id && attrs.version {
    return Err(syn::Error::new_spanned(
      field,
      "a field cannot be both the `id` and the `version` column",
    ));
  }
  Ok(attrs)
}

/// The field's `FromRow` column (`#[from_row(rename)]`, else `#[schema("..")]`, else its name)
/// and whether it is `#[from_row(flatten)]`. Other `from_row` keys are `FromRow`'s to check.
fn from_row_column(field: &Field) -> syn::Result<(LitStr, bool)> {
  let (mut rename, mut flatten) = (None::<LitStr>, false);
  for attr in field.attrs.iter().filter(|a| a.path().is_ident("from_row")) {
    attr.parse_nested_meta(|meta| {
      if meta.path.is_ident("rename") {
        rename = Some(meta.value()?.parse()?);
      } else if meta.path.is_ident("flatten") {
        flatten = true;
      }
      Ok(())
    })?;
  }
  let ident = field.ident.as_ref().expect("named field");
  let column = rename
    .or_else(|| {
      field
        .attrs
        .iter()
        .filter(|a| a.path().is_ident("schema"))
        .find_map(|a| a.parse_args::<LitStr>().ok())
    })
    .unwrap_or_else(|| LitStr::new(&ident.unraw().to_string(), ident.span()));
  Ok((column, flatten))
}

/// `LedgerEntry` → `ledger_entry`.
fn snake_case(ident: &Ident) -> String {
  let mut out = String::new();
  let mut prev_lower = false;
  for c in ident.unraw().to_string().chars() {
    if c.is_uppercase() {
      if prev_lower {
        out.push('_');
      }
      out.extend(c.to_lowercase());
      prev_lower = false;
    } else {
      out.push(c);
      prev_lower = c.is_lowercase() || c.is_ascii_digit();
    }
  }
  out
}

fn option(lit: Option<&LitStr>) -> TokenStream2 {
  match lit {
    Some(lit) => quote! { ::core::option::Option::Some(#lit) },
    None => quote! { ::core::option::Option::None },
  }
}

pub(crate) fn expand(input: DeriveInput) -> syn::Result<TokenStream2> {
  let name = &input.ident;
  let Data::Struct(data) = &input.data else {
    return Err(syn::Error::new(
      Span::call_site(),
      "#[derive(Entity)] supports structs with named fields only",
    ));
  };
  let Fields::Named(fields) = &data.fields else {
    return Err(syn::Error::new_spanned(
      &data.fields,
      "#[derive(Entity)] supports structs with named fields only",
    ));
  };
  let attrs = parse_container(&input)?;
  let krate = attrs
    .krate
    .unwrap_or_else(|| syn::parse_quote!(::id_effect_sql));
  let (impl_generics, ty_generics, where_clause) = input.generics.split_for_impl();

  let (mut id, mut version) = (None, None);
  let (mut columns, mut values) = (Vec::new(), Vec::new());
  for field in &fields.named {
    let fa = parse_field(field)?;
    let (column, flatten) = from_row_column(field)?;
    if flatten {
      return Err(syn::Error::new_spanned(
        field,
        "#[derive(Entity)] maps each field to one column: `#[from_row(flatten)]` fields are not \
         supported",
      ));
    }
    let ident = field.ident.as_ref().expect("named field");
    let ty = &field.ty;
    let span = fa.span.unwrap_or_else(|| field.span());
    if fa.id {
      if id.is_some() {
        return Err(syn::Error::new(
          span,
          "#[derive(Entity)] takes one `#[entity(id)]` field",
        ));
      }
      id = Some((ident, ty, column.clone(), fa.generated));
    }
    if fa.version {
      if version.is_some() {
        return Err(syn::Error::new(
          span,
          "#[derive(Entity)] takes one `#[entity(version)]` field",
        ));
      }
      version = Some((ident, ty, column.clone()));
    }
    values.push(quote_spanned! { ty.span()=>
      (#column, <#ty as #krate::ToSqlValue>::to_sql_value(&self.#ident))
    });
    columns.push(column);
  }
  let Some((id_ident, id_ty, id_column, generated)) = id else {
    return Err(syn::Error::new(
      name.span(),
      "#[derive(Entity)] needs an `#[entity(id)]` field (the primary key)",
    ));
  };

  let name_str = name.unraw().to_string();
  let table = attrs
    .table
    .unwrap_or_else(|| LitStr::new(&snake_case(name), name.span()));
  let version_column = option(version.as_ref().map(|(_, _, c)| c));
  let tenant = option(attrs.tenant.as_ref());
  let soft_delete = option(attrs.soft_delete.as_ref());
  let bump = version.map(|(ident, ty, _)| {
    let add = quote_spanned! { ty.span()=> self.#ident += 1; };
    quote! {
      fn bump_version(&mut self) {
        #add
      }
    }
  });
  let scope = (!attrs.scopes.is_empty()).then(|| {
    let scopes = &attrs.scopes;
    quote! {
      fn scope() -> #krate::query_builder::sea_query::Condition {
        #krate::query_builder::sea_query::Condition::all() #(.add(#scopes()))*
      }
    }
  });

  Ok(quote! {
    #[automatically_derived]
    impl #impl_generics #krate::repository::Entity for #name #ty_generics #where_clause {
      type Id = #id_ty;

      const NAME: &'static str = #name_str;
      const TABLE: &'static str = #table;
      const COLUMNS: &'static [&'static str] = &[#(#columns),*];
      const ID: &'static str = #id_column;
      const ID_GENERATED: bool = #generated;
      const VERSION: ::core::option::Option<&'static str> = #version_column;
      const SOFT_DELETE: ::core::option::Option<&'static str> = #soft_delete;
      const TENANT: ::core::option::Option<&'static str> = #tenant;

      fn id(&self) -> Self::Id {
        ::core::clone::Clone::clone(&self.#id_ident)
      }

      fn values(&self) -> ::std::vec::Vec<(&'static str, #krate::SqlValue)> {
        ::std::vec![#(#values),*]
      }

      #bump
      #scope
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
  fn maps_table_columns_id_version_and_filters() {
    let out = expand_str(
      "#[entity(table = \"accounts\", tenant = \"org_id\", soft_delete = \"deleted_at\")]
       #[entity(scope = active, scope = crate::visible)]
       struct Account {
         #[entity(id, generated)] id: i64,
         #[from_row(rename = \"title\", default)] name: String,
         #[schema(\"nick_name\")] nick: Option<String>,
         #[entity(version)] version: i32,
         r#type: String,
       }",
    )
    .unwrap();
    for part in [
      "impl :: id_effect_sql :: repository :: Entity for Account",
      "type Id = i64 ;",
      "const NAME : & 'static str = \"Account\" ;",
      "const TABLE : & 'static str = \"accounts\" ;",
      "const COLUMNS : & 'static [& 'static str] = & [\"id\" , \"title\" , \"nick_name\" , \"version\" , \"type\"] ;",
      "const ID : & 'static str = \"id\" ;",
      "const ID_GENERATED : bool = true ;",
      "const VERSION : :: core :: option :: Option < & 'static str > = :: core :: option :: Option :: Some (\"version\") ;",
      "const SOFT_DELETE : :: core :: option :: Option < & 'static str > = :: core :: option :: Option :: Some (\"deleted_at\") ;",
      "const TENANT : :: core :: option :: Option < & 'static str > = :: core :: option :: Option :: Some (\"org_id\") ;",
      "(\"title\" , < String as :: id_effect_sql :: ToSqlValue > :: to_sql_value (& self . name))",
      "(\"type\" , < String as :: id_effect_sql :: ToSqlValue > :: to_sql_value (& self . r#type))",
      "fn bump_version (& mut self) { self . version += 1 ; }",
      "Condition :: all () . add (active ()) . add (crate :: visible ())",
      ":: core :: clone :: Clone :: clone (& self . id)",
    ] {
      assert!(out.contains(part), "missing `{part}` in {out}");
    }
  }

  #[test]
  fn defaults_and_crate_override() {
    let out = expand_str(
      "#[entity(crate = my::sql)]
       struct LedgerEntry2Line<T> where T: Clone { #[entity(id)] key: String, t: T }",
    )
    .unwrap();
    for part in [
      "impl < T > my :: sql :: repository :: Entity for LedgerEntry2Line < T > where T : Clone",
      "const TABLE : & 'static str = \"ledger_entry2_line\" ;",
      "const ID : & 'static str = \"key\" ;",
      "const ID_GENERATED : bool = false ;",
      "const VERSION : :: core :: option :: Option < & 'static str > = :: core :: option :: Option :: None ;",
      "my :: sql :: SqlValue",
    ] {
      assert!(out.contains(part), "missing `{part}` in {out}");
    }
    assert!(!out.contains("bump_version"), "{out}");
    assert!(!out.contains("fn scope"), "{out}");
    assert_eq!(
      snake_case(&syn::parse_str("HTTPServer").unwrap()),
      "httpserver"
    );
  }

  #[test]
  fn rejects_shapes() {
    let err = expand_str("enum E { A }").unwrap_err();
    assert!(err.contains("named fields only"), "{err}");
    let err = expand_str("struct T(i64);").unwrap_err();
    assert!(err.contains("named fields only"), "{err}");
    let err = expand_str("struct S { a: i64 }").unwrap_err();
    assert!(err.contains("needs an `#[entity(id)]` field"), "{err}");
  }

  #[test]
  fn rejects_bad_attributes() {
    let cases = [
      (
        "#[entity(name = \"x\")] struct S { #[entity(id)] a: i64 }",
        "expected `table",
      ),
      (
        "#[entity(table = 1)] struct S { #[entity(id)] a: i64 }",
        "expected string literal",
      ),
      (
        "struct S { #[entity(key)] a: i64 }",
        "expected `id`, `generated` or `version`",
      ),
      (
        "struct S { #[entity(generated)] a: i64 }",
        "#[entity(id, generated)]",
      ),
      (
        "struct S { #[entity(id, version)] a: i64 }",
        "both the `id` and the `version`",
      ),
      (
        "struct S { #[entity(id)] a: i64, #[entity(id)] b: i64 }",
        "one `#[entity(id)]` field",
      ),
      (
        "struct S { #[entity(id)] a: i64, #[entity(version)] b: i64, #[entity(version)] c: i64 }",
        "one `#[entity(version)]` field",
      ),
      (
        "struct S { #[entity(id)] a: i64, #[from_row(flatten)] b: B }",
        "`#[from_row(flatten)]` fields are not supported",
      ),
      (
        "struct S { #[entity(id)] #[from_row(rename = 1)] a: i64 }",
        "expected string literal",
      ),
    ];
    for (src, expected) in cases {
      let err = expand_str(src).unwrap_err();
      assert!(err.contains(expected), "{src}: {err}");
    }
  }
}

//! `#[derive(Entity)]` — generate an `id_effect_sql::repository::Entity` impl mapping a
//! `#[derive(FromRow)]` struct to a table, its field and relation metadata, and one relation
//! constructor per declared relation.
//!
//! ```text
//! #[entity(table = "orders",              // default: the type name in snake case
//!          tenant = "org_id",             // scope every statement to the repository's tenant
//!          soft_delete = "deleted_at",    // hide rows whose column is not NULL
//!          scope = active_only,           // fn() -> impl Into<Condition>; repeatable
//!          label = "shop.order",          // Entity::LABEL (default: the snake-case name)
//!          belongs_to(Customer, foreign_key = "customer_id", name = "customer"),
//!          has_many(Line, foreign_key = "order_id", name = "lines"),
//!          many_to_many(Tag, through = "order_tags", local_key = "order_id",
//!                       foreign_key = "tag_id", name = "tags"),
//!          crate = path)]                 // all optional
//! struct Order {
//!   #[entity(id, generated)] id: i64,     // primary key; `generated`: assigned by the database
//!   #[from_row(rename = "title")] name: String, // columns are the FromRow columns
//!   #[entity(editable, label = "shop.order.note")] note: Option<String>, // admin metadata
//!   customer_id: i64,
//!   #[entity(version)] version: i64,      // optimistic locking
//! }
//! ```
//!
//! The columns follow `FromRow`'s rules (`#[from_row(rename)]`, else `#[schema("..")]`, else
//! the field name) so the two derives always agree; `#[from_row(flatten)]` fields have no single
//! column and are rejected. Each value is `<FieldTy as ToSqlValue>::to_sql_value(&self.field)`,
//! spanned at the field type, so a type without `ToSqlValue` is reported there.
//!
//! Relations: the first item is the target entity; `name` defaults to the target in snake case
//! (`belongs_to`) or that plus `s` (`has_many`, `many_to_many`); keys default to
//! `<target>_id` (`belongs_to`'s foreign key on this entity, which must be a column), `<self>_id`
//! (`has_many`'s foreign key on the target) and `<self>_id` / `<target>_id` (the join table's
//! `local_key` / `foreign_key`). Each becomes `Self::<name>() -> Relation<Self, Target>` and an
//! `Entity::RELATIONS` entry.
//!
//! Field metadata (`Entity::FIELDS`): `editable` marks a field generic forms may bind (never the
//! id, version, tenant or soft-delete column); `label` sets its label key (default
//! `<entity>.<field>`).

use proc_macro::TokenStream;
use proc_macro2::{Span, TokenStream as TokenStream2};
use quote::{ToTokens, quote, quote_spanned};
use syn::ext::IdentExt;
use syn::meta::ParseNestedMeta;
use syn::spanned::Spanned;
use syn::{
  Data, DeriveInput, Field, Fields, GenericArgument, Ident, LitStr, Path, PathArguments, Type,
  parse_macro_input,
};

pub(crate) fn derive(input: TokenStream) -> TokenStream {
  let input = parse_macro_input!(input as DeriveInput);
  match expand(input) {
    Ok(ts) => ts.into(),
    Err(e) => e.to_compile_error().into(),
  }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RelKind {
  BelongsTo,
  HasMany,
  ManyToMany,
}

impl RelKind {
  fn of(path: &Path) -> Option<Self> {
    if path.is_ident("belongs_to") {
      Some(Self::BelongsTo)
    } else if path.is_ident("has_many") {
      Some(Self::HasMany)
    } else if path.is_ident("many_to_many") {
      Some(Self::ManyToMany)
    } else {
      None
    }
  }

  fn attr(self) -> &'static str {
    match self {
      Self::BelongsTo => "belongs_to",
      Self::HasMany => "has_many",
      Self::ManyToMany => "many_to_many",
    }
  }
}

/// One `belongs_to(..)` / `has_many(..)` / `many_to_many(..)` as written.
struct RelAttr {
  kind: RelKind,
  target: Path,
  name: Option<LitStr>,
  foreign_key: Option<LitStr>,
  through: Option<LitStr>,
  local_key: Option<LitStr>,
  span: Span,
}

#[derive(Default)]
struct ContainerAttrs {
  table: Option<LitStr>,
  tenant: Option<LitStr>,
  soft_delete: Option<LitStr>,
  label: Option<LitStr>,
  scopes: Vec<Path>,
  relations: Vec<RelAttr>,
  krate: Option<Path>,
}

fn parse_relation(kind: RelKind, meta: &ParseNestedMeta<'_>) -> syn::Result<RelAttr> {
  let span = meta.path.span();
  let (mut name, mut foreign_key, mut through, mut local_key) = (None, None, None, None);
  let mut target = None;
  meta.parse_nested_meta(|m| {
    let join_table = kind == RelKind::ManyToMany;
    if m.path.is_ident("name") {
      name = Some(m.value()?.parse()?);
    } else if m.path.is_ident("foreign_key") {
      foreign_key = Some(m.value()?.parse()?);
    } else if join_table && m.path.is_ident("through") {
      through = Some(m.value()?.parse()?);
    } else if join_table && m.path.is_ident("local_key") {
      local_key = Some(m.value()?.parse()?);
    } else if target.is_none() && (m.input.is_empty() || m.input.peek(syn::Token![,])) {
      target = Some(m.path.clone());
    } else {
      let keys = if join_table {
        "`through = \"..\"`, `local_key = \"..\"`, `foreign_key = \"..\"` or `name = \"..\"`"
      } else {
        "`foreign_key = \"..\"` or `name = \"..\"`"
      };
      return Err(m.error(format!(
        "expected the target entity, then {keys} in `{}(..)`",
        kind.attr()
      )));
    }
    Ok(())
  })?;
  let target = target.ok_or_else(|| {
    syn::Error::new(
      span,
      format!(
        "`{}(..)` needs the target entity first, e.g. `{}(Line, ..)`",
        kind.attr(),
        kind.attr()
      ),
    )
  })?;
  if kind == RelKind::ManyToMany && through.is_none() {
    return Err(syn::Error::new(
      span,
      "`many_to_many(..)` needs its join table: `through = \"..\"`",
    ));
  }
  Ok(RelAttr {
    kind,
    target,
    name,
    foreign_key,
    through,
    local_key,
    span,
  })
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
      } else if meta.path.is_ident("label") {
        attrs.label = Some(meta.value()?.parse()?);
      } else if meta.path.is_ident("scope") {
        attrs.scopes.push(meta.value()?.parse()?);
      } else if meta.path.is_ident("crate") {
        attrs.krate = Some(meta.value()?.parse()?);
      } else if let Some(kind) = RelKind::of(&meta.path) {
        attrs.relations.push(parse_relation(kind, &meta)?);
      } else {
        return Err(meta.error(
          "expected `table = \"..\"`, `tenant = \"..\"`, `soft_delete = \"..\"`, `scope = path`, \
           `label = \"..\"`, `belongs_to(..)`, `has_many(..)`, `many_to_many(..)` or \
           `crate = path` on the struct",
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
  editable: bool,
  label: Option<LitStr>,
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
      } else if meta.path.is_ident("editable") {
        attrs.editable = true;
      } else if meta.path.is_ident("label") {
        attrs.label = Some(meta.value()?.parse()?);
      } else {
        return Err(
          meta.error("expected `id`, `generated`, `version`, `editable` or `label = \"..\"`"),
        );
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
  if attrs.editable && (attrs.id || attrs.version) {
    return Err(syn::Error::new_spanned(
      field,
      "the `id` and `version` columns are never `editable` (no form may write them)",
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

/// `T` when `ty` is `Option<T>` (by its last path segment).
fn option_inner(ty: &Type) -> Option<&Type> {
  let Type::Path(path) = ty else {
    return None;
  };
  let segment = path.path.segments.last()?;
  let PathArguments::AngleBracketed(args) = &segment.arguments else {
    return None;
  };
  match (
    segment.ident == "Option",
    args.args.first(),
    args.args.len(),
  ) {
    (true, Some(GenericArgument::Type(inner)), 1) => Some(inner),
    _ => None,
  }
}

/// `ty` as written, without the token spacing `quote` adds (`Vec < u8 >` → `Vec<u8>`).
fn type_name(ty: &Type) -> String {
  ty.to_token_stream()
    .to_string()
    .replace(" :: ", "::")
    .replace(":: ", "::")
    .replace(" < ", "<")
    .replace("< ", "<")
    .replace(" >", ">")
    .replace(" ,", ",")
    .replace("& ", "&")
}

/// A relation with its defaults resolved.
struct Resolved {
  kind: RelKind,
  target: Path,
  name: LitStr,
  ident: Ident,
  foreign_key: LitStr,
  through: Option<LitStr>,
  local_key: LitStr,
}

fn resolve(rel: RelAttr, entity: &str, columns: &[LitStr]) -> syn::Result<Resolved> {
  let span = rel.span;
  let target = rel
    .target
    .segments
    .last()
    .map_or_else(String::new, |s| snake_case(&s.ident));
  let lit = |value: String| LitStr::new(&value, span);
  let name = rel.name.unwrap_or_else(|| match rel.kind {
    RelKind::BelongsTo => lit(target.clone()),
    RelKind::HasMany | RelKind::ManyToMany => lit(format!("{target}s")),
  });
  let ident = syn::parse_str::<Ident>(&name.value()).map_err(|_| {
    syn::Error::new(
      name.span(),
      format!(
        "relation name `{}` must be an identifier (it names `Self::{}()`)",
        name.value(),
        name.value()
      ),
    )
  })?;
  let foreign_key = rel.foreign_key.unwrap_or_else(|| match rel.kind {
    RelKind::BelongsTo | RelKind::ManyToMany => lit(format!("{target}_id")),
    RelKind::HasMany => lit(format!("{entity}_id")),
  });
  if rel.kind == RelKind::BelongsTo && !columns.iter().any(|c| c.value() == foreign_key.value()) {
    return Err(syn::Error::new(
      foreign_key.span(),
      format!(
        "`belongs_to` foreign key `{}` is not a column of this entity (name it with \
         `foreign_key = \"..\"`)",
        foreign_key.value()
      ),
    ));
  }
  Ok(Resolved {
    kind: rel.kind,
    target: rel.target,
    name,
    ident,
    foreign_key,
    through: rel.through,
    local_key: rel.local_key.unwrap_or_else(|| lit(format!("{entity}_id"))),
  })
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
    .clone()
    .unwrap_or_else(|| syn::parse_quote!(::id_effect_sql));
  let (impl_generics, ty_generics, where_clause) = input.generics.split_for_impl();
  let entity = snake_case(name);
  let protected = [attrs.tenant.as_ref(), attrs.soft_delete.as_ref()];

  let (mut id, mut version) = (None, None);
  let (mut columns, mut values, mut field_attrs) = (Vec::new(), Vec::new(), Vec::new());
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
    if fa.editable
      && protected
        .iter()
        .flatten()
        .any(|c| c.value() == column.value())
    {
      return Err(syn::Error::new_spanned(
        field,
        "the tenant and soft-delete columns are never `editable` (the repository writes them)",
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
    field_attrs.push((field, fa));
  }
  let Some((id_ident, id_ty, id_column, generated)) = id else {
    return Err(syn::Error::new(
      name.span(),
      "#[derive(Entity)] needs an `#[entity(id)]` field (the primary key)",
    ));
  };

  let mut relations: Vec<Resolved> = Vec::new();
  for rel in attrs.relations {
    let rel = resolve(rel, &entity, &columns)?;
    if relations.iter().any(|r| r.name.value() == rel.name.value()) {
      return Err(syn::Error::new(
        rel.name.span(),
        format!(
          "two relations are named `{}` (set `name = \"..\"`)",
          rel.name.value()
        ),
      ));
    }
    relations.push(rel);
  }

  let fields_meta = field_attrs
    .iter()
    .zip(&columns)
    .map(|((field, fa), column)| {
      let ident = field.ident.as_ref().expect("named field");
      let field_name = ident.unraw().to_string();
      let (ty, nullable) = match option_inner(&field.ty) {
        Some(inner) => (type_name(inner), true),
        None => (type_name(&field.ty), false),
      };
      let label = fa
        .label
        .clone()
        .unwrap_or_else(|| LitStr::new(&format!("{entity}.{field_name}"), ident.span()));
      let editable = fa.editable;
      let relation = relations
        .iter()
        .find(|r| r.kind == RelKind::BelongsTo && r.foreign_key.value() == column.value())
        .map(|r| {
          let name = &r.name;
          quote! { .relation(#name) }
        });
      quote! {
        #krate::repository::FieldMeta::new(#field_name, #column, #ty)
          .nullable(#nullable)
          .editable(#editable)
          .label(#label)
          #relation
      }
    })
    .collect::<Vec<_>>();

  let relations_meta = relations.iter().map(|r| {
    let (name, target, fk) = (&r.name, &r.target, &r.foreign_key);
    let kind = match r.kind {
      RelKind::BelongsTo => quote! { BelongsTo { foreign_key: #fk } },
      RelKind::HasMany => quote! { HasMany { foreign_key: #fk } },
      RelKind::ManyToMany => {
        let (through, local) = (&r.through, &r.local_key);
        quote! { ManyToMany { through: #through, local_key: #local, foreign_key: #fk } }
      }
    };
    quote! {
      #krate::repository::RelationMeta::new(
        #name,
        #krate::repository::RelationKind::#kind,
        <#target as #krate::repository::Entity>::NAME,
        <#target as #krate::repository::Entity>::TABLE,
      )
    }
  });

  let constructors = relations.iter().map(|r| {
    let (ident, name, target, fk) = (&r.ident, &r.name, &r.target, &r.foreign_key);
    let vis = &input.vis;
    let target_name = type_name(&syn::parse_quote!(#target));
    let (build, doc) = match (r.kind, &r.through) {
      (RelKind::ManyToMany, Some(through)) => {
        let local = &r.local_key;
        (
          quote! { many_to_many(#name, #through, #local, #fk) },
          format!(
            "The `{}` relation: the `{target_name}` rows paired with this row in `{}` \
             (`many_to_many`).",
            name.value(),
            through.value()
          ),
        )
      }
      (RelKind::HasMany, _) => (
        quote! { has_many(#name, #fk) },
        format!(
          "The `{}` relation: the `{target_name}` rows whose `{}` is this row's id \
           (`has_many`).",
          name.value(),
          fk.value()
        ),
      ),
      _ => (
        quote! { belongs_to(#name, #fk) },
        format!(
          "The `{}` relation: the `{target_name}` this row's `{}` references (`belongs_to`).",
          name.value(),
          fk.value()
        ),
      ),
    };
    quote! {
      #[doc = #doc]
      #[allow(dead_code)]
      #vis fn #ident() -> #krate::repository::Relation<Self, #target> {
        #krate::repository::Relation::#build
      }
    }
  });
  let constructors = (!relations.is_empty()).then(|| {
    quote! {
      #[automatically_derived]
      impl #impl_generics #name #ty_generics #where_clause {
        #(#constructors)*
      }
    }
  });

  let name_str = name.unraw().to_string();
  let table = attrs
    .table
    .unwrap_or_else(|| LitStr::new(&entity, name.span()));
  let label = attrs
    .label
    .unwrap_or_else(|| LitStr::new(&entity, name.span()));
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
      const LABEL: &'static str = #label;
      const FIELDS: &'static [#krate::repository::FieldMeta] = &[#(#fields_meta),*];
      const RELATIONS: &'static [#krate::repository::RelationMeta] = &[#(#relations_meta),*];

      fn id(&self) -> Self::Id {
        ::core::clone::Clone::clone(&self.#id_ident)
      }

      fn values(&self) -> ::std::vec::Vec<(&'static str, #krate::SqlValue)> {
        ::std::vec![#(#values),*]
      }

      #bump
      #scope
    }

    #constructors
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
      "const LABEL : & 'static str = \"account\" ;",
      "(\"title\" , < String as :: id_effect_sql :: ToSqlValue > :: to_sql_value (& self . name))",
      "(\"type\" , < String as :: id_effect_sql :: ToSqlValue > :: to_sql_value (& self . r#type))",
      "fn bump_version (& mut self) { self . version += 1 ; }",
      "Condition :: all () . add (active ()) . add (crate :: visible ())",
      ":: core :: clone :: Clone :: clone (& self . id)",
      "const RELATIONS : & 'static [:: id_effect_sql :: repository :: RelationMeta] = & [] ;",
    ] {
      assert!(out.contains(part), "missing `{part}` in {out}");
    }
  }

  #[test]
  fn fields_carry_type_nullability_editability_and_labels() {
    let out = expand_str(
      "#[entity(label = \"crm.account\")]
       struct Account {
         #[entity(id)] id: i64,
         #[entity(editable, label = \"crm.account.nick\")] #[from_row(rename = \"nick_name\")]
         nick: Option<std::string::String>,
         #[entity(editable)] tags: Vec<u8>,
         pairs: ::std::collections::HashMap<String, i64>,
         r#type: &'static str,
       }",
    )
    .unwrap();
    for part in [
      "const LABEL : & 'static str = \"crm.account\" ;",
      "FieldMeta :: new (\"id\" , \"id\" , \"i64\") . nullable (false) . editable (false) . label (\"account.id\")",
      "FieldMeta :: new (\"nick\" , \"nick_name\" , \"std::string::String\") . nullable (true) . editable (true) . label (\"crm.account.nick\")",
      "FieldMeta :: new (\"tags\" , \"tags\" , \"Vec<u8>\") . nullable (false) . editable (true)",
      "FieldMeta :: new (\"pairs\" , \"pairs\" , \"::std::collections::HashMap<String, i64>\")",
      "FieldMeta :: new (\"type\" , \"type\" , \"&'static str\")",
    ] {
      assert!(out.contains(part), "missing `{part}` in {out}");
    }
    assert!(
      !out.contains("impl Account"),
      "no relations, no constructors: {out}"
    );
    assert!(option_inner(&syn::parse_str("Option").unwrap()).is_none());
    assert!(option_inner(&syn::parse_str("Option<A, B>").unwrap()).is_none());
    assert!(option_inner(&syn::parse_str("[u8; 4]").unwrap()).is_none());
  }

  #[test]
  fn relations_generate_metadata_and_constructors() {
    let out = expand_str(
      "#[entity(table = \"orders\")]
       #[entity(belongs_to(Customer), has_many(crate::shop::OrderLine, name = \"lines\"))]
       #[entity(many_to_many(Tag, through = \"order_tags\"))]
       #[entity(belongs_to(Customer, foreign_key = \"billed_to\", name = \"payer\"))]
       #[entity(many_to_many(Tag, through = \"t\", local_key = \"o\", foreign_key = \"g\", name = \"labels\"))]
       pub struct ShopOrder {
         #[entity(id)] id: i64,
         customer_id: i64,
         billed_to: Option<i64>,
       }",
    )
    .unwrap();
    for part in [
      "RelationMeta :: new (\"customer\" , :: id_effect_sql :: repository :: RelationKind :: BelongsTo { foreign_key : \"customer_id\" } , < Customer as :: id_effect_sql :: repository :: Entity > :: NAME , < Customer as :: id_effect_sql :: repository :: Entity > :: TABLE ,)",
      "RelationKind :: HasMany { foreign_key : \"shop_order_id\" }",
      "< crate :: shop :: OrderLine as :: id_effect_sql :: repository :: Entity > :: NAME",
      "RelationKind :: ManyToMany { through : \"order_tags\" , local_key : \"shop_order_id\" , foreign_key : \"tag_id\" }",
      "RelationKind :: ManyToMany { through : \"t\" , local_key : \"o\" , foreign_key : \"g\" }",
      "pub fn customer () -> :: id_effect_sql :: repository :: Relation < Self , Customer > { :: id_effect_sql :: repository :: Relation :: belongs_to (\"customer\" , \"customer_id\") }",
      "pub fn lines () -> :: id_effect_sql :: repository :: Relation < Self , crate :: shop :: OrderLine > { :: id_effect_sql :: repository :: Relation :: has_many (\"lines\" , \"shop_order_id\") }",
      "pub fn tags () -> :: id_effect_sql :: repository :: Relation < Self , Tag > { :: id_effect_sql :: repository :: Relation :: many_to_many (\"tags\" , \"order_tags\" , \"shop_order_id\" , \"tag_id\") }",
      "the `crate::shop::OrderLine` rows whose `shop_order_id` is this row's id",
      "pub fn payer ()",
      "pub fn labels ()",
      "FieldMeta :: new (\"customer_id\" , \"customer_id\" , \"i64\") . nullable (false) . editable (false) . label (\"shop_order.customer_id\") . relation (\"customer\")",
      ". label (\"shop_order.billed_to\") . relation (\"payer\")",
      "impl ShopOrder {",
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
      "my :: sql :: repository :: FieldMeta :: new (\"t\" , \"t\" , \"T\")",
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
        "expected `id`, `generated`, `version`, `editable` or `label",
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
      (
        "struct S { #[entity(id, editable)] a: i64 }",
        "never `editable`",
      ),
      (
        "struct S { #[entity(id)] a: i64, #[entity(version, editable)] v: i64 }",
        "never `editable`",
      ),
      (
        "#[entity(tenant = \"org\")] struct S { #[entity(id)] a: i64, #[entity(editable)] org: i64 }",
        "tenant and soft-delete columns are never `editable`",
      ),
      (
        "struct S { #[entity(id)] a: i64, #[entity(label = 1)] b: i64 }",
        "expected string literal",
      ),
      (
        "#[entity(belongs_to(Customer))] struct S { #[entity(id)] a: i64 }",
        "`belongs_to` foreign key `customer_id` is not a column",
      ),
      (
        "#[entity(has_many(foreign_key = \"x\"))] struct S { #[entity(id)] a: i64 }",
        "needs the target entity first, e.g. `has_many(Line, ..)`",
      ),
      (
        "#[entity(has_many(Line, through = \"x\"))] struct S { #[entity(id)] a: i64 }",
        "expected the target entity, then `foreign_key = \"..\"` or `name = \"..\"` in `has_many(..)`",
      ),
      (
        "#[entity(many_to_many(Tag, Other))] struct S { #[entity(id)] a: i64 }",
        "`through = \"..\"`, `local_key",
      ),
      (
        "#[entity(many_to_many(Tag))] struct S { #[entity(id)] a: i64 }",
        "needs its join table",
      ),
      (
        "#[entity(has_many(Line), has_many(Line))] struct S { #[entity(id)] a: i64 }",
        "two relations are named `lines`",
      ),
      (
        "#[entity(has_many(Line, name = \"not ok\"))] struct S { #[entity(id)] a: i64 }",
        "relation name `not ok` must be an identifier",
      ),
      (
        "#[entity(has_many(Line, name = 1))] struct S { #[entity(id)] a: i64 }",
        "expected string literal",
      ),
    ];
    for (src, expected) in cases {
      let err = expand_str(src).unwrap_err();
      assert!(err.contains(expected), "{src}: {err}");
    }
  }

  fn assert_parts(out: &str, parts: &[&str]) {
    for part in parts {
      assert!(out.contains(part), "missing `{part}` in {out}");
    }
  }

  #[test]
  fn has_many_default_name_pluralises_the_snake_case_target() {
    let out = expand_str("#[entity(has_many(OrderLine))] struct Order { #[entity(id)] id: i64 }");
    assert!(
      out
        .unwrap()
        .contains("has_many (\"order_lines\" , \"order_id\")")
    );
  }

  #[test]
  fn belongs_to_custom_name_keeps_the_default_key() {
    let out = expand_str(
      "#[entity(belongs_to(User, name = \"author\"))] struct Post { #[entity(id)] id: i64, user_id: i64 }",
    );
    assert!(
      out
        .unwrap()
        .contains("belongs_to (\"author\" , \"user_id\")")
    );
  }

  #[test]
  fn many_to_many_local_key_alone_keeps_the_default_foreign_key() {
    let src = "#[entity(many_to_many(Role, through = \"m\", local_key = \"member\"))] struct Team { #[entity(id)] id: i64 }";
    let out = expand_str(src).unwrap();
    assert!(out.contains("many_to_many (\"roles\" , \"m\" , \"member\" , \"role_id\")"));
  }

  #[test]
  fn entity_without_tenant_or_soft_delete_has_no_filters() {
    let out = expand_str("struct Plain { #[entity(id)] id: i64 }").unwrap();
    assert!(out.contains("const TENANT : :: core :: option :: Option < & 'static str > = :: core :: option :: Option :: None ;"));
    assert!(out.contains("const SOFT_DELETE : :: core :: option :: Option < & 'static str > = :: core :: option :: Option :: None ;"));
  }

  #[test]
  fn has_many_rejects_join_table_keys() {
    let err =
      expand_str("#[entity(has_many(Line, local_key = \"x\"))] struct S { #[entity(id)] a: i64 }");
    assert!(err.unwrap_err().contains("in `has_many(..)`"));
  }

  #[test]
  fn belongs_to_rejects_a_second_target() {
    let err =
      expand_str("#[entity(belongs_to(A, B))] struct S { #[entity(id)] a: i64, a_id: i64 }");
    assert!(err.unwrap_err().contains("in `belongs_to(..)`"));
  }

  #[test]
  fn rel_kind_recognises_only_the_three_relation_attributes() {
    let path = |s: &str| syn::parse_str::<Path>(s).unwrap();
    assert_eq!(RelKind::of(&path("belongs_to")), Some(RelKind::BelongsTo));
    assert_eq!(RelKind::of(&path("has_many")), Some(RelKind::HasMany));
    assert_eq!(
      RelKind::of(&path("many_to_many")),
      Some(RelKind::ManyToMany)
    );
    assert_eq!(RelKind::of(&path("has_one")), None);
    assert_eq!(RelKind::of(&path("crate::has_many")), None);
    assert_eq!(RelKind::BelongsTo.attr(), "belongs_to");
    assert_eq!(RelKind::HasMany.attr(), "has_many");
    assert_eq!(RelKind::ManyToMany.attr(), "many_to_many");
  }

  #[test]
  fn snake_case_splits_words_and_unraws() {
    let snake = |s: &str| snake_case(&syn::parse_str::<Ident>(s).unwrap());
    assert_eq!(snake("Order"), "order");
    assert_eq!(snake("OrderLine"), "order_line");
    assert_eq!(snake("Line2Item"), "line2_item");
    assert_eq!(snake("already_snake"), "already_snake");
    assert_eq!(
      snake_case(&Ident::new_raw("type", Span::call_site())),
      "type"
    );
  }

  #[test]
  fn type_name_and_option_inner_render_field_types() {
    let ty = |s: &str| syn::parse_str::<Type>(s).unwrap();
    assert_eq!(type_name(&ty("Vec<u8>")), "Vec<u8>");
    assert_eq!(type_name(&ty("&'static str")), "&'static str");
    assert_eq!(type_name(&ty("Option<Vec<u8>>")), "Option<Vec<u8>>");
    assert_eq!(
      type_name(&ty("std::collections::BTreeMap<String, i64>")),
      "std::collections::BTreeMap<String, i64>"
    );
    let inner = |s: &str| option_inner(&ty(s)).map(type_name);
    assert_eq!(inner("Option<i64>").as_deref(), Some("i64"));
    assert_eq!(
      inner("std::option::Option<String>").as_deref(),
      Some("String")
    );
    assert_eq!(inner("Option<Vec<u8>>").as_deref(), Some("Vec<u8>"));
    assert_eq!(inner("Vec<i64>"), None);
    assert_eq!(inner("Option<'a>"), None);
  }

  #[test]
  fn option_renders_some_and_none() {
    let lit = LitStr::new("deleted_at", Span::call_site());
    assert_eq!(
      option(Some(&lit)).to_string(),
      ":: core :: option :: Option :: Some (\"deleted_at\")"
    );
    assert_eq!(
      option(None).to_string(),
      ":: core :: option :: Option :: None"
    );
  }

  #[test]
  fn belongs_to_defaults_name_and_key_from_the_target_path() {
    let out = expand_str(
      "#[entity(belongs_to(crate::crm::CustomerAccount))]
       struct Invoice { #[entity(id)] id: i64, customer_account_id: i64 }",
    )
    .unwrap();
    assert_parts(
      &out,
      &[
        "RelationKind :: BelongsTo { foreign_key : \"customer_account_id\" }",
        "< crate :: crm :: CustomerAccount as :: id_effect_sql :: repository :: Entity > :: TABLE",
        "# [allow (dead_code)] fn customer_account () -> :: id_effect_sql :: repository :: Relation < Self , crate :: crm :: CustomerAccount >",
        "Relation :: belongs_to (\"customer_account\" , \"customer_account_id\")",
        "the `crate::crm::CustomerAccount` this row's `customer_account_id` references (`belongs_to`)",
        ". label (\"invoice.customer_account_id\") . relation (\"customer_account\")",
      ],
    );
    assert!(
      !out.contains("pub fn customer_account"),
      "a private entity gets private constructors: {out}"
    );
  }

  #[test]
  fn belongs_to_matches_the_renamed_column_not_the_field_name() {
    let out = expand_str(
      "#[entity(belongs_to(Customer, foreign_key = \"cust\"))]
       struct Order {
         #[entity(id)] id: i64,
         #[from_row(rename = \"cust\")] customer: i64,
       }",
    )
    .unwrap();
    assert_parts(
      &out,
      &[
        "FieldMeta :: new (\"customer\" , \"cust\" , \"i64\") . nullable (false) . editable (false) . label (\"order.customer\") . relation (\"customer\")",
        "Relation :: belongs_to (\"customer\" , \"cust\")",
      ],
    );
    let err = expand_str(
      "#[entity(belongs_to(Customer, foreign_key = \"customer\"))]
       struct Order { #[entity(id)] id: i64, #[from_row(rename = \"cust\")] customer: i64 }",
    )
    .unwrap_err();
    assert!(
      err.contains("`belongs_to` foreign key `customer` is not a column"),
      "{err}"
    );
  }

  #[test]
  fn has_many_takes_an_explicit_foreign_key_on_the_target() {
    let out = expand_str(
      "#[entity(has_many(Line, foreign_key = \"parent\"))]
       pub(crate) struct Order { #[entity(id)] id: i64 }",
    )
    .unwrap();
    assert_parts(
      &out,
      &[
        "RelationMeta :: new (\"lines\" , :: id_effect_sql :: repository :: RelationKind :: HasMany { foreign_key : \"parent\" }",
        "pub (crate) fn lines () -> :: id_effect_sql :: repository :: Relation < Self , Line >",
        "Relation :: has_many (\"lines\" , \"parent\")",
        "the `Line` rows whose `parent` is this row's id (`has_many`)",
      ],
    );
  }

  #[test]
  fn has_many_foreign_key_never_marks_a_field_as_a_relation() {
    let out = expand_str(
      "#[entity(has_many(Node, foreign_key = \"node_id\", name = \"children\"))]
       struct Node { #[entity(id)] id: i64, node_id: Option<i64> }",
    )
    .unwrap();
    assert_parts(
      &out,
      &[
        "FieldMeta :: new (\"node_id\" , \"node_id\" , \"i64\") . nullable (true)",
        "fn children ()",
      ],
    );
    assert!(
      !out.contains(". label (\"node.node_id\") . relation"),
      "only belongs_to keys are field relations: {out}"
    );
  }

  #[test]
  fn many_to_many_documents_its_join_table_and_defaults_its_keys() {
    let out = expand_str(
      "#[entity(many_to_many(Tag, through = \"post_tags\"))]
       struct BlogPost { #[entity(id)] id: i64 }",
    )
    .unwrap();
    assert_parts(
      &out,
      &[
        "RelationKind :: ManyToMany { through : \"post_tags\" , local_key : \"blog_post_id\" , foreign_key : \"tag_id\" }",
        "Relation :: many_to_many (\"tags\" , \"post_tags\" , \"blog_post_id\" , \"tag_id\")",
        "the `Tag` rows paired with this row in `post_tags` (`many_to_many`)",
      ],
    );
  }

  #[test]
  fn relations_keep_their_declared_order() {
    let out = expand_str(
      "#[entity(has_many(Zeta), belongs_to(Alpha, name = \"owner\"))]
       #[entity(many_to_many(Mid, through = \"j\"))]
       struct Doc { #[entity(id)] id: i64, alpha_id: i64 }",
    )
    .unwrap();
    let at = |needle: &str| {
      out
        .find(needle)
        .unwrap_or_else(|| panic!("{needle}: {out}"))
    };
    let (zetas, owner, mids) = (
      at("RelationMeta :: new (\"zetas\""),
      at("RelationMeta :: new (\"owner\""),
      at("RelationMeta :: new (\"mids\""),
    );
    assert!(zetas < owner && owner < mids, "{out}");
    let (f_zetas, f_owner, f_mids) = (at("fn zetas ()"), at("fn owner ()"), at("fn mids ()"));
    assert!(f_zetas < f_owner && f_owner < f_mids, "{out}");
  }

  #[test]
  fn relation_constructors_carry_the_entity_generics() {
    let out = expand_str(
      "#[entity(has_many(Line))]
       pub struct Holder<T> where T: Clone { #[entity(id)] id: i64, t: T }",
    )
    .unwrap();
    assert_parts(
      &out,
      &[
        "# [automatically_derived] impl < T > Holder < T > where T : Clone {",
        "pub fn lines () -> :: id_effect_sql :: repository :: Relation < Self , Line >",
        "Relation :: has_many (\"lines\" , \"holder_id\")",
      ],
    );
  }

  #[test]
  fn relation_names_must_be_unique_identifiers_across_kinds() {
    let err = expand_str(
      "#[entity(belongs_to(Customer, name = \"party\"), has_many(Line, name = \"party\"))]
       struct S { #[entity(id)] a: i64, customer_id: i64 }",
    )
    .unwrap_err();
    assert!(err.contains("two relations are named `party`"), "{err}");
    let err =
      expand_str("#[entity(has_many(Line, name = \"type\"))] struct S { #[entity(id)] a: i64 }")
        .unwrap_err();
    assert!(
      err.contains("relation name `type` must be an identifier"),
      "{err}"
    );
  }

  #[test]
  fn field_labels_default_to_entity_and_unraw_field_name() {
    let out = expand_str(
      "struct TaxRate {
         #[entity(id, label = \"rates.key\")] id: i64,
         r#type: String,
         #[entity(label = \"rates.pct\")] percent: f64,
       }",
    )
    .unwrap();
    assert_parts(
      &out,
      &[
        "FieldMeta :: new (\"id\" , \"id\" , \"i64\") . nullable (false) . editable (false) . label (\"rates.key\")",
        "FieldMeta :: new (\"type\" , \"type\" , \"String\") . nullable (false) . editable (false) . label (\"tax_rate.type\")",
        "FieldMeta :: new (\"percent\" , \"percent\" , \"f64\") . nullable (false) . editable (false) . label (\"rates.pct\")",
        "const LABEL : & 'static str = \"tax_rate\" ;",
      ],
    );
  }

  #[test]
  fn soft_delete_column_is_never_editable_but_may_carry_a_label() {
    let err = expand_str(
      "#[entity(soft_delete = \"removed\")]
       struct S { #[entity(id)] a: i64, #[entity(editable)] removed: Option<i64> }",
    )
    .unwrap_err();
    assert!(
      err.contains("tenant and soft-delete columns are never `editable`"),
      "{err}"
    );
    let out = expand_str(
      "#[entity(soft_delete = \"removed\", tenant = \"org\")]
       struct S {
         #[entity(id)] a: i64,
         #[entity(label = \"s.removed\")] removed: Option<i64>,
         #[entity(editable)] #[from_row(rename = \"title\")] org: String,
       }",
    )
    .unwrap();
    assert_parts(
      &out,
      &[
        "FieldMeta :: new (\"removed\" , \"removed\" , \"i64\") . nullable (true) . editable (false) . label (\"s.removed\")",
        "FieldMeta :: new (\"org\" , \"title\" , \"String\") . nullable (false) . editable (true)",
      ],
    );
  }
}

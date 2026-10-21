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
}

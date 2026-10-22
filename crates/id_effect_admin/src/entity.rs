//! How one entity appears in the admin: [`EntityAdmin<E>`], checked against the entity's field
//! metadata ([`Entity::FIELDS`]) when it is [registered](crate::AdminSite::register).
//!
//! Every name is a Rust field name of `E` (`#[derive(Entity)]` fills [`Entity::FIELDS`] with one
//! [`FieldMeta`] per field); a name the entity does not have, or a field that cannot do what it is
//! declared for (text search over a number, sorting by a nullable column), is a configuration
//! error ([`AdminConfigError`]) when the site is built, never a runtime surprise.

use std::fmt;
use std::marker::PhantomData;

use id_effect_sql::repository::SortOrder;
use id_effect_sql::{Entity, FieldMeta, SqlValue};

use crate::permission::{AdminAction, AdminPermission, AdminPermissionError};
use crate::value::{ValueKind, snake_case};

/// The default rows per list page.
pub const DEFAULT_PAGE_SIZE: u32 = 25;
/// The largest list page an entity may declare (the keyset page limit).
pub const MAX_PAGE_SIZE: u32 = 500;

/// A typed filter of the list page ([`EntityAdmin::filter`]): the rows whose field equals the
/// chosen value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Filter {
  choices: Option<Vec<String>>,
}

impl Filter {
  /// Any value of the field's type, parsed from the request (a yes / no choice for a `bool`
  /// field, a text box otherwise); a value that does not parse is ignored.
  pub fn exact() -> Self {
    Self { choices: None }
  }

  /// One of `choices` (each parsed as the field's type when the entity is registered); any other
  /// value is ignored.
  pub fn choices<S: Into<String>>(choices: impl IntoIterator<Item = S>) -> Self {
    Self {
      choices: Some(choices.into_iter().map(Into::into).collect()),
    }
  }
}

/// How the admin shows the entity `E`; see the [module docs](self).
///
/// ```
/// use id_effect_admin::{EntityAdmin, Filter};
/// use id_effect_sql::repository::SortOrder;
/// use id_effect_sql::{Entity, FromRow};
///
/// #[derive(Clone, Debug, FromRow, Entity)]
/// #[entity(table = "documents", tenant = "org_id")]
/// struct Document {
///   #[entity(id)]
///   id: i64,
///   title: String,
///   status: String,
/// }
///
/// let admin: EntityAdmin<Document> = EntityAdmin::default()
///   .list(["id", "title", "status"])
///   .search(["title"])
///   .filter("status", Filter::choices(["draft", "published"]))
///   .sortable(["title"])
///   .default_sort("title", SortOrder::Asc);
/// assert_eq!(admin.info().unwrap().slug, "document");
/// ```
pub struct EntityAdmin<E> {
  slug: Option<String>,
  list: Option<Vec<String>>,
  detail: Option<Vec<String>>,
  exclude: Vec<String>,
  search: Vec<String>,
  filters: Vec<(String, Filter)>,
  sortable: Vec<String>,
  default_sort: Option<(String, SortOrder)>,
  page_size: u32,
  /// The editing options: form fields, hooks, validation, new-record template, bulk actions
  /// (set by the builder methods of [`edit`](crate::edit) and [`bulk`](crate::bulk)).
  pub(crate) edit: crate::edit::EditOptions<E>,
  entity: PhantomData<fn() -> E>,
}

impl<E> Default for EntityAdmin<E> {
  fn default() -> Self {
    Self {
      slug: None,
      list: None,
      detail: None,
      exclude: Vec::new(),
      search: Vec::new(),
      filters: Vec::new(),
      sortable: Vec::new(),
      default_sort: None,
      page_size: DEFAULT_PAGE_SIZE,
      edit: crate::edit::EditOptions::default(),
      entity: PhantomData,
    }
  }
}

impl<E> Clone for EntityAdmin<E> {
  fn clone(&self) -> Self {
    Self {
      slug: self.slug.clone(),
      list: self.list.clone(),
      detail: self.detail.clone(),
      exclude: self.exclude.clone(),
      search: self.search.clone(),
      filters: self.filters.clone(),
      sortable: self.sortable.clone(),
      default_sort: self.default_sort.clone(),
      page_size: self.page_size,
      edit: self.edit.clone(),
      entity: PhantomData,
    }
  }
}

impl<E> fmt::Debug for EntityAdmin<E> {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("EntityAdmin")
      .field("entity", &std::any::type_name::<E>())
      .field("slug", &self.slug)
      .field("list", &self.list)
      .field("search", &self.search)
      .field("sortable", &self.sortable)
      .finish_non_exhaustive()
  }
}

fn names<S: Into<String>>(names: impl IntoIterator<Item = S>) -> Vec<String> {
  names.into_iter().map(Into::into).collect()
}

impl<E> EntityAdmin<E> {
  /// The entity's name in URLs and permissions (`/admin/<slug>`, `admin.<slug>.list`); by
  /// default the type name in snake case. Snake_case ASCII starting with a letter.
  #[must_use]
  pub fn slug(mut self, slug: impl Into<String>) -> Self {
    self.slug = Some(slug.into());
    self
  }

  /// The columns of the list page, in order (default: every shown field).
  #[must_use]
  pub fn list<S: Into<String>>(mut self, fields: impl IntoIterator<Item = S>) -> Self {
    self.list = Some(names(fields));
    self
  }

  /// The fields of the detail page, in order (default: every shown field).
  #[must_use]
  pub fn detail<S: Into<String>>(mut self, fields: impl IntoIterator<Item = S>) -> Self {
    self.detail = Some(names(fields));
    self
  }

  /// Fields never shown (secrets such as a password hash), on every page.
  #[must_use]
  pub fn exclude<S: Into<String>>(mut self, fields: impl IntoIterator<Item = S>) -> Self {
    self.exclude.extend(names(fields));
    self
  }

  /// The text fields the list page's search box matches (case-insensitive substring, any of
  /// them). No search box without one.
  #[must_use]
  pub fn search<S: Into<String>>(mut self, fields: impl IntoIterator<Item = S>) -> Self {
    self.search.extend(names(fields));
    self
  }

  /// A typed filter on `field`.
  #[must_use]
  pub fn filter(mut self, field: impl Into<String>, filter: Filter) -> Self {
    self.filters.push((field.into(), filter));
    self
  }

  /// The fields the list page can be sorted by (non-nullable fields of a sortable type).
  #[must_use]
  pub fn sortable<S: Into<String>>(mut self, fields: impl IntoIterator<Item = S>) -> Self {
    self.sortable.extend(names(fields));
    self
  }

  /// The list order when the request asks for none (default: the primary key ascending). The
  /// field need not be [`sortable`](Self::sortable) by the user.
  #[must_use]
  pub fn default_sort(mut self, field: impl Into<String>, order: SortOrder) -> Self {
    self.default_sort = Some((field.into(), order));
    self
  }

  /// Rows per list page, `1..=`[`MAX_PAGE_SIZE`] (default [`DEFAULT_PAGE_SIZE`]).
  #[must_use]
  pub fn page_size(mut self, rows: u32) -> Self {
    self.page_size = rows;
    self
  }
}

impl<E: Entity> EntityAdmin<E> {
  /// What the site will show for `E`, checked against [`Entity::FIELDS`].
  ///
  /// # Errors
  ///
  /// The first [`AdminConfigError`] of the configuration.
  pub fn info(&self) -> Result<EntityInfo, AdminConfigError> {
    self.spec().map(|spec| spec.info())
  }

  /// The checked configuration.
  pub(crate) fn spec(&self) -> Result<EntitySpec, AdminConfigError> {
    let entity = E::NAME;
    let slug = self.slug.clone().unwrap_or_else(|| snake_case(E::NAME));
    let permissions =
      AdminPermission::all_for(&slug).map_err(|reason| AdminConfigError::InvalidSlug {
        entity,
        slug: slug.clone(),
        reason,
      })?;
    if E::FIELDS.is_empty() {
      return Err(AdminConfigError::NoFieldMetadata { entity });
    }
    let field = |name: &str| -> Result<usize, AdminConfigError> {
      E::FIELDS
        .iter()
        .position(|f| f.name == name)
        .ok_or_else(|| AdminConfigError::UnknownField {
          entity,
          field: name.to_owned(),
        })
    };
    for name in &self.exclude {
      field(name)?;
    }
    let shown = |name: &str| -> Result<usize, AdminConfigError> {
      let at = field(name)?;
      if self.exclude.iter().any(|x| x == name) {
        return Err(AdminConfigError::Excluded {
          entity,
          field: name.to_owned(),
        });
      }
      Ok(at)
    };
    let all_shown = || -> Vec<usize> {
      (0..E::FIELDS.len())
        .filter(|at| !self.exclude.iter().any(|x| x == E::FIELDS[*at].name))
        .collect()
    };
    let pick = |names: &Option<Vec<String>>| -> Result<Vec<usize>, AdminConfigError> {
      match names {
        Some(names) => names.iter().map(|n| shown(n)).collect(),
        None => Ok(all_shown()),
      }
    };
    let list = pick(&self.list)?;
    let detail = pick(&self.detail)?;
    let id = E::FIELDS
      .iter()
      .position(|f| f.column == E::ID)
      .ok_or(AdminConfigError::UnsupportedId { entity })?;
    let id_kind = ValueKind::of(E::FIELDS[id].schema_type);
    if !id_kind.parses() || matches!(id_kind, ValueKind::F32 | ValueKind::F64) {
      return Err(AdminConfigError::UnsupportedId { entity });
    }
    let mut search = Vec::with_capacity(self.search.len());
    for name in &self.search {
      let at = shown(name)?;
      if ValueKind::of(E::FIELDS[at].schema_type) != ValueKind::Text {
        return Err(AdminConfigError::NotSearchable {
          entity,
          field: name.clone(),
        });
      }
      search.push(E::FIELDS[at].column);
    }
    let sortable_field = |name: &str| -> Result<usize, AdminConfigError> {
      let at = shown(name)?;
      let meta = &E::FIELDS[at];
      if meta.nullable || !ValueKind::of(meta.schema_type).sorts() {
        return Err(AdminConfigError::NotSortable {
          entity,
          field: name.to_owned(),
        });
      }
      Ok(at)
    };
    let sortable = self
      .sortable
      .iter()
      .map(|n| sortable_field(n))
      .collect::<Result<Vec<_>, _>>()?;
    let default_sort = match &self.default_sort {
      Some((name, order)) => Some((sortable_field(name)?, *order)),
      None => None,
    };
    let mut filters = Vec::with_capacity(self.filters.len());
    for (name, filter) in &self.filters {
      let at = shown(name)?;
      let kind = ValueKind::of(E::FIELDS[at].schema_type);
      if !kind.parses() {
        return Err(AdminConfigError::NotFilterable {
          entity,
          field: name.clone(),
        });
      }
      let choices = match &filter.choices {
        None => None,
        Some(choices) => Some(
          choices
            .iter()
            .map(|c| {
              kind
                .parse(c)
                .map(|v| (c.clone(), v))
                .ok_or_else(|| AdminConfigError::InvalidChoice {
                  entity,
                  field: name.clone(),
                  choice: c.clone(),
                })
            })
            .collect::<Result<Vec<_>, _>>()?,
        ),
      };
      filters.push(FilterSpec {
        field: at,
        kind,
        choices,
      });
    }
    if !(1..=MAX_PAGE_SIZE).contains(&self.page_size) {
      return Err(AdminConfigError::PageSize {
        entity,
        size: self.page_size,
      });
    }
    let edit = crate::edit::EditSpec::check::<E>(&self.edit, &self.exclude, &search)?;
    Ok(EntitySpec {
      slug,
      permissions,
      entity,
      table: E::TABLE,
      label: E::LABEL,
      fields: E::FIELDS,
      id,
      id_kind,
      list,
      detail,
      search,
      filters,
      sortable,
      default_sort,
      page_size: self.page_size,
      edit,
    })
  }
}

/// A filter, checked: the field, its kind and the parsed choices.
#[derive(Clone, Debug)]
pub(crate) struct FilterSpec {
  pub(crate) field: usize,
  pub(crate) kind: ValueKind,
  pub(crate) choices: Option<Vec<(String, SqlValue)>>,
}

/// An [`EntityAdmin`] checked against its entity: indices into `fields`.
#[derive(Clone, Debug)]
pub(crate) struct EntitySpec {
  pub(crate) slug: String,
  /// One per action, in [`AdminAction::ALL`] (declaration) order.
  pub(crate) permissions: Vec<AdminPermission>,
  pub(crate) entity: &'static str,
  pub(crate) table: &'static str,
  pub(crate) label: &'static str,
  pub(crate) fields: &'static [FieldMeta],
  pub(crate) id: usize,
  pub(crate) id_kind: ValueKind,
  pub(crate) list: Vec<usize>,
  pub(crate) detail: Vec<usize>,
  pub(crate) search: Vec<&'static str>,
  pub(crate) filters: Vec<FilterSpec>,
  pub(crate) sortable: Vec<usize>,
  pub(crate) default_sort: Option<(usize, SortOrder)>,
  pub(crate) page_size: u32,
  /// Forms, relations and bulk actions, checked.
  pub(crate) edit: crate::edit::EditSpec,
}

impl EntitySpec {
  pub(crate) fn permission(&self, action: AdminAction) -> AdminPermission {
    self.permissions[action as usize].clone()
  }

  pub(crate) fn id_field(&self) -> &'static FieldMeta {
    &self.fields[self.id]
  }

  pub(crate) fn info(&self) -> EntityInfo {
    let names = |at: &[usize]| at.iter().map(|i| self.fields[*i].name).collect();
    EntityInfo {
      slug: self.slug.clone(),
      entity: self.entity,
      table: self.table,
      label: self.label,
      list: names(&self.list),
      detail: names(&self.detail),
      search: self
        .search
        .iter()
        .filter_map(|c| self.fields.iter().find(|f| f.column == *c).map(|f| f.name))
        .collect(),
      filters: self
        .filters
        .iter()
        .map(|f| self.fields[f.field].name)
        .collect(),
      sortable: names(&self.sortable),
      page_size: self.page_size,
      permissions: self.permissions.clone(),
      form: names(&self.edit.form),
      actions: self.edit.actions.iter().map(|a| a.name.clone()).collect(),
      relations: self.edit.relations.iter().map(|r| r.name).collect(),
    }
  }
}

/// What the admin shows for one registered entity ([`EntityAdmin::info`],
/// [`AdminSite::entities`](crate::AdminSite::entities)). Field lists hold Rust field names.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct EntityInfo {
  /// The name in URLs and permissions.
  pub slug: String,
  /// The Rust type name ([`Entity::NAME`]).
  pub entity: &'static str,
  /// The table.
  pub table: &'static str,
  /// The label key ([`Entity::LABEL`]).
  pub label: &'static str,
  /// The list page's columns.
  pub list: Vec<&'static str>,
  /// The detail page's fields.
  pub detail: Vec<&'static str>,
  /// The fields the search box matches.
  pub search: Vec<&'static str>,
  /// The filtered fields.
  pub filters: Vec<&'static str>,
  /// The sortable fields.
  pub sortable: Vec<&'static str>,
  /// Rows per list page.
  pub page_size: u32,
  /// The entity's permissions, one per [`AdminAction`] in [`AdminAction::ALL`] order.
  pub permissions: Vec<AdminPermission>,
  /// The fields the create and edit forms bind ([`EntityAdmin::form`]).
  pub form: Vec<&'static str>,
  /// The custom bulk actions, by name ([`EntityAdmin::action`]).
  pub actions: Vec<String>,
  /// The declared relations ([`Entity::RELATIONS`]), by name.
  pub relations: Vec<&'static str>,
}

/// Why an [`EntityAdmin`] (or an [`AdminSite`](crate::AdminSite)) configuration was refused.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum AdminConfigError {
  /// The slug is not a valid entity name.
  InvalidSlug {
    /// The entity type.
    entity: &'static str,
    /// The slug.
    slug: String,
    /// Why it is invalid.
    reason: AdminPermissionError,
  },
  /// Two registered entities share a slug.
  DuplicateSlug {
    /// The slug.
    slug: String,
  },
  /// The entity has no [`Entity::FIELDS`] (a hand-written `impl Entity` without metadata).
  NoFieldMetadata {
    /// The entity type.
    entity: &'static str,
  },
  /// A name is not a field of the entity.
  UnknownField {
    /// The entity type.
    entity: &'static str,
    /// The name.
    field: String,
  },
  /// An excluded field is named for a page, the search, a filter or the sort.
  Excluded {
    /// The entity type.
    entity: &'static str,
    /// The field.
    field: String,
  },
  /// The primary key is not among the fields, or of a type the admin cannot read from a path.
  UnsupportedId {
    /// The entity type.
    entity: &'static str,
  },
  /// A search field is not a text field.
  NotSearchable {
    /// The entity type.
    entity: &'static str,
    /// The field.
    field: String,
  },
  /// A sort field is nullable or of a type that cannot be a sort key.
  NotSortable {
    /// The entity type.
    entity: &'static str,
    /// The field.
    field: String,
  },
  /// A filter field is of a type the admin cannot parse.
  NotFilterable {
    /// The entity type.
    entity: &'static str,
    /// The field.
    field: String,
  },
  /// A filter choice does not parse as the field's type.
  InvalidChoice {
    /// The entity type.
    entity: &'static str,
    /// The field.
    field: String,
    /// The choice.
    choice: String,
  },
  /// The page size is outside `1..=`[`MAX_PAGE_SIZE`].
  PageSize {
    /// The entity type.
    entity: &'static str,
    /// The size.
    size: u32,
  },
  /// The site's path prefix is not `/` followed by path segments of `a-z 0-9 - _`.
  InvalidPrefix {
    /// The prefix.
    prefix: String,
  },
  /// A form field is not declared `#[entity(editable)]`, is excluded, or is of a type a form
  /// cannot parse ([`EntityAdmin::form`]).
  NotEditable {
    /// The entity type.
    entity: &'static str,
    /// The field.
    field: String,
  },
  /// A bulk action's name is not snake_case ASCII starting with a letter, is `delete`, or is
  /// registered twice ([`EntityAdmin::action`]).
  InvalidAction {
    /// The entity type.
    entity: &'static str,
    /// The action name.
    action: String,
  },
}

impl fmt::Display for AdminConfigError {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    match self {
      Self::InvalidSlug {
        entity,
        slug,
        reason,
      } => write!(f, "admin `{entity}`: slug `{slug}`: {reason}"),
      Self::DuplicateSlug { slug } => write!(f, "admin: two entities use the slug `{slug}`"),
      Self::NoFieldMetadata { entity } => write!(
        f,
        "admin `{entity}`: the entity has no field metadata (derive `Entity` or list FIELDS)"
      ),
      Self::UnknownField { entity, field } => {
        write!(f, "admin `{entity}`: `{field}` is not a field")
      }
      Self::Excluded { entity, field } => {
        write!(f, "admin `{entity}`: `{field}` is excluded")
      }
      Self::UnsupportedId { entity } => write!(
        f,
        "admin `{entity}`: the primary key must be a field of an integer, text, UUID or date type"
      ),
      Self::NotSearchable { entity, field } => {
        write!(f, "admin `{entity}`: `{field}` is not a text field")
      }
      Self::NotSortable { entity, field } => write!(
        f,
        "admin `{entity}`: `{field}` is nullable or of a type that cannot be sorted by"
      ),
      Self::NotFilterable { entity, field } => {
        write!(
          f,
          "admin `{entity}`: `{field}` is of a type that cannot be filtered"
        )
      }
      Self::InvalidChoice {
        entity,
        field,
        choice,
      } => write!(
        f,
        "admin `{entity}`: filter choice `{choice}` is not a value of `{field}`"
      ),
      Self::PageSize { entity, size } => write!(
        f,
        "admin `{entity}`: page size {size} is outside 1..={MAX_PAGE_SIZE}"
      ),
      Self::InvalidPrefix { prefix } => write!(
        f,
        "admin: path prefix `{prefix}` must be `/` followed by segments of a-z, 0-9, `-` and `_`"
      ),
      Self::NotEditable { entity, field } => write!(
        f,
        "admin `{entity}`: `{field}` is not an editable field (`#[entity(editable)]`, not \
         excluded, of a type a form can parse)"
      ),
      Self::InvalidAction { entity, action } => write!(
        f,
        "admin `{entity}`: bulk action `{action}` must be a unique snake_case name other than \
         `delete`"
      ),
    }
  }
}

impl std::error::Error for AdminConfigError {}

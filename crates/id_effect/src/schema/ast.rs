//! Introspectable description of a [`Schema`](crate::schema::parse::Schema) — [`SchemaAst`].
//!
//! A [`Schema`](crate::schema::parse::Schema) decodes through closures, which cannot be inspected.
//! Every built-in combinator therefore also records a [`SchemaAst`] describing the **wire** shape
//! it accepts: the kind ([`SchemaKind`]), declarative [`Constraints`] added by the built-in
//! validators (`min_length`, `range`, `email`, …), and [`Annotations`] (title, description,
//! examples, deprecation). The JSON Schema exporter
//! ([`json_schema_export`](crate::schema::json_schema_export), feature `schema-serde`) and value
//! generators read this tree; decoding never does, so the AST and the decoder describe the **same
//! schema value**.
//!
//! Schemas built directly with [`Schema::make`](crate::schema::parse::Schema::make) are opaque
//! ([`SchemaKind::Any`]) unless given an AST with
//! [`Schema::with_ast`](crate::schema::parse::Schema::with_ast). Opaque predicates
//! ([`filter`](crate::schema::parse::filter) / [`refine`](crate::schema::parse::refine)) and
//! [`transform`](crate::schema::parse::transform) keep the inner AST unchanged.

use std::fmt;
use std::sync::Arc;

use crate::schema::parse::Unknown;

/// Wire-shape description of a schema: kind + constraints + annotations.
#[derive(Clone, Debug, PartialEq)]
pub struct SchemaAst {
  /// Structural kind of the accepted wire value.
  pub kind: SchemaKind,
  /// Declarative constraints (length, range, pattern, enum, …).
  pub constraints: Constraints,
  /// Documentation annotations (title, description, examples, deprecation).
  pub annotations: Annotations,
}

impl SchemaAst {
  /// AST of `kind` with no constraints or annotations.
  pub fn new(kind: SchemaKind) -> Self {
    Self {
      kind,
      constraints: Constraints::default(),
      annotations: Annotations::default(),
    }
  }

  /// Opaque schema ([`SchemaKind::Any`]).
  pub fn any() -> Self {
    Self::new(SchemaKind::Any)
  }

  /// Replace the constraints.
  pub fn with_constraints(mut self, constraints: Constraints) -> Self {
    self.constraints = constraints;
    self
  }

  /// Replace the annotations.
  pub fn with_annotations(mut self, annotations: Annotations) -> Self {
    self.annotations = annotations;
    self
  }
}

impl Default for SchemaAst {
  fn default() -> Self {
    Self::any()
  }
}

/// Structural kind of a [`SchemaAst`].
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub enum SchemaKind {
  /// Opaque: built with [`Schema::make`](crate::schema::parse::Schema::make); accepts any value as
  /// far as introspection knows.
  Any,
  /// [`Unknown::Bool`].
  Boolean,
  /// [`Unknown::I64`].
  Integer,
  /// [`Unknown::F64`] (or [`Unknown::I64`], promoted).
  Number,
  /// [`Unknown::String`].
  String,
  /// Exactly this value (`literal_string`, `literal_i64`).
  Literal(Unknown),
  /// Homogeneous array of the inner schema.
  Array(Box<SchemaAst>),
  /// Fixed-length positional array.
  Tuple(Vec<SchemaAst>),
  /// Object with named fields in declaration order.
  Struct(Vec<Field>),
  /// Object with arbitrary string keys, all values of the inner schema.
  Record(Box<SchemaAst>),
  /// Inner schema or [`Unknown::Null`].
  Nullable(Box<SchemaAst>),
  /// First matching arm wins.
  Union(Vec<SchemaAst>),
  /// Deferred (recursive) schema built by `suspend` / `suspend_named`.
  Lazy(LazyAst),
}

/// One named field of a [`SchemaKind::Struct`].
#[derive(Clone, Debug, PartialEq)]
pub struct Field {
  /// Wire key.
  pub name: String,
  /// Field schema.
  pub schema: SchemaAst,
  /// Whether the key must be present (always `true` for the built-in `struct_*` combinators;
  /// `false` for `Option<_>` fields of `#[derive(Schema)]`).
  pub required: bool,
}

impl Field {
  /// Required field `name` with `schema`.
  pub fn required(name: impl Into<String>, schema: SchemaAst) -> Self {
    Self {
      name: name.into(),
      schema,
      required: true,
    }
  }

  /// Field `name` with `schema` whose key may be absent (JSON Schema: not listed in `required`).
  pub fn optional(name: impl Into<String>, schema: SchemaAst) -> Self {
    Self {
      name: name.into(),
      schema,
      required: false,
    }
  }
}

/// Deferred AST of a recursive schema; [`LazyAst::force`] builds the target AST on demand.
///
/// Named lazies (`suspend_named`) are exported as `$ref` into `$defs`, which is how recursion
/// terminates; unnamed ones are expanded a bounded number of times.
#[derive(Clone)]
pub struct LazyAst {
  name: Option<String>,
  thunk: Arc<dyn Fn() -> SchemaAst + Send + Sync>,
}

impl LazyAst {
  /// Lazy AST with an optional definition `name`.
  pub fn new(name: Option<String>, thunk: impl Fn() -> SchemaAst + Send + Sync + 'static) -> Self {
    Self {
      name,
      thunk: Arc::new(thunk),
    }
  }

  /// Definition name (`$defs` key), if any.
  pub fn name(&self) -> Option<&str> {
    self.name.as_deref()
  }

  /// Build the target AST (may itself contain further [`SchemaKind::Lazy`] nodes).
  pub fn force(&self) -> SchemaAst {
    (self.thunk)()
  }
}

impl fmt::Debug for LazyAst {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("LazyAst").field("name", &self.name).finish()
  }
}

impl PartialEq for LazyAst {
  /// Named lazies compare by name; unnamed ones by thunk identity.
  fn eq(&self, other: &Self) -> bool {
    match (&self.name, &other.name) {
      (Some(a), Some(b)) => a == b,
      (None, None) => Arc::ptr_eq(&self.thunk, &other.thunk),
      _ => false,
    }
  }
}

/// Declarative constraints recorded by the built-in validators.
///
/// Lengths count Unicode scalar values (`chars()`), matching JSON Schema `minLength`/`maxLength`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Constraints {
  /// Minimum string length.
  pub min_length: Option<usize>,
  /// Maximum string length.
  pub max_length: Option<usize>,
  /// ECMA-262 regular expression the string must match.
  pub pattern: Option<String>,
  /// Semantic string format (`email`, `uuid`, `uri`).
  pub format: Option<String>,
  /// Inclusive lower bound.
  pub minimum: Option<f64>,
  /// Inclusive upper bound.
  pub maximum: Option<f64>,
  /// Exclusive lower bound.
  pub exclusive_minimum: Option<f64>,
  /// Exclusive upper bound.
  pub exclusive_maximum: Option<f64>,
  /// Minimum array length.
  pub min_items: Option<usize>,
  /// Maximum array length.
  pub max_items: Option<usize>,
  /// Closed set of allowed values.
  pub enum_values: Option<Vec<Unknown>>,
}

impl Constraints {
  /// `true` when no constraint is set.
  pub fn is_empty(&self) -> bool {
    *self == Self::default()
  }
}

/// Documentation annotations (JSON Schema `title`, `description`, `examples`, `deprecated`).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Annotations {
  /// Short title.
  pub title: Option<String>,
  /// Longer description.
  pub description: Option<String>,
  /// Example wire values.
  pub examples: Vec<Unknown>,
  /// Default wire value (documentation only; decoding does not substitute it).
  pub default: Option<Unknown>,
  /// Marked deprecated.
  pub deprecated: bool,
}

impl Annotations {
  /// `true` when no annotation is set.
  pub fn is_empty(&self) -> bool {
    *self == Self::default()
  }
}

/// Tighten `slot` to the larger of the current and `new` lower bound.
pub(crate) fn tighten_min<T: PartialOrd + Copy>(slot: &mut Option<T>, new: T) {
  *slot = Some(match *slot {
    Some(old) if old > new => old,
    _ => new,
  });
}

/// Tighten `slot` to the smaller of the current and `new` upper bound.
pub(crate) fn tighten_max<T: PartialOrd + Copy>(slot: &mut Option<T>, new: T) {
  *slot = Some(match *slot {
    Some(old) if old < new => old,
    _ => new,
  });
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn new_ast_has_no_constraints_or_annotations() {
    let ast = SchemaAst::new(SchemaKind::String);
    assert!(ast.constraints.is_empty());
    assert!(ast.annotations.is_empty());
    assert_eq!(SchemaAst::default().kind, SchemaKind::Any);
  }

  #[test]
  fn field_constructors_set_required() {
    let req = Field::required("a", SchemaAst::any());
    let opt = Field::optional("b", SchemaAst::any());
    assert!(req.required && !opt.required);
    assert_eq!(opt.name, "b");
  }

  #[test]
  fn tighten_keeps_the_stricter_bound() {
    let mut lo = Some(5_usize);
    tighten_min(&mut lo, 3);
    assert_eq!(lo, Some(5));
    tighten_min(&mut lo, 8);
    assert_eq!(lo, Some(8));
    let mut hi = None;
    tighten_max(&mut hi, 10.0);
    tighten_max(&mut hi, 20.0);
    assert_eq!(hi, Some(10.0));
  }

  #[test]
  fn lazy_equality_by_name_or_identity() {
    let a = LazyAst::new(Some("T".into()), SchemaAst::any);
    let b = LazyAst::new(Some("T".into()), || SchemaAst::new(SchemaKind::String));
    assert_eq!(a, b);
    let c = LazyAst::new(None, SchemaAst::any);
    assert_eq!(c, c.clone());
    assert_ne!(c, LazyAst::new(None, SchemaAst::any));
    assert_ne!(a, c);
    assert_eq!(a.name(), Some("T"));
    assert_eq!(b.force().kind, SchemaKind::String);
    assert!(format!("{a:?}").contains("T"));
  }
}

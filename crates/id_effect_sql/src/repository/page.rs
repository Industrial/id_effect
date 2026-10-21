//! Keyset (cursor) pagination (data.orm, E3.3 "pagination"; ADR 0015 D11).
//!
//! [`Repository::find_after`] and [`Repository::find_before`] read one page of rows in a
//! [`Keyset`] order — the declared sort columns, then the primary key as the tie-breaker — and
//! return a [`Paged`] page with the [`PageCursor`]s of its neighbours. A page starts *after* (or
//! ends *before*) the sort-key tuple of the row a cursor points at, so rows inserted or deleted
//! elsewhere never shift a page the way an `OFFSET` does: the next page is the next rows. The
//! offset [`Page`](super::Page) ([`find_page`](Repository::find_page)) is kept for numbered
//! pages.
//!
//! **The cursor** (D11) is opaque to clients: base64url (no padding) of a small JSON document
//! holding the sort-key tuple and a fingerprint of the request (the [`Keyset`] filter and sort,
//! the entity's table and the repository's filter mode). It is at most [`MAX_CURSOR_BYTES`]
//! long and [`PageCursor::decode`] reads it through a bounded [`Schema`]. It is not signed: every
//! page query re-applies the repository's tenant, soft-delete and custom filters, so a forged
//! cursor only moves the page inside the caller's own rows. A cursor whose fingerprint differs
//! from the request's is refused ([`PageCursorError::FilterMismatch`]), so a cursor taken under one
//! filter cannot page another.
//!
//! **Page size** (ADR 0013 R4): [`DEFAULT_PAGE_SIZE`] (50) unless the request asks for another,
//! never more than the limit ([`MAX_PAGE_SIZE`], 500, configurable through [`PageLimits`]). A
//! larger request is capped, not refused.
//!
//! Sort columns must be mapped columns of the entity (anything else is refused before a query
//! runs) and should be `NOT NULL`: a row whose sort key is NULL cannot be a cursor position, so
//! building the cursor of such a row fails the page with [`SqlError::Unsupported`].
//!
//! ```rust
//! use id_effect::Exit;
//! use id_effect_sql::repository::{Keyset, PageCursor, SortOrder};
//! use id_effect_sql::{
//!   Dialect, Entity, FromRow, Repository, SqlClient, SqlClientEnv, SqlRow, SqlValue,
//!   TestSqlClient, sql_client_env,
//! };
//!
//! #[derive(Clone, Debug, PartialEq, FromRow, Entity)]
//! #[entity(table = "posts")]
//! struct Post {
//!   #[entity(id)]
//!   id: i64,
//!   score: i64,
//! }
//!
//! let test = TestSqlClient::new();
//! let _ = test.connect().block_on();
//! let post = |id: i64, score: i64| {
//!   SqlRow::from_pairs([("id", SqlValue::I64(id)), ("score", SqlValue::I64(score))])
//! };
//! test.script_query(
//!   r#"SELECT "id", "score" FROM "posts" ORDER BY "score" DESC, "id" ASC LIMIT $1"#,
//!   vec![post(4, 9), post(2, 7), post(3, 7)],
//! );
//!
//! let keyset = Keyset::new().order_by("score", SortOrder::Desc).size(2);
//! let page = Repository::<Post>::new(Dialect::Postgres)
//!   .find_after::<SqlClientEnv, _>(&keyset, None)
//!   .provide_all(sql_client_env(test.clone()))
//!   .block_on();
//! let Exit::Success(page) = page else { panic!("{page:?}") };
//! assert_eq!(page.items.iter().map(|p| p.id).collect::<Vec<_>>(), [4, 2]);
//! assert!(page.prev.is_none(), "the first page has no previous page");
//!
//! // The next page starts after (score 7, id 2): the token is what a client sends back.
//! let token = page.next.expect("a third row exists").encode();
//! let cursor = PageCursor::decode(&token).expect("a cursor this server issued");
//! assert_eq!(cursor.keys(), [SqlValue::I64(7), SqlValue::I64(2)]);
//! ```

use std::collections::BTreeMap;
use std::fmt;
use std::str::FromStr;
use std::sync::LazyLock;

use id_effect::schema::{
  ParseError, Schema, Unknown, array, filter, max_items, min_items, struct3,
};
use id_effect::{Effect, HList, Selector};
use sea_query::{Condition, Expr, ExprTrait, IntoCondition, Order, Query, Value};
use serde_json::json;

use super::relations::{EagerRepository, Loaded};
use super::{BoundRepository, Entity, Filters, Repository, Run, and, rows};
use crate::client::SqlClientService;
use crate::error::SqlError;
use crate::from_row::from_rows;
use crate::query_builder::{BuildSql, Dialect};
use crate::value::SqlValue;

/// The longest cursor token accepted or issued, in bytes (D11, ADR 0013 R4).
pub const MAX_CURSOR_BYTES: usize = 1024;
/// The page size when a [`Keyset`] does not ask for one.
pub const DEFAULT_PAGE_SIZE: u32 = 50;
/// The default largest page ([`PageLimits::max_size`]); larger requests are capped to it.
pub const MAX_PAGE_SIZE: u32 = 500;
/// The most sort columns a [`Keyset`] (and so a cursor) has, the primary key included.
pub const MAX_SORT_KEYS: usize = 8;

/// The cursor wire format version (`"v"`).
const CURSOR_VERSION: i64 = 1;

// ---- limits and the request ----

/// The page-size bounds of a [`Keyset`] (ADR 0013 R4): the size used when none is requested and
/// the largest page served. Both are at least 1; [`Default`] is [`DEFAULT_PAGE_SIZE`] and
/// [`MAX_PAGE_SIZE`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct PageLimits {
  default_size: u32,
  max_size: u32,
}

impl Default for PageLimits {
  fn default() -> Self {
    Self::new(DEFAULT_PAGE_SIZE, MAX_PAGE_SIZE)
  }
}

impl PageLimits {
  /// Pages of `default_size` rows unless a request asks otherwise, and never more than
  /// `max_size` rows. `max_size` is at least 1 and `default_size` is kept within `1..=max_size`.
  #[must_use]
  pub const fn new(default_size: u32, max_size: u32) -> Self {
    let max_size = if max_size == 0 { 1 } else { max_size };
    let default_size = if default_size == 0 {
      1
    } else if default_size > max_size {
      max_size
    } else {
      default_size
    };
    Self {
      default_size,
      max_size,
    }
  }

  /// The page size used when none is requested.
  #[inline]
  pub const fn default_size(&self) -> u32 {
    self.default_size
  }

  /// The largest page served.
  #[inline]
  pub const fn max_size(&self) -> u32 {
    self.max_size
  }

  /// The size served for a request of `requested` rows: the default when `None`, else
  /// `requested` capped to `1..=max_size`.
  #[must_use]
  pub fn page_size(&self, requested: Option<u32>) -> u32 {
    requested
      .unwrap_or(self.default_size)
      .clamp(1, self.max_size)
  }
}

/// The direction of one sort column.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SortOrder {
  /// Smallest first.
  Asc,
  /// Largest first.
  Desc,
}

impl SortOrder {
  fn sql(self) -> Order {
    match self {
      Self::Asc => Order::Asc,
      Self::Desc => Order::Desc,
    }
  }

  fn reversed(self) -> Self {
    match self {
      Self::Asc => Self::Desc,
      Self::Desc => Self::Asc,
    }
  }

  fn tag(self) -> &'static str {
    match self {
      Self::Asc => "asc",
      Self::Desc => "desc",
    }
  }
}

/// A keyset page request: the filter, the sort columns and the page size
/// ([`Repository::find_after`]).
///
/// The order is the [`order_by`](Self::order_by) columns, then the primary key ascending (unless
/// it is already one of them), so every row has one position. The filter and the order are what a
/// cursor's fingerprint covers: page with the same `Keyset` you took the cursor under.
#[derive(Clone, Debug)]
pub struct Keyset {
  sort: Vec<(String, SortOrder)>,
  filter: Condition,
  size: Option<u32>,
  limits: PageLimits,
}

impl Default for Keyset {
  fn default() -> Self {
    Self::new()
  }
}

impl Keyset {
  /// Pages in primary-key order of every row the repository's filters let through,
  /// [`DEFAULT_PAGE_SIZE`] rows each.
  #[must_use]
  pub fn new() -> Self {
    Self {
      sort: Vec::new(),
      filter: Condition::all(),
      size: None,
      limits: PageLimits::default(),
    }
  }

  /// Sort by `column` (a mapped column of the entity, checked when the page is read) after the
  /// columns already given.
  #[must_use]
  pub fn order_by(mut self, column: impl Into<String>, order: SortOrder) -> Self {
    self.sort.push((column.into(), order));
    self
  }

  /// Only the rows matching `cond` (AND-ed with earlier filters and, when read, with the
  /// repository's global filters).
  #[must_use]
  pub fn filter(mut self, cond: impl IntoCondition) -> Self {
    self.filter = and(self.filter, cond.into_condition());
    self
  }

  /// Ask for pages of `size` rows (capped to `1..=` [`PageLimits::max_size`]).
  #[must_use]
  pub fn size(mut self, size: u32) -> Self {
    self.size = Some(size);
    self
  }

  /// Use `limits` instead of the default ones.
  #[must_use]
  pub fn limits(mut self, limits: PageLimits) -> Self {
    self.limits = limits;
    self
  }

  /// The rows per page this request is served: the requested size within the limits.
  #[must_use]
  pub fn page_size(&self) -> u32 {
    self.limits.page_size(self.size)
  }

  /// The sort columns given with [`order_by`](Self::order_by) (without the primary-key
  /// tie-breaker).
  pub fn sort_columns(&self) -> impl Iterator<Item = (&str, SortOrder)> {
    self.sort.iter().map(|(c, o)| (c.as_str(), *o))
  }

  /// The order rows are read in: the declared columns resolved to the entity's columns, then the
  /// primary key. Unknown or repeated columns, or more than [`MAX_SORT_KEYS`], are refused.
  fn resolve<T: Entity>(&self) -> Result<Vec<(&'static str, SortOrder)>, SqlError> {
    let mut sort: Vec<(&'static str, SortOrder)> = Vec::with_capacity(self.sort.len() + 1);
    for (name, order) in &self.sort {
      let Some(col) = T::COLUMNS.iter().copied().find(|c| c == name) else {
        return Err(SqlError::Unsupported(format!(
          "entity `{}`: cannot sort by `{name}`, not a mapped column",
          T::NAME
        )));
      };
      if sort.iter().any(|(c, _)| *c == col) {
        return Err(SqlError::Unsupported(format!(
          "entity `{}`: sort column `{col}` is given twice",
          T::NAME
        )));
      }
      sort.push((col, *order));
    }
    if !sort.iter().any(|(c, _)| *c == T::ID) {
      sort.push((T::ID, SortOrder::Asc));
    }
    if sort.len() > MAX_SORT_KEYS {
      return Err(SqlError::Unsupported(format!(
        "entity `{}`: a keyset sorts by at most {MAX_SORT_KEYS} columns (primary key included), \
         got {}",
        T::NAME,
        sort.len()
      )));
    }
    Ok(sort)
  }
}

// ---- the page ----

/// One keyset page ([`Repository::find_after`], [`Repository::find_before`]): the rows in the
/// [`Keyset`] order and the cursors of the neighbouring pages.
///
/// `next` is the cursor of the last row (read the next page with
/// [`find_after`](Repository::find_after)); `prev` the cursor of the first row (read the previous
/// page with [`find_before`](Repository::find_before)). Each is `None` when no page lies that way:
/// `next` is known exactly when paging forward (one extra row is read), `prev` when paging back;
/// the other is set whenever the page was read from a cursor.
#[derive(Clone, Debug, PartialEq)]
pub struct Paged<T> {
  /// The rows, in the keyset order.
  pub items: Vec<T>,
  /// The cursor to read the next page after, if there is one.
  pub next: Option<PageCursor>,
  /// The cursor to read the previous page before, if there is one.
  pub prev: Option<PageCursor>,
}

impl<T> Paged<T> {
  /// The same page with `f` applied to every row.
  #[must_use]
  pub fn map<U>(self, f: impl FnMut(T) -> U) -> Paged<U> {
    let items = self.items.into_iter().map(f).collect();
    Paged {
      items,
      next: self.next,
      prev: self.prev,
    }
  }

  /// The same cursors with `items` instead of the rows, e.g. the rows with their relations loaded
  /// by [`BoundEagerRepository::load`](super::BoundEagerRepository::load).
  #[must_use]
  pub fn with_items<U>(self, items: Vec<U>) -> Paged<U> {
    Paged {
      items,
      next: self.next,
      prev: self.prev,
    }
  }

  /// Whether the page has no rows.
  #[inline]
  pub fn is_empty(&self) -> bool {
    self.items.is_empty()
  }
}

// ---- the cursor ----

/// Why a cursor token was refused.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum PageCursorError {
  /// The token (or the cursor being issued) is longer than [`MAX_CURSOR_BYTES`].
  TooLong {
    /// Its length in bytes.
    len: usize,
  },
  /// The token is not a cursor: not base64url, not the JSON document, or it fails the cursor
  /// `Schema` (version, fingerprint, 1 to [`MAX_SORT_KEYS`] typed keys).
  Malformed(String),
  /// The cursor was issued for another filter, sort or entity than the request's.
  FilterMismatch,
}

impl fmt::Display for PageCursorError {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    match self {
      Self::TooLong { len } => write!(
        f,
        "page cursor is {len} bytes, over the {MAX_CURSOR_BYTES}-byte limit"
      ),
      Self::Malformed(why) => write!(f, "malformed page cursor: {why}"),
      Self::FilterMismatch => f.write_str(
        "page cursor was issued for another filter or sort than this request (fingerprint \
         mismatch)",
      ),
    }
  }
}

impl std::error::Error for PageCursorError {}

impl From<PageCursorError> for SqlError {
  /// [`SqlError::Decode`] with the reason (the repository's page reads fail with it).
  fn from(e: PageCursorError) -> Self {
    SqlError::Decode(e.to_string())
  }
}

/// A position in a keyset order: the sort-key tuple of one row and the fingerprint of the request
/// it was issued for (D11). [`encode`](Self::encode) is the opaque token clients carry;
/// [`decode`](Self::decode) reads one back (bounded, Schema-checked).
///
/// Cursors are not signed: a client can forge any position, and the page read from it is still
/// filtered by the repository's tenant and global filters (see the [module docs](self)).
#[derive(Clone, Debug, PartialEq)]
pub struct PageCursor {
  fingerprint: u64,
  keys: Vec<SqlValue>,
}

impl PageCursor {
  /// The cursor at `keys` (one value per sort column, the primary key included) for the request
  /// with `fingerprint`, checked as [`decode`](Self::decode) checks a token: 1 to
  /// [`MAX_SORT_KEYS`] keys of a sortable type (not NULL, not JSON, finite floats) and an encoding
  /// of at most [`MAX_CURSOR_BYTES`].
  pub fn new(fingerprint: u64, keys: Vec<SqlValue>) -> Result<Self, PageCursorError> {
    if keys.is_empty() || keys.len() > MAX_SORT_KEYS {
      return Err(PageCursorError::Malformed(format!(
        "a cursor has 1 to {MAX_SORT_KEYS} keys, got {}",
        keys.len()
      )));
    }
    for key in &keys {
      key_to_json(key)?;
    }
    let cursor = Self { fingerprint, keys };
    let len = cursor.encode().len();
    if len > MAX_CURSOR_BYTES {
      return Err(PageCursorError::TooLong { len });
    }
    Ok(cursor)
  }

  /// Read the cursor token `token` (from a client: untrusted). A token over
  /// [`MAX_CURSOR_BYTES`] is refused before it is decoded; the rest goes through base64url, JSON
  /// and the cursor `Schema`. Whether the cursor fits a request is checked when a page is read
  /// with it.
  pub fn decode(token: &str) -> Result<Self, PageCursorError> {
    if token.len() > MAX_CURSOR_BYTES {
      return Err(PageCursorError::TooLong { len: token.len() });
    }
    let bytes = base64url_decode(token)
      .ok_or_else(|| PageCursorError::Malformed("not base64url".to_owned()))?;
    let json: serde_json::Value = serde_json::from_slice(&bytes)
      .map_err(|e| PageCursorError::Malformed(format!("not JSON: {e}")))?;
    let (_, fingerprint, keys) = CURSOR_SCHEMA
      .decode_unknown(&unknown(&json))
      .map_err(|e| PageCursorError::Malformed(format!("{}: {}", e.path, e.message)))?;
    // Sixteen hex digits (the Schema checked them) always fit a u64.
    let fingerprint = u64::from_str_radix(&fingerprint, 16).unwrap_or_default();
    Ok(Self { fingerprint, keys })
  }

  /// The opaque token: base64url (no padding) of `{"f": fingerprint, "k": keys, "v": 1}`.
  #[must_use]
  pub fn encode(&self) -> String {
    let keys: Vec<serde_json::Value> = self
      .keys
      .iter()
      .map(|k| key_to_json(k).unwrap_or(serde_json::Value::Null))
      .collect();
    let doc = json!({
      "v": CURSOR_VERSION,
      "f": format!("{:016x}", self.fingerprint),
      "k": keys,
    });
    base64url_encode(doc.to_string().as_bytes())
  }

  /// The fingerprint of the request the cursor was issued for.
  #[inline]
  pub fn fingerprint(&self) -> u64 {
    self.fingerprint
  }

  /// The sort-key tuple: one value per sort column, the primary key last unless it was sorted on.
  #[inline]
  pub fn keys(&self) -> &[SqlValue] {
    &self.keys
  }
}

impl fmt::Display for PageCursor {
  /// The token ([`encode`](Self::encode)).
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.write_str(&self.encode())
  }
}

impl FromStr for PageCursor {
  type Err = PageCursorError;

  /// [`PageCursor::decode`].
  fn from_str(s: &str) -> Result<Self, Self::Err> {
    Self::decode(s)
  }
}

/// The cursor document: `{"v": 1, "f": "<16 hex digits>", "k": [[tag, value], …]}` with 1 to
/// [`MAX_SORT_KEYS`] keys.
type CursorSchema = Schema<(i64, String, Vec<SqlValue>), (i64, String, Vec<Unknown>)>;

static CURSOR_SCHEMA: LazyLock<CursorSchema> = LazyLock::new(|| {
  let version = filter(
    id_effect::schema::i64::<()>(),
    |v| *v == CURSOR_VERSION,
    "unsupported cursor version",
  );
  let fingerprint = filter(
    id_effect::schema::string::<()>(),
    |f| f.len() == 16 && f.bytes().all(|b| b.is_ascii_hexdigit()),
    "expected 16 hex digits",
  );
  let keys = max_items(min_items(array(key_schema()), 1), MAX_SORT_KEYS);
  struct3("v", version, "f", fingerprint, "k", keys)
});

/// One sort key on the wire: `[tag, value]`.
fn key_schema() -> Schema<SqlValue, Unknown> {
  Schema::make(
    |u: Unknown| key_from_unknown(&u),
    |v: SqlValue| key_to_json(&v).map_or(Unknown::Null, |j| unknown(&j)),
    key_from_unknown,
  )
}

fn bad(message: impl Into<String>) -> ParseError {
  ParseError::new("", message)
}

/// `r`, or the parse error naming the key `tag`.
fn parsed<A, E: fmt::Display>(
  tag: &str,
  r: Result<A, E>,
  into: fn(A) -> SqlValue,
) -> Result<SqlValue, ParseError> {
  r.map(into)
    .map_err(|e| bad(format!("invalid `{tag}` key: {e}")))
}

/// A key value from its `[tag, value]` pair.
fn key_from_unknown(u: &Unknown) -> Result<SqlValue, ParseError> {
  let Unknown::Array(pair) = u else {
    return Err(bad("expected a [tag, value] pair"));
  };
  let [Unknown::String(tag), value] = pair.as_slice() else {
    return Err(bad("expected a [tag, value] pair"));
  };
  let tag = tag.as_str();
  let int = || match value {
    Unknown::I64(n) => Ok(*n),
    _ => Err(bad(format!("expected an integer for `{tag}`"))),
  };
  let text = || match value {
    Unknown::String(s) => Ok(s.as_str()),
    _ => Err(bad(format!("expected a string for `{tag}`"))),
  };
  match tag {
    "b" => match value {
      Unknown::Bool(b) => Ok(SqlValue::Bool(*b)),
      _ => Err(bad("expected a boolean for `b`")),
    },
    "i16" => parsed(tag, i16::try_from(int()?), SqlValue::I16),
    "i32" => parsed(tag, i32::try_from(int()?), SqlValue::I32),
    "i64" => Ok(SqlValue::I64(int()?)),
    "f32" => parsed(
      tag,
      finite(text()?.parse::<f32>(), f32::is_finite),
      SqlValue::F32,
    ),
    "f64" => parsed(
      tag,
      finite(text()?.parse::<f64>(), f64::is_finite),
      SqlValue::F64,
    ),
    "s" => Ok(SqlValue::Text(text()?.to_owned())),
    "by" => parsed(
      tag,
      base64url_decode(text()?).ok_or("not base64url"),
      SqlValue::Bytes,
    ),
    "u" => parsed(tag, text()?.parse::<uuid::Uuid>(), SqlValue::Uuid),
    "ts" => parsed(tag, text()?.parse::<jiff::Timestamp>(), SqlValue::Timestamp),
    "dt" => parsed(
      tag,
      text()?.parse::<jiff::civil::DateTime>(),
      SqlValue::DateTime,
    ),
    "d" => parsed(tag, text()?.parse::<jiff::civil::Date>(), SqlValue::Date),
    "t" => parsed(tag, text()?.parse::<jiff::civil::Time>(), SqlValue::Time),
    "dec" => parsed(
      tag,
      text()?.parse::<rust_decimal::Decimal>(),
      SqlValue::Decimal,
    ),
    other => Err(bad(format!("unknown key tag `{other}`"))),
  }
}

/// A parsed float, refused when not finite.
fn finite<F: Copy, E: fmt::Display>(r: Result<F, E>, ok: fn(F) -> bool) -> Result<F, String> {
  match r {
    Ok(v) if ok(v) => Ok(v),
    Ok(_) => Err("not finite".to_owned()),
    Err(e) => Err(e.to_string()),
  }
}

/// A key value as its `[tag, value]` pair; NULL, JSON and non-finite floats cannot be keys.
fn key_to_json(v: &SqlValue) -> Result<serde_json::Value, PageCursorError> {
  let pair = |tag: &str, value: serde_json::Value| Ok(json!([tag, value]));
  let refused = |what: &str| {
    Err(PageCursorError::Malformed(format!(
      "a {what} value cannot be a cursor key"
    )))
  };
  match v {
    SqlValue::Bool(b) => pair("b", json!(b)),
    SqlValue::I16(n) => pair("i16", json!(n)),
    SqlValue::I32(n) => pair("i32", json!(n)),
    SqlValue::I64(n) => pair("i64", json!(n)),
    SqlValue::F32(f) if f.is_finite() => pair("f32", json!(f.to_string())),
    SqlValue::F64(f) if f.is_finite() => pair("f64", json!(f.to_string())),
    SqlValue::F32(_) | SqlValue::F64(_) => refused("non-finite float"),
    SqlValue::Text(s) => pair("s", json!(s)),
    SqlValue::Bytes(b) => pair("by", json!(base64url_encode(b))),
    SqlValue::Uuid(u) => pair("u", json!(u.to_string())),
    SqlValue::Timestamp(t) => pair("ts", json!(t.to_string())),
    SqlValue::DateTime(t) => pair("dt", json!(t.to_string())),
    SqlValue::Date(d) => pair("d", json!(d.to_string())),
    SqlValue::Time(t) => pair("t", json!(t.to_string())),
    SqlValue::Decimal(d) => pair("dec", json!(d.to_string())),
    SqlValue::Null => refused("NULL"),
    SqlValue::Json(_) => refused("JSON"),
  }
}

/// `serde_json` → [`Unknown`] (the input is bounded by [`MAX_CURSOR_BYTES`], and `serde_json`
/// limits nesting).
fn unknown(v: &serde_json::Value) -> Unknown {
  match v {
    serde_json::Value::Null => Unknown::Null,
    serde_json::Value::Bool(b) => Unknown::Bool(*b),
    serde_json::Value::Number(n) => match n.as_i64() {
      Some(i) => Unknown::I64(i),
      None => Unknown::F64(n.as_f64().unwrap_or(f64::NAN)),
    },
    serde_json::Value::String(s) => Unknown::String(s.clone()),
    serde_json::Value::Array(a) => Unknown::Array(a.iter().map(unknown).collect()),
    serde_json::Value::Object(o) => Unknown::Object(
      o.iter()
        .map(|(k, v)| (k.clone(), unknown(v)))
        .collect::<BTreeMap<_, _>>(),
    ),
  }
}

const BASE64URL: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

fn base64url_encode(bytes: &[u8]) -> String {
  let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
  for chunk in bytes.chunks(3) {
    let b = [
      chunk[0],
      chunk.get(1).copied().unwrap_or(0),
      chunk.get(2).copied().unwrap_or(0),
    ];
    let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
    for i in 0..=chunk.len() {
      out.push(char::from(BASE64URL[((n >> (18 - 6 * i)) & 63) as usize]));
    }
  }
  out
}

/// Strict base64url without padding: canonical (unused trailing bits are zero), so one byte
/// string has one token.
fn base64url_decode(s: &str) -> Option<Vec<u8>> {
  if s.len() % 4 == 1 {
    return None;
  }
  let mut out = Vec::with_capacity(s.len() / 4 * 3 + 2);
  for chunk in s.as_bytes().chunks(4) {
    let mut n = 0u32;
    for (i, &c) in chunk.iter().enumerate() {
      let sextet = BASE64URL.iter().position(|&b| b == c)? as u32;
      n |= sextet << (18 - 6 * i);
    }
    let bytes = n.to_be_bytes();
    let len = chunk.len() - 1;
    if bytes[1 + len..].iter().any(|&b| b != 0) {
      return None;
    }
    out.extend_from_slice(&bytes[1..=len]);
  }
  Some(out)
}

/// FNV-1a, 64 bits: a fingerprint that stays the same across builds and Rust versions (cursors
/// outlive deploys).
struct Fnv(u64);

impl Fnv {
  fn new() -> Self {
    Self(0xcbf2_9ce4_8422_2325)
  }

  /// `part`, then a separator, so field boundaries count.
  fn part(&mut self, part: &[u8]) -> &mut Self {
    for &b in part.iter().chain(&[0xff]) {
      self.0 ^= u64::from(b);
      self.0 = self.0.wrapping_mul(0x0000_0100_0000_01b3);
    }
    self
  }
}

// ---- the reads ----

/// Which way a page is read from its cursor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Dir {
  After,
  Before,
}

/// The rows strictly after (`Dir::After`) or before `keys` in the `sort` order: the OR of
/// `c1 = k1 AND … AND c(i-1) = k(i-1) AND ci > ki` (`<` for a descending column, flipped for
/// `Before`).
fn seek(sort: &[(&'static str, SortOrder)], keys: &[SqlValue], dir: Dir) -> Condition {
  let mut any = Condition::any();
  for (i, (&(col, order), key)) in sort.iter().zip(keys).enumerate() {
    let mut all = Condition::all();
    for (&(prev, _), key) in sort[..i].iter().zip(keys) {
      all = all.add(Expr::col(prev).eq(Value::from(key.clone())));
    }
    let key = Value::from(key.clone());
    let forward = (order == SortOrder::Asc) == (dir == Dir::After);
    all = all.add(if forward {
      Expr::col(col).gt(key)
    } else {
      Expr::col(col).lt(key)
    });
    any = any.add(all);
  }
  any
}

/// The cursor at `entity` in the `sort` order.
fn cursor_of<T: Entity>(
  entity: &T,
  sort: &[(&'static str, SortOrder)],
  fingerprint: u64,
) -> Result<PageCursor, SqlError> {
  let values = entity.values();
  let mut keys = Vec::with_capacity(sort.len());
  for (col, _) in sort {
    let value = values
      .iter()
      .find(|(c, _)| c == col)
      .map(|(_, v)| v.clone())
      .ok_or_else(|| {
        SqlError::Unsupported(format!(
          "entity `{}`: sort column `{col}` is not one of its field values",
          T::NAME
        ))
      })?;
    if value == SqlValue::Null {
      return Err(SqlError::Unsupported(format!(
        "entity `{}`: a row's sort column `{col}` is NULL, so it cannot be a cursor position \
         (sort by NOT NULL columns)",
        T::NAME
      )));
    }
    keys.push(value);
  }
  PageCursor::new(fingerprint, keys).map_err(|e| match e {
    PageCursorError::TooLong { len } => SqlError::Unsupported(format!(
      "entity `{}`: the cursor of a row would be {len} bytes, over the {MAX_CURSOR_BYTES}-byte \
       limit (sort by shorter columns)",
      T::NAME
    )),
    other => SqlError::Unsupported(format!("entity `{}`: {other}", T::NAME)),
  })
}

impl<T: Entity> Repository<T> {
  /// The fingerprint of `keyset` read by this repository: the table, the filter mode, the sort
  /// and the filter (rendered, with its parameters).
  fn keyset_fingerprint(
    &self,
    keyset: &Keyset,
    sort: &[(&'static str, SortOrder)],
  ) -> Result<u64, SqlError> {
    let mut probe = Query::select();
    probe.column(T::ID).from(T::TABLE);
    if !keyset.filter.is_empty() {
      probe.cond_where(keyset.filter.clone());
    }
    // A fixed dialect: the fingerprint is the request's, not the database's.
    let (sql, params) = probe.build_sql(Dialect::Postgres)?;
    let mode = match self.filters {
      Filters::All => "all",
      Filters::WithDeleted => "with_deleted",
      Filters::None => "unscoped",
    };
    let mut h = Fnv::new();
    h.part(b"id_effect.keyset/1")
      .part(T::TABLE.as_bytes())
      .part(mode.as_bytes());
    for (col, order) in sort {
      h.part(col.as_bytes()).part(order.tag().as_bytes());
    }
    h.part(sql.as_bytes());
    for p in &params {
      h.part(format!("{p:?}").as_bytes());
    }
    Ok(h.0)
  }

  /// The page of `keyset` after / before `cursor` (from the start / end without one).
  fn keyset_op(
    &self,
    keyset: &Keyset,
    cursor: Option<PageCursor>,
    dir: Dir,
  ) -> Result<Run<Paged<T>>, SqlError> {
    let sort = keyset.resolve::<T>()?;
    let fingerprint = self.keyset_fingerprint(keyset, &sort)?;
    let mut cond = keyset.filter.clone();
    let from_cursor = cursor.is_some();
    if let Some(cursor) = cursor {
      if cursor.fingerprint != fingerprint {
        return Err(PageCursorError::FilterMismatch.into());
      }
      if cursor.keys.len() != sort.len() {
        return Err(
          PageCursorError::Malformed(format!(
            "the cursor has {} keys, the sort has {} columns",
            cursor.keys.len(),
            sort.len()
          ))
          .into(),
        );
      }
      cond = and(cond, seek(&sort, &cursor.keys, dir));
    }
    let size = keyset.page_size() as usize;
    // The tenant, soft-delete and custom filters are AND-ed in here, whatever the cursor says.
    let mut stmt = self.select(cond)?;
    for &(col, order) in &sort {
      let order = if dir == Dir::Before {
        order.reversed()
      } else {
        order
      };
      stmt.order_by(col, order.sql());
    }
    // One row more than the page tells whether another page lies that way.
    stmt.limit(size as u64 + 1);
    Ok(rows(self.render(&stmt)?, move |rows| {
      let mut items: Vec<T> = from_rows(&rows)?;
      let more = items.len() > size;
      items.truncate(size);
      if dir == Dir::Before {
        items.reverse();
      }
      let (ahead, behind) = match dir {
        Dir::After => (more, from_cursor),
        Dir::Before => (from_cursor, more),
      };
      let at = |row: Option<&T>, wanted: bool| match row {
        Some(row) if wanted => cursor_of(row, &sort, fingerprint).map(Some),
        _ => Ok(None),
      };
      let next = at(items.last(), ahead)?;
      let prev = at(items.first(), behind)?;
      Ok(Paged { items, next, prev })
    }))
  }

  /// One page of `keyset` after `cursor` — the first page without one — on the
  /// [`SqlClientService`] in `R`. The tenant and global filters apply as to every read; a cursor
  /// issued for another filter or sort fails with [`SqlError::Decode`]
  /// ([`PageCursorError::FilterMismatch`]). See the [module docs](self).
  pub fn find_after<R, I>(
    &self,
    keyset: &Keyset,
    cursor: Option<PageCursor>,
  ) -> Effect<Paged<T>, SqlError, R>
  where
    R: Selector<SqlClientService, I> + HList,
  {
    Self::in_env(self.keyset_op(keyset, cursor, Dir::After))
  }

  /// One page of `keyset` before `cursor` — the last page without one — in the keyset order, on
  /// the [`SqlClientService`] in `R`. As [`find_after`](Self::find_after) otherwise.
  pub fn find_before<R, I>(
    &self,
    keyset: &Keyset,
    cursor: Option<PageCursor>,
  ) -> Effect<Paged<T>, SqlError, R>
  where
    R: Selector<SqlClientService, I> + HList,
  {
    Self::in_env(self.keyset_op(keyset, cursor, Dir::Before))
  }
}

impl<T: Entity> BoundRepository<T> {
  /// [`Repository::find_after`] on this executor.
  pub fn find_after(
    &self,
    keyset: &Keyset,
    cursor: Option<PageCursor>,
  ) -> Effect<Paged<T>, SqlError> {
    self.run(self.repo.keyset_op(keyset, cursor, Dir::After))
  }

  /// [`Repository::find_before`] on this executor.
  pub fn find_before(
    &self,
    keyset: &Keyset,
    cursor: Option<PageCursor>,
  ) -> Effect<Paged<T>, SqlError> {
    self.run(self.repo.keyset_op(keyset, cursor, Dir::Before))
  }
}

impl<T: Entity> EagerRepository<T> {
  fn eager_page<R, I>(
    &self,
    keyset: &Keyset,
    cursor: Option<PageCursor>,
    dir: Dir,
  ) -> Effect<Paged<Loaded<T>>, SqlError, R>
  where
    R: Selector<SqlClientService, I> + HList,
  {
    let eager = self.clone();
    Repository::<T>::in_env::<_, R, I>(self.repository().keyset_op(keyset, cursor, dir)).flat_map(
      move |page: Paged<T>| {
        let Paged { items, next, prev } = page;
        eager
          .load::<R, I>(items)
          .map(move |items| Paged { items, next, prev })
      },
    )
  }

  /// [`Repository::find_after`], then the relations of the page's rows (one query per relation
  /// per level). On an executor: [`BoundRepository::find_after`], then
  /// [`BoundEagerRepository::load`](super::BoundEagerRepository::load) the page's rows and
  /// [`Paged::with_items`].
  pub fn find_after<R, I>(
    &self,
    keyset: &Keyset,
    cursor: Option<PageCursor>,
  ) -> Effect<Paged<Loaded<T>>, SqlError, R>
  where
    R: Selector<SqlClientService, I> + HList,
  {
    self.eager_page(keyset, cursor, Dir::After)
  }

  /// [`Repository::find_before`], then the relations of the page's rows.
  pub fn find_before<R, I>(
    &self,
    keyset: &Keyset,
    cursor: Option<PageCursor>,
  ) -> Effect<Paged<Loaded<T>>, SqlError, R>
  where
    R: Selector<SqlClientService, I> + HList,
  {
    self.eager_page(keyset, cursor, Dir::Before)
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  fn round_trip(v: SqlValue) {
    let c = PageCursor::new(42, vec![v.clone(), SqlValue::I64(1)]).expect("cursor");
    let back = PageCursor::decode(&c.encode()).expect("decode");
    assert_eq!(back.keys()[0], v);
    assert_eq!(back, c);
    assert_eq!(back.fingerprint(), 42);
    assert_eq!(c.to_string().parse::<PageCursor>(), Ok(c));
  }

  #[test]
  fn every_sortable_type_round_trips() {
    round_trip(SqlValue::Bool(true));
    round_trip(SqlValue::I16(-7));
    round_trip(SqlValue::I32(1 << 20));
    round_trip(SqlValue::I64(i64::MIN));
    round_trip(SqlValue::F32(1.5));
    round_trip(SqlValue::F64(-0.1));
    round_trip(SqlValue::Text("ünïcode \"quoted\"".into()));
    round_trip(SqlValue::Bytes(vec![0, 1, 2, 250, 251, 255, 7]));
    round_trip(SqlValue::Uuid(uuid::Uuid::from_u128(7)));
    round_trip(SqlValue::Timestamp(
      "2024-05-06T07:08:09.123Z".parse().expect("ts"),
    ));
    round_trip(SqlValue::DateTime(
      "2024-05-06T07:08:09".parse().expect("dt"),
    ));
    round_trip(SqlValue::Date("2024-05-06".parse().expect("d")));
    round_trip(SqlValue::Time("07:08:09.5".parse().expect("t")));
    round_trip(SqlValue::Decimal("12.3400".parse().expect("dec")));
  }

  #[test]
  fn unsortable_keys_are_refused() {
    for v in [
      SqlValue::Null,
      SqlValue::Json(json!({})),
      SqlValue::F64(f64::NAN),
      SqlValue::F32(f32::INFINITY),
    ] {
      assert!(matches!(
        PageCursor::new(1, vec![v]),
        Err(PageCursorError::Malformed(_))
      ));
    }
    assert!(matches!(
      PageCursor::new(1, vec![]),
      Err(PageCursorError::Malformed(_))
    ));
    assert!(matches!(
      PageCursor::new(1, vec![SqlValue::I64(1); MAX_SORT_KEYS + 1]),
      Err(PageCursorError::Malformed(_))
    ));
    assert!(matches!(
      PageCursor::new(1, vec![SqlValue::Text("x".repeat(MAX_CURSOR_BYTES))]),
      Err(PageCursorError::TooLong { .. })
    ));
    // The key schema's own encode / decode (the cursor uses decode_unknown).
    assert_eq!(key_schema().encode(SqlValue::Null), Unknown::Null);
    assert_eq!(
      key_schema().encode(SqlValue::I64(3)),
      Unknown::Array(vec![Unknown::String("i64".into()), Unknown::I64(3)])
    );
    assert_eq!(
      key_schema()
        .decode(Unknown::Array(vec![
          Unknown::String("i64".into()),
          Unknown::I64(3)
        ]))
        .map_err(|e| e.message),
      Ok(SqlValue::I64(3))
    );
  }

  fn token(doc: &serde_json::Value) -> String {
    base64url_encode(doc.to_string().as_bytes())
  }

  fn malformed(token: &str) -> String {
    match PageCursor::decode(token) {
      Err(PageCursorError::Malformed(why)) => why,
      other => panic!("expected malformed, got {other:?}"),
    }
  }

  #[test]
  fn decode_is_bounded_and_schema_checked() {
    let long = "A".repeat(MAX_CURSOR_BYTES + 1);
    assert_eq!(
      PageCursor::decode(&long),
      Err(PageCursorError::TooLong {
        len: MAX_CURSOR_BYTES + 1
      })
    );
    assert!(malformed("a").contains("base64url"));
    assert!(malformed("a*bc").contains("base64url"));
    assert!(
      malformed("ab").contains("base64url"),
      "non-canonical trailing bits"
    );
    assert!(malformed(&base64url_encode(b"{nope")).contains("JSON"));
    let ok = json!({"v": 1, "f": "000000000000002a", "k": [["i64", 1]]});
    assert_eq!(
      PageCursor::decode(&token(&ok)).map(|c| c.fingerprint()),
      Ok(42)
    );
    let extra =
      json!({"v": 1, "f": "000000000000002a", "k": [["i64", 1]], "x": {"y": [true, 1.5, null]}});
    assert!(
      PageCursor::decode(&token(&extra)).is_ok(),
      "unknown fields are ignored"
    );
    let f = "000000000000002a";
    for (doc, why) in [
      (json!({"v": 2, "f": f, "k": [["i64", 1]]}), "version"),
      (json!({"v": 1, "f": "2a", "k": [["i64", 1]]}), "hex"),
      (
        json!({"v": 1, "f": "zzzzzzzzzzzzzzzz", "k": [["i64", 1]]}),
        "hex",
      ),
      (json!({"v": 1, "f": f, "k": []}), "k"),
      (
        json!({"v": 1, "f": f, "k": vec![json!(["i64", 1]); 9]}),
        "k",
      ),
      (json!({"v": 1, "f": f}), "k"),
      (json!({"v": 1, "f": f, "k": [["x", 1]]}), "unknown key tag"),
      (json!({"v": 1, "f": f, "k": [["i64"]]}), "pair"),
      (json!({"v": 1, "f": f, "k": [[1, 1]]}), "pair"),
      (json!({"v": 1, "f": f, "k": [1]}), "pair"),
      (json!({"v": 1, "f": f, "k": [["b", 1]]}), "boolean"),
      (json!({"v": 1, "f": f, "k": [["i64", "1"]]}), "integer"),
      (json!({"v": 1, "f": f, "k": [["i64", 1.5]]}), "integer"),
      (json!({"v": 1, "f": f, "k": [["i16", 70000]]}), "i16"),
      (json!({"v": 1, "f": f, "k": [["i32", 1i64 << 40]]}), "i32"),
      (json!({"v": 1, "f": f, "k": [["s", 1]]}), "string"),
      (json!({"v": 1, "f": f, "k": [["f64", "NaN"]]}), "finite"),
      (json!({"v": 1, "f": f, "k": [["f64", "x"]]}), "f64"),
      (json!({"v": 1, "f": f, "k": [["f32", "inf"]]}), "finite"),
      (json!({"v": 1, "f": f, "k": [["by", "a"]]}), "by"),
      (json!({"v": 1, "f": f, "k": [["u", "x"]]}), "`u`"),
      (json!({"v": 1, "f": f, "k": [["ts", "x"]]}), "ts"),
      (json!({"v": 1, "f": f, "k": [["dt", "x"]]}), "dt"),
      (json!({"v": 1, "f": f, "k": [["d", "x"]]}), "`d`"),
      (json!({"v": 1, "f": f, "k": [["t", "x"]]}), "`t`"),
      (json!({"v": 1, "f": f, "k": [["dec", "x"]]}), "dec"),
      (json!({"v": 1, "f": f, "k": [["s", null]]}), "string"),
      (json!(null), ""),
      (json!(u64::MAX), ""),
    ] {
      let msg = malformed(&token(&doc));
      assert!(msg.contains(why), "{doc}: {msg}");
    }
  }

  #[test]
  fn base64url_matches_rfc4648() {
    for (plain, encoded) in [
      ("", ""),
      ("f", "Zg"),
      ("fo", "Zm8"),
      ("foo", "Zm9v"),
      ("foob", "Zm9vYg"),
      ("fooba", "Zm9vYmE"),
      ("foobar", "Zm9vYmFy"),
    ] {
      assert_eq!(base64url_encode(plain.as_bytes()), encoded);
      assert_eq!(base64url_decode(encoded).as_deref(), Some(plain.as_bytes()));
    }
    assert_eq!(base64url_encode(&[0xfb, 0xff]), "-_8");
    assert_eq!(base64url_decode("Zh"), None, "unused bits must be zero");
    assert_eq!(base64url_decode("Zm9="), None, "no padding");
  }

  #[test]
  fn errors_display_and_convert() {
    assert!(
      PageCursorError::TooLong { len: 2000 }
        .to_string()
        .contains("2000 bytes")
    );
    assert!(
      PageCursorError::FilterMismatch
        .to_string()
        .contains("fingerprint mismatch")
    );
    let e: SqlError = PageCursorError::Malformed("x".into()).into();
    assert!(matches!(e, SqlError::Decode(m) if m == "malformed page cursor: x"));
    let e: &dyn std::error::Error = &PageCursorError::FilterMismatch;
    assert!(e.source().is_none());
  }

  #[test]
  fn limits_cap_and_default() {
    let l = PageLimits::default();
    assert_eq!((l.default_size(), l.max_size()), (50, 500));
    assert_eq!(l.page_size(None), 50);
    assert_eq!(l.page_size(Some(0)), 1);
    assert_eq!(l.page_size(Some(10_000)), 500);
    assert_eq!(PageLimits::new(0, 0), PageLimits::new(1, 1));
    assert_eq!(PageLimits::new(900, 100).default_size(), 100);
    let k = Keyset::new()
      .order_by("a", SortOrder::Desc)
      .limits(PageLimits::new(10, 20));
    assert_eq!(k.page_size(), 10);
    assert_eq!(k.clone().size(30).page_size(), 20);
    assert_eq!(
      k.sort_columns().collect::<Vec<_>>(),
      [("a", SortOrder::Desc)]
    );
  }

  #[test]
  fn paged_maps_and_keeps_cursors() {
    let c = PageCursor::new(1, vec![SqlValue::I64(1)]).expect("cursor");
    let p = Paged {
      items: vec![1, 2],
      next: Some(c.clone()),
      prev: None,
    };
    assert!(!p.is_empty());
    let q = p.map(|n| n * 10);
    assert_eq!(q.items, [10, 20]);
    assert_eq!(q.next, Some(c));
    let r = q.with_items(Vec::<()>::new());
    assert!(r.is_empty() && r.next.is_some() && r.prev.is_none());
  }

  #[test]
  fn seek_expands_to_or_of_prefixes() {
    let sort = [("a", SortOrder::Desc), ("id", SortOrder::Asc)];
    let keys = [SqlValue::I64(5), SqlValue::I64(9)];
    let render = |dir| {
      Query::select()
        .column("id")
        .from("t")
        .cond_where(seek(&sort, &keys, dir))
        .build_sql(Dialect::Postgres)
        .expect("render")
        .0
    };
    assert_eq!(
      render(Dir::After),
      r#"SELECT "id" FROM "t" WHERE "a" < $1 OR ("a" = $2 AND "id" > $3)"#
    );
    assert_eq!(
      render(Dir::Before),
      r#"SELECT "id" FROM "t" WHERE "a" > $1 OR ("a" = $2 AND "id" < $3)"#
    );
  }

  /// An entity with nine columns, one of them JSON and one not a field value.
  struct Wide;

  impl crate::from_row::FromRow for Wide {
    fn from_row(_: &crate::row::SqlRow) -> Result<Self, SqlError> {
      Ok(Wide)
    }
  }

  impl Entity for Wide {
    type Id = i64;
    const NAME: &'static str = "Wide";
    const TABLE: &'static str = "wide";
    const COLUMNS: &'static [&'static str] = &["id", "a", "b", "c", "d", "e", "f", "g", "h"];
    const ID: &'static str = "id";

    fn id(&self) -> i64 {
      0
    }

    fn values(&self) -> Vec<(&'static str, SqlValue)> {
      vec![("id", SqlValue::I64(0)), ("b", SqlValue::Json(json!([])))]
    }
  }

  #[test]
  fn the_sort_is_resolved_and_bounded() {
    let nine = ["a", "b", "c", "d", "e", "f", "g", "h"]
      .iter()
      .fold(Keyset::new(), |k, c| k.order_by(*c, SortOrder::Asc));
    assert!(matches!(
      nine.resolve::<Wide>(),
      Err(SqlError::Unsupported(m)) if m.contains("at most 8 columns")
    ));
    let by_id = Keyset::new()
      .order_by("id", SortOrder::Desc)
      .order_by("a", SortOrder::Asc);
    assert_eq!(
      by_id.resolve::<Wide>().expect("sort"),
      [("id", SortOrder::Desc), ("a", SortOrder::Asc)],
      "the primary key is not appended twice"
    );
    assert!(matches!(
      cursor_of(&Wide, &[("a", SortOrder::Asc)], 1),
      Err(SqlError::Unsupported(m)) if m.contains("not one of its field values")
    ));
    assert!(matches!(
      cursor_of(&Wide, &[("b", SortOrder::Asc)], 1),
      Err(SqlError::Unsupported(m)) if m.contains("JSON")
    ));
    assert_eq!(
      cursor_of(&Wide, &[("id", SortOrder::Asc)], 1)
        .ok()
        .map(|c| c.keys().to_vec()),
      Some(vec![SqlValue::I64(0)])
    );
  }
}

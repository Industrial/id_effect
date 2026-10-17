//! Schema-validated extractors for plain Axum handlers.
//!
//! Each extractor names a [`HasSchema`] type `T` and yields its decoded `T::A`; the request part
//! goes through the same decoder as typed [`Route`](super::Route)s and every issue of the part is
//! reported at once in a [`Problem`](super::Problem) (`application/problem+json`, **422**; **400**
//! for a JSON body that is not well-formed). Axum's own rejections (body limit, form content type,
//! …) pass through unchanged; see [`RouteRejection`].
//!
//! | Extractor | Reads | `in` |
//! |---|---|---|
//! | [`ValidPath<T>`] | path parameters (struct by name, or one scalar) | `path` |
//! | [`ValidQuery<T>`] | the query string (repeated keys fill array fields) | `query` |
//! | [`ValidHeaders<T>`] | headers (struct field names, case-insensitive) | `header` |
//! | [`ValidJson<T>`] | a JSON body | `body` |
//! | [`ValidForm<T>`] | an urlencoded or multipart body (text fields only) | `body` |
//!
//! Axum runs extractors one after another and stops at the first rejection, so a handler taking
//! several of them reports the issues of the first failing part only; a typed
//! [`Route`](super::Route) reports every part's issues together.
//!
//! ```rust
//! use axum::Router;
//! use axum::routing::post;
//! use id_effect::schema::{self, HasSchema, Schema};
//! use id_effect_axum::route::{ValidJson, ValidPath};
//!
//! struct OrgId;
//!
//! impl HasSchema for OrgId {
//!   type A = i64;
//!   type I = i64;
//!   type E = ();
//!
//!   fn schema() -> Schema<i64, i64> {
//!     schema::i64()
//!   }
//! }
//!
//! struct NewAccount;
//!
//! impl HasSchema for NewAccount {
//!   type A = (String, i64);
//!   type I = (String, i64);
//!   type E = ();
//!
//!   fn schema() -> Schema<Self::A, Self::I> {
//!     schema::struct_("name", schema::string(), "age", schema::i64())
//!   }
//! }
//!
//! async fn create(
//!   ValidPath(org): ValidPath<OrgId>,
//!   ValidJson((name, age)): ValidJson<NewAccount>,
//! ) -> String {
//!   format!("{org}: {name} ({age})")
//! }
//!
//! let app: Router = Router::new().route("/orgs/{org}/accounts", post(create));
//! # let _ = app;
//! ```

use std::fmt;
use std::ops::{Deref, DerefMut};

use axum::extract::{FromRequest, FromRequestParts, Request};
use axum::http::request::Parts as RequestParts;
use axum::http::{StatusCode, Uri};
use id_effect::schema::{HasSchema, Schema, Unknown};

use super::decode::{
  RouteRejection, body_unknown, headers_unknown, issues, path_unknown, problem, query_unknown,
};
use super::problem::Location;
use super::registry::BodyKind;

/// Decode `input` with `schema`; every issue becomes part of one **422** [`Problem`](super::Problem).
fn decode<A, I, E>(
  schema: &Schema<A, I, E>,
  location: Location,
  input: &Unknown,
  uri: &Uri,
) -> Result<A, RouteRejection> {
  schema.decode_unknown_all(input).map_err(|errors| {
    problem(
      StatusCode::UNPROCESSABLE_ENTITY,
      issues(location, errors).collect(),
      uri,
    )
    .into()
  })
}

/// Read and decode the body of `req` as `kind`.
async fn body<T: HasSchema, S: Send + Sync>(
  req: Request,
  state: &S,
  kind: BodyKind,
) -> Result<T::A, RouteRejection> {
  let uri = req.uri().clone();
  let schema = T::schema();
  match body_unknown(req, state, schema.ast(), kind).await? {
    Ok(input) => decode(&schema, Location::Body, &input, &uri),
    Err(malformed) => Err(problem(StatusCode::BAD_REQUEST, vec![malformed], &uri).into()),
  }
}

macro_rules! extractor {
  ($(#[$doc:meta])* $name:ident) => {
    $(#[$doc])*
    pub struct $name<T: HasSchema>(pub T::A);

    impl<T: HasSchema> $name<T> {
      /// The decoded value.
      pub fn into_inner(self) -> T::A {
        self.0
      }
    }

    impl<T: HasSchema> Deref for $name<T> {
      type Target = T::A;

      fn deref(&self) -> &T::A {
        &self.0
      }
    }

    impl<T: HasSchema> DerefMut for $name<T> {
      fn deref_mut(&mut self) -> &mut T::A {
        &mut self.0
      }
    }

    impl<T: HasSchema> Clone for $name<T>
    where
      T::A: Clone,
    {
      fn clone(&self) -> Self {
        Self(self.0.clone())
      }
    }

    impl<T: HasSchema> fmt::Debug for $name<T>
    where
      T::A: fmt::Debug,
    {
      fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple(stringify!($name)).field(&self.0).finish()
      }
    }
  };
}

extractor! {
  /// Path parameters decoded as `T`: a struct schema keyed by parameter name, or any scalar schema
  /// when the route has exactly one parameter.
  ValidPath
}

extractor! {
  /// The query string decoded as `T` (a struct or record schema); repeated keys fill an array
  /// field, and an absent key gives an array field `[]` and a nullable field `null`.
  ValidQuery
}

extractor! {
  /// Request headers decoded as `T`: a struct schema whose field names are header names (matched
  /// case-insensitively), or any other schema over every header by lowercase name.
  ValidHeaders
}

extractor! {
  /// A JSON body decoded as `T`; a body that is not JSON answers **400**. The `Content-Type` is not
  /// checked.
  ValidJson
}

extractor! {
  /// A form body decoded as `T`, like [`ValidQuery`]: `application/x-www-form-urlencoded` (the
  /// query string for `GET`/`HEAD`), or the text fields of `multipart/form-data` (file fields are
  /// skipped). Another content type keeps Axum's `415` rejection.
  ValidForm
}

impl<T, S> FromRequestParts<S> for ValidPath<T>
where
  T: HasSchema,
  S: Send + Sync,
{
  type Rejection = RouteRejection;

  async fn from_request_parts(
    parts: &mut RequestParts,
    state: &S,
  ) -> Result<Self, Self::Rejection> {
    let schema = T::schema();
    let input = path_unknown(parts, state, schema.ast()).await?;
    decode(&schema, Location::Path, &input, &parts.uri).map(Self)
  }
}

impl<T, S> FromRequestParts<S> for ValidQuery<T>
where
  T: HasSchema,
  S: Send + Sync,
{
  type Rejection = RouteRejection;

  async fn from_request_parts(parts: &mut RequestParts, _: &S) -> Result<Self, Self::Rejection> {
    let schema = T::schema();
    let input = query_unknown(&parts.uri, schema.ast())?;
    decode(&schema, Location::Query, &input, &parts.uri).map(Self)
  }
}

impl<T, S> FromRequestParts<S> for ValidHeaders<T>
where
  T: HasSchema,
  S: Send + Sync,
{
  type Rejection = RouteRejection;

  async fn from_request_parts(parts: &mut RequestParts, _: &S) -> Result<Self, Self::Rejection> {
    let schema = T::schema();
    let input = headers_unknown(&parts.headers, schema.ast());
    decode(&schema, Location::Header, &input, &parts.uri).map(Self)
  }
}

impl<T, S> FromRequest<S> for ValidJson<T>
where
  T: HasSchema,
  S: Send + Sync,
{
  type Rejection = RouteRejection;

  async fn from_request(req: Request, state: &S) -> Result<Self, Self::Rejection> {
    body::<T, S>(req, state, BodyKind::Json).await.map(Self)
  }
}

impl<T, S> FromRequest<S> for ValidForm<T>
where
  T: HasSchema,
  S: Send + Sync,
{
  type Rejection = RouteRejection;

  async fn from_request(req: Request, state: &S) -> Result<Self, Self::Rejection> {
    body::<T, S>(req, state, BodyKind::Form).await.map(Self)
  }
}

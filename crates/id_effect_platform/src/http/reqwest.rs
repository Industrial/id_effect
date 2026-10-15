//! [`reqwest`](https://docs.rs/reqwest) helpers: `send`, JSON/schema decode, and client pools.
//!
//! For portable [`HttpClient`](crate::http::HttpClient) /
//! [`HttpRequest`](crate::http::HttpRequest) boundaries, use the parent [`crate::http`] module
//! (`execute`, `execute_stream`, [`ReqwestHttpClient`](crate::http::ReqwestHttpClient)).
//!
//! The capabilities are [`ReqwestClient`] ([`ReqwestClientLive::layer`],
//! [`provide_reqwest_client`]) and [`ReqwestPool`] ([`provide_reqwest_pool`]). Each helper reads
//! its capability from the effect's `R` (`R: Selector<ReqwestClient, I>`, the index `I` is
//! inferred).

use std::sync::Arc;
use std::time::Duration;

use ::id_effect::next::{
  Effect, HCons, HList, HNil, Layer, Pool, Selector, fail, from_async, hlist, need, succeed, sync,
};
use id_effect::schema::data::EffectData;
use id_effect::schema::{ParseError, Schema, Unknown};
use reqwest::{Client, Error, RequestBuilder, Response};
use serde_json::Value;

/// Injectable [`reqwest::Client`] capability.
#[derive(Clone, Debug)]
pub struct ReqwestClient(pub Client);

impl std::ops::Deref for ReqwestClient {
  type Target = Client;

  fn deref(&self) -> &Self::Target {
    &self.0
  }
}

/// The minimal typed environment holding only [`ReqwestClient`].
pub type ReqwestClientEnv = HCons<ReqwestClient, HNil>;

/// Wraps [`Client`] in an [`Arc`] so it can live in [`Pool`] ([`PartialEq`] uses pointer identity).
#[derive(Clone, Debug)]
pub struct PooledClient(Arc<Client>);

impl PooledClient {
  /// A pooled handle to `client`.
  #[inline]
  pub fn new(client: Client) -> Self {
    Self(Arc::new(client))
  }

  /// Allocation identity of the inner [`Client`] (for tests / pooling assertions).
  #[inline]
  pub fn allocation_ptr(&self) -> *const Client {
    Arc::as_ptr(&self.0)
  }
}

impl std::ops::Deref for PooledClient {
  type Target = Client;

  fn deref(&self) -> &Self::Target {
    &self.0
  }
}

impl PartialEq for PooledClient {
  fn eq(&self, other: &Self) -> bool {
    Arc::ptr_eq(&self.0, &other.0)
  }
}

impl Eq for PooledClient {}

/// Injectable [`Pool`] of [`PooledClient`] capability.
pub type ReqwestPool = Pool<PooledClient>;

/// The minimal typed environment holding only [`ReqwestPool`].
pub type ReqwestPoolEnv = HCons<ReqwestPool, HNil>;

/// Default [`ReqwestClient`] provider ([`reqwest::Client::new`]).
#[derive(Clone, Copy, Debug, Default)]
pub struct ReqwestClientLive;

impl ReqwestClientLive {
  /// The layer: a new [`reqwest::Client`] built once per layer build.
  pub fn layer<E: Send + 'static>() -> Layer<ReqwestClientEnv, E> {
    Layer::effect(|| sync(|| hlist![ReqwestClient(Client::new())]))
  }
}

/// A [`Layer`] providing `client` as the [`ReqwestClient`] capability.
#[inline]
pub fn provide_reqwest_client<E: Send + 'static>(client: Client) -> Layer<ReqwestClientEnv, E> {
  Layer::succeed(hlist![ReqwestClient(client)])
}

/// A [`Layer`] providing a [`ReqwestPool`] of at most `capacity` checked-out clients; idle
/// clients older than `ttl` are replaced. Each build gets a new pool.
#[inline]
pub fn provide_reqwest_pool<E: Send + 'static>(
  capacity: usize,
  ttl: Duration,
) -> Layer<ReqwestPoolEnv, E> {
  Layer::effect(move || {
    sync(move || {
      hlist![Pool::new(capacity, || succeed(PooledClient::new(Client::new()))).with_ttl(ttl)]
    })
  })
}

/// [`send`] with a client checked out from [`ReqwestPool`]; it returns to the pool when the
/// request completes (however it ends).
#[inline]
pub fn send_pooled<A, E, R, I>(
  build: impl FnOnce(&Client) -> RequestBuilder + Send + 'static,
) -> Effect<A, E, R>
where
  A: From<Response> + Send + 'static,
  E: From<Error> + Send + 'static,
  R: Selector<ReqwestPool, I> + HList,
{
  need::<ReqwestPool>().flat_map(move |pool| {
    Effect::scoped(move |scope| {
      pool
        .get(&scope)
        .widen::<E>()
        .narrow::<R, _>()
        .flat_map(move |pooled| {
          from_async(
            move || async move { build(&pooled).send().await.map(A::from).map_err(E::from) },
          )
        })
    })
  })
}

/// Failure from [`json_schema`].
#[derive(Debug)]
pub enum JsonSchemaError {
  /// HTTP transport or status failure from the underlying [`reqwest`] client.
  Http(Error),
  /// Invalid JSON (syntax); see message.
  Json(String),
  /// Response body did not match the expected [`Schema`](struct@id_effect::schema::Schema).
  Schema(ParseError),
}

fn unknown_from_json_value(value: Value) -> Unknown {
  id_effect::schema::serde_bridge::unknown_from_serde_json(value)
}

fn decode_response_schema<A, I, Es>(
  schema: &Schema<A, I, Es>,
  bytes: &[u8],
) -> Result<A, JsonSchemaError>
where
  Es: EffectData + 'static,
  A: 'static,
  I: 'static,
{
  let v: Value = serde_json::from_slice(bytes).map_err(|e| JsonSchemaError::Json(e.to_string()))?;
  let u = unknown_from_json_value(v);
  schema.decode_unknown(&u).map_err(JsonSchemaError::Schema)
}

/// The response of `build(client)` from the [`ReqwestClient`] in `R`, then `then(resp)`.
fn with_response<B, E, R, Ix, Fut>(
  build: impl FnOnce(&Client) -> RequestBuilder + Send + 'static,
  send_err: impl FnOnce(Error) -> E + Send + 'static,
  then: impl FnOnce(Response) -> Fut + Send + 'static,
) -> Effect<B, E, R>
where
  B: Send + 'static,
  E: Send + 'static,
  R: Selector<ReqwestClient, Ix> + HList,
  Fut: std::future::Future<Output = Result<B, E>> + Send + 'static,
{
  need::<ReqwestClient>().flat_map(move |client| {
    from_async(move || async move {
      let resp = build(&client.0).send().await.map_err(send_err)?;
      then(resp).await
    })
  })
}

/// [`send`] then decode the response body as JSON through `schema` ([`Schema::decode_unknown`]).
#[inline]
pub fn json_schema<R, Ix, A, I, Es>(
  schema: Arc<Schema<A, I, Es>>,
  build: impl FnOnce(&Client) -> RequestBuilder + Send + 'static,
) -> Effect<A, JsonSchemaError, R>
where
  R: Selector<ReqwestClient, Ix> + HList,
  Es: EffectData + Send + Sync + 'static,
  A: Send + 'static,
  I: Send + Sync + 'static,
{
  let buffered = with_response::<_, JsonSchemaError, R, Ix, _>(
    build,
    JsonSchemaError::Http,
    |resp| async move { resp.bytes().await.map_err(JsonSchemaError::Http) },
  );
  buffered.flat_map(move |buf| match decode_response_schema(&schema, &buf) {
    Ok(v) => succeed(v),
    Err(e) => fail(e),
  })
}

/// Run [`RequestBuilder::send`](reqwest::RequestBuilder::send) using the client from `R`.
#[inline]
pub fn send<A, E, R, I>(
  build: impl FnOnce(&Client) -> RequestBuilder + Send + 'static,
) -> Effect<A, E, R>
where
  A: From<Response> + Send + 'static,
  E: From<Error> + Send + 'static,
  R: Selector<ReqwestClient, I> + HList,
{
  with_response(build, E::from, |resp| async move { Ok(A::from(resp)) })
}

/// [`send`] then [`Response::text`](reqwest::Response::text).
#[inline]
pub fn text<A, E, R, I>(
  build: impl FnOnce(&Client) -> RequestBuilder + Send + 'static,
) -> Effect<A, E, R>
where
  A: From<String> + Send + 'static,
  E: From<Error> + Send + 'static,
  R: Selector<ReqwestClient, I> + HList,
{
  with_response(build, E::from, |resp| async move {
    resp.text().await.map(A::from).map_err(E::from)
  })
}

/// [`send`] then [`Response::bytes`](reqwest::Response::bytes).
#[inline]
pub fn bytes<A, E, R, I>(
  build: impl FnOnce(&Client) -> RequestBuilder + Send + 'static,
) -> Effect<A, E, R>
where
  A: From<bytes::Bytes> + Send + 'static,
  E: From<Error> + Send + 'static,
  R: Selector<ReqwestClient, I> + HList,
{
  with_response(build, E::from, |resp| async move {
    resp.bytes().await.map(A::from).map_err(E::from)
  })
}

/// [`send`] then [`Response::json`](reqwest::Response::json).
#[inline]
pub fn json<A, E, R, I, T>(
  build: impl FnOnce(&Client) -> RequestBuilder + Send + 'static,
) -> Effect<A, E, R>
where
  A: From<T> + Send + 'static,
  E: From<Error> + Send + 'static,
  R: Selector<ReqwestClient, I> + HList,
  T: serde::de::DeserializeOwned + 'static,
{
  with_response(build, E::from, |resp| async move {
    resp.json::<T>().await.map(A::from).map_err(E::from)
  })
}

macro_rules! verb_helper {
  ($(#[$doc:meta])* $name:ident) => {
    $(#[$doc])*
    #[inline]
    pub fn $name<A, E, R, I>(url: String) -> Effect<A, E, R>
    where
      A: From<Response> + Send + 'static,
      E: From<Error> + Send + 'static,
      R: Selector<ReqwestClient, I> + HList,
    {
      send(move |c| c.$name(url))
    }
  };
}

verb_helper!(
  /// Shorthand for [`send`]`(|c| c.get(url))`.
  get
);
verb_helper!(
  /// Shorthand for [`send`]`(|c| c.post(url))`.
  post
);
verb_helper!(
  /// Shorthand for [`send`]`(|c| c.put(url))`.
  put
);
verb_helper!(
  /// Shorthand for [`send`]`(|c| c.delete(url))`.
  delete
);
verb_helper!(
  /// Shorthand for [`send`]`(|c| c.head(url))`.
  head
);
verb_helper!(
  /// Shorthand for [`send`]`(|c| c.patch(url))`.
  patch
);

#[cfg(test)]
mod tests {
  use super::*;
  use id_effect::schema;
  use id_effect::{Cause, Exit};
  use id_effect_tokio::TokioRuntime;
  use serde::{Deserialize, Serialize};
  use wiremock::matchers::{method, path};
  use wiremock::{Mock, MockServer, ResponseTemplate};

  /// Runs `eff` on the ambient tokio runtime (reqwest needs its IO driver).
  async fn run<A: Send + 'static, E: Send + std::fmt::Debug + 'static>(
    eff: Effect<A, E>,
  ) -> Result<A, E> {
    match TokioRuntime::current().expect("tokio").run(eff).await {
      Exit::Success(a) => Ok(a),
      Exit::Failure(Cause::Fail(e)) => Err(e),
      Exit::Failure(c) => panic!("unexpected cause: {c:?}"),
    }
  }

  fn client_env() -> ReqwestClientEnv {
    hlist![ReqwestClient(Client::new())]
  }

  async fn mock(verb: &str, route: &str, resp: ResponseTemplate) -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method(verb))
      .and(path(route))
      .respond_with(resp)
      .mount(&server)
      .await;
    server
  }

  #[tokio::test]
  async fn text_roundtrip() {
    let server = mock(
      "GET",
      "/ping",
      ResponseTemplate::new(200).set_body_string("pong"),
    )
    .await;
    let url = format!("{}/ping", server.uri());
    let body = run(text::<String, Error, _, _>(move |c| c.get(url)).provide_all(client_env()))
      .await
      .unwrap();
    assert_eq!(body, "pong");
  }

  #[derive(Debug, Deserialize, Serialize, PartialEq)]
  struct Msg {
    n: i32,
  }

  #[tokio::test]
  async fn json_roundtrip() {
    let server = mock(
      "GET",
      "/data",
      ResponseTemplate::new(200).set_body_json(&Msg { n: 7 }),
    )
    .await;
    let url = format!("{}/data", server.uri());
    let msg = run(json::<Msg, Error, _, _, Msg>(move |c| c.get(url)).provide_all(client_env()))
      .await
      .unwrap();
    assert_eq!(msg, Msg { n: 7 });
  }

  #[tokio::test]
  async fn bytes_roundtrip() {
    let server = mock(
      "GET",
      "/data",
      ResponseTemplate::new(200).set_body_bytes(b"hello"),
    )
    .await;
    let url = format!("{}/data", server.uri());
    let body =
      run(bytes::<bytes::Bytes, Error, _, _>(move |c| c.get(url)).provide_all(client_env()))
        .await
        .unwrap();
    assert_eq!(body.as_ref(), b"hello");
  }

  #[tokio::test]
  async fn send_error_maps_through_from() {
    let err = run(
      send::<Response, Error, _, _>(|c| c.get("http://127.0.0.1:1/refused"))
        .provide_all(client_env()),
    )
    .await
    .expect_err("refused");
    assert!(err.is_connect() || err.is_request(), "{err:?}");
  }

  #[tokio::test]
  async fn verb_helpers_send_their_method() {
    let server = MockServer::start().await;
    for verb in ["GET", "POST", "PUT", "DELETE", "HEAD", "PATCH"] {
      Mock::given(method(verb))
        .and(path("/item"))
        .respond_with(ResponseTemplate::new(204))
        .mount(&server)
        .await;
    }
    let url = format!("{}/item", server.uri());
    type Help = fn(String) -> Effect<Response, Error, ReqwestClientEnv>;
    let helpers: [Help; 6] = [get, post, put, delete, head, patch];
    for helper in helpers {
      let resp = run(helper(url.clone()).provide_all(client_env()))
        .await
        .unwrap();
      assert_eq!(resp.status().as_u16(), 204);
    }
  }

  #[tokio::test]
  async fn reqwest_client_layers_supply_the_capability() {
    let server = mock(
      "GET",
      "/layer",
      ResponseTemplate::new(200).set_body_string("ok"),
    )
    .await;
    let url = format!("{}/layer", server.uri());
    let u2 = url.clone();
    let live = text::<String, Error, ReqwestClientEnv, _>(move |c| c.get(url))
      .provide_layer(ReqwestClientLive::layer());
    assert_eq!(run(live).await.unwrap(), "ok");
    let client = Client::builder()
      .timeout(Duration::from_secs(30))
      .build()
      .unwrap();
    let given = text::<String, Error, ReqwestClientEnv, _>(move |c| c.get(u2))
      .provide_layer(provide_reqwest_client(client));
    assert_eq!(run(given).await.unwrap(), "ok");
  }

  #[tokio::test]
  async fn send_pooled_reuses_one_client() {
    let server = mock(
      "GET",
      "/pooled",
      ResponseTemplate::new(200).set_body_string("ok"),
    )
    .await;
    let url = format!("{}/pooled", server.uri());
    let env = match run(provide_reqwest_pool::<Error>(1, Duration::from_secs(60)).build()).await {
      Ok(env) => env,
      Err(e) => panic!("pool layer: {e:?}"),
    };
    let pool: ReqwestPool = (*env.0).clone();
    for _ in 0..2 {
      let u = url.clone();
      let resp =
        run(send_pooled::<Response, Error, _, _>(move |c| c.get(u)).provide_all(env.clone()))
          .await
          .unwrap();
      assert_eq!(resp.status().as_u16(), 200);
    }
    // Checked back in after each request; one client was ever created.
    assert_eq!((pool.in_use(), pool.idle_count()), (0, 1));
  }

  #[tokio::test]
  async fn reqwest_response_schema_decode_error_has_field_path() {
    let server = mock(
      "GET",
      "/bad",
      ResponseTemplate::new(200).set_body_string(r#"{"name":"x","age":"oops"}"#),
    )
    .await;
    let url = format!("{}/bad", server.uri());
    let sch = Arc::new(schema::struct_(
      "name",
      schema::string::<()>(),
      "age",
      schema::i64::<()>(),
    ));
    let err = run(json_schema(sch, move |c| c.get(url)).provide_all(client_env()))
      .await
      .expect_err("schema");
    match err {
      JsonSchemaError::Schema(p) => assert!(p.path.contains("age"), "path={:?}", p.path),
      e => panic!("unexpected {e:?}"),
    }
  }

  #[tokio::test]
  async fn json_schema_decodes_a_valid_body() {
    let server = mock(
      "GET",
      "/n",
      ResponseTemplate::new(200).set_body_string("41"),
    )
    .await;
    let url = format!("{}/n", server.uri());
    let sch = Arc::new(schema::i64::<()>());
    let n = run(json_schema(sch, move |c| c.get(url)).provide_all(client_env()))
      .await
      .unwrap();
    assert_eq!(n, 41);
  }

  #[tokio::test]
  async fn json_schema_error_bad_json_returns_json_variant() {
    let server = mock(
      "GET",
      "/badjson",
      ResponseTemplate::new(200).set_body_string("not json at all"),
    )
    .await;
    let url = format!("{}/badjson", server.uri());
    let sch = Arc::new(schema::i64::<()>());
    let err = run(json_schema(sch, move |c| c.get(url)).provide_all(client_env()))
      .await
      .expect_err("should fail");
    assert!(matches!(err, JsonSchemaError::Json(_)));
  }

  #[tokio::test]
  async fn json_schema_transport_error_is_http_variant() {
    let sch = Arc::new(schema::i64::<()>());
    let err =
      run(json_schema(sch, |c| c.get("http://127.0.0.1:1/refused")).provide_all(client_env()))
        .await
        .expect_err("refused");
    assert!(matches!(err, JsonSchemaError::Http(_)));
  }

  #[test]
  fn json_schema_error_debug_variants() {
    let e = JsonSchemaError::Json("bad json".to_string());
    assert!(format!("{e:?}").contains("bad json"));
    let e = JsonSchemaError::Schema(id_effect::schema::ParseError::new("field", "invalid"));
    let _ = format!("{e:?}");
  }

  #[test]
  fn pooled_client_partial_eq_is_pointer_identity() {
    let arc = Arc::new(Client::new());
    let a = PooledClient(arc.clone());
    let b = PooledClient(arc);
    assert_eq!(a, b);
    assert_eq!(a.allocation_ptr(), b.allocation_ptr());
    assert_ne!(
      PooledClient::new(Client::new()),
      PooledClient::new(Client::new())
    );
    assert!(a.get("https://example.com").build().is_ok());
  }

  #[test]
  fn reqwest_client_derefs_to_the_client() {
    let c = ReqwestClient(Client::new());
    assert!(c.get("https://example.com").build().is_ok());
  }
}

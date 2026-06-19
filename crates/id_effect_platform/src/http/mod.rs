#![allow(private_bounds, clippy::new_ret_no_self, clippy::unused_unit)]

//! Portable HTTP client ([`HttpClient`]) and reqwest-backed [`ReqwestHttpClient`].

use std::sync::Arc;
use std::time::Duration;

use ::reqwest::header::{HeaderName, HeaderValue};
use bytes::Bytes;
use id_effect::kernel::Effect;
use id_effect::{
  Chunk, Env, Needs, ProviderBox, ProviderError, ProviderSpec, end_stream, provide, send_chunk,
  stream_from_channel,
};

use crate::error::HttpError;

/// HTTP verb for [`HttpRequest`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HttpMethod {
  /// GET
  Get,
  /// POST
  Post,
  /// PUT
  Put,
  /// DELETE
  Delete,
  /// PATCH
  Patch,
}

/// Minimal portable HTTP request description.
#[derive(Clone, Debug)]
pub struct HttpRequest {
  /// HTTP method.
  pub method: HttpMethod,
  /// Full URL string.
  pub url: String,
  /// `(name, value)` header pairs.
  pub headers: Vec<(String, String)>,
  /// Optional request body.
  pub body: Option<Vec<u8>>,
  /// Per-request timeout (applied on top of client defaults).
  pub timeout: Option<Duration>,
  /// Maximum response body bytes to buffer in memory (default [`HttpRequest::DEFAULT_MAX_BODY_BYTES`]).
  pub max_body_bytes: Option<usize>,
}

impl HttpRequest {
  /// Default cap for buffered response bodies (10 MiB).
  pub const DEFAULT_MAX_BODY_BYTES: usize = 10 * 1024 * 1024;

  /// GET without body.
  #[inline]
  pub fn get(url: impl Into<String>) -> Self {
    Self {
      method: HttpMethod::Get,
      url: url.into(),
      headers: Vec::new(),
      body: None,
      timeout: None,
      max_body_bytes: None,
    }
  }

  /// POST with body bytes.
  #[inline]
  pub fn post(url: impl Into<String>, body: Vec<u8>) -> Self {
    Self {
      method: HttpMethod::Post,
      url: url.into(),
      headers: Vec::new(),
      body: Some(body),
      timeout: None,
      max_body_bytes: None,
    }
  }
}

/// Buffered HTTP response (MVP — full body in memory).
#[derive(Clone, Debug)]
pub struct HttpResponse {
  /// Status code.
  pub status: u16,
  /// `(name, value)` headers (lowercased names not guaranteed).
  pub headers: Vec<(String, String)>,
  /// Response body bytes.
  pub body: Vec<u8>,
}

/// HTTP response whose body is a pull-based [`id_effect::Stream`] of byte chunks.
pub struct StreamingHttpResponse {
  /// Status code.
  pub status: u16,
  /// `(name, value)` headers.
  pub headers: Vec<(String, String)>,
  /// Chunked response body (`Chunk<u8>` items are successive byte runs).
  pub body: id_effect::Stream<u8, HttpError, ()>,
}

/// Split a buffered body into fixed-size chunks as a [`id_effect::Stream`].
#[inline]
pub fn body_as_chunk_stream(body: Vec<u8>, chunk_size: usize) -> id_effect::Stream<u8, (), ()> {
  let (stream, sender) = stream_from_channel::<u8, (), ()>(8);
  let chunk_size = chunk_size.max(1);
  std::thread::spawn(move || {
    for offset in (0..body.len()).step_by(chunk_size) {
      let end = (offset + chunk_size).min(body.len());
      let chunk = Chunk::from_vec(body[offset..end].to_vec());
      if id_effect::run_blocking(send_chunk(&sender, chunk), ()).is_err() {
        return;
      }
    }
    let _ = id_effect::run_blocking(end_stream(sender), ());
  });
  stream
}

/// Map a buffered [`HttpResponse`] to a multi-chunk body stream.
#[inline]
pub fn response_body_stream(
  resp: &HttpResponse,
  chunk_size: usize,
) -> id_effect::Stream<u8, (), ()> {
  body_as_chunk_stream(resp.body.clone(), chunk_size)
}

/// Map a buffered [`HttpResponse`] body to a single [`id_effect::Chunk`] (MVP for stream interop).
#[inline]
pub fn response_body_chunk(resp: &HttpResponse) -> id_effect::Chunk<u8> {
  id_effect::Chunk::from_vec(resp.body.clone())
}

/// Capability: execute portable HTTP requests as [`Effect`] values.
#[::id_effect::capability(Arc<dyn HttpClient>)]
pub trait HttpClient: Send + Sync + 'static {
  /// Execute `req` and return a buffered response.
  fn execute(&self, req: HttpRequest) -> Effect<HttpResponse, HttpError, ()>;

  /// Execute `req` and return a streaming body (`Chunk<u8>` chunks).
  fn execute_stream(&self, req: HttpRequest) -> Effect<StreamingHttpResponse, HttpError, ()>;
}

/// [`::reqwest::Client`]-backed [`HttpClient`].
#[derive(Clone)]
pub struct ReqwestHttpClient {
  inner: ::reqwest::Client,
}

impl ReqwestHttpClient {
  /// Wrap an existing [`::reqwest::Client`].
  #[inline]
  pub fn new(client: ::reqwest::Client) -> Self {
    Self { inner: client }
  }

  /// Build with [`::reqwest::Client::new`].
  #[inline]
  pub fn default_client() -> Self {
    Self::new(::reqwest::Client::new())
  }
}

impl HttpClient for ReqwestHttpClient {
  fn execute(&self, req: HttpRequest) -> Effect<HttpResponse, HttpError, ()> {
    let client = self.inner.clone();
    Effect::new_async(move |_r: &mut ()| {
      let req = req.clone();
      Box::pin(async move {
        let max = req
          .max_body_bytes
          .unwrap_or(HttpRequest::DEFAULT_MAX_BODY_BYTES);
        let method = match req.method {
          HttpMethod::Get => ::reqwest::Method::GET,
          HttpMethod::Post => ::reqwest::Method::POST,
          HttpMethod::Put => ::reqwest::Method::PUT,
          HttpMethod::Delete => ::reqwest::Method::DELETE,
          HttpMethod::Patch => ::reqwest::Method::PATCH,
        };
        let mut rb = client.request(method, &req.url);
        if let Some(t) = req.timeout {
          rb = rb.timeout(t);
        }
        for (k, v) in &req.headers {
          let name = HeaderName::from_bytes(k.as_bytes())
            .map_err(|e| HttpError::InvalidRequest(format!("bad header name {k:?}: {e}")))?;
          let value = HeaderValue::from_str(v)
            .map_err(|e| HttpError::InvalidRequest(format!("bad header value for {k:?}: {e}")))?;
          rb = rb.header(name, value);
        }
        if let Some(b) = &req.body {
          rb = rb.body(b.clone());
        }
        let resp = rb.send().await.map_err(HttpError::from)?;
        let status = resp.status().as_u16();
        let mut headers = Vec::new();
        for (k, v) in resp.headers().iter() {
          if let Ok(s) = v.to_str() {
            headers.push((k.as_str().to_string(), s.to_string()));
          }
        }
        let bytes: Bytes = resp.bytes().await.map_err(HttpError::from)?;
        let len = bytes.len();
        if len > max {
          return Err(HttpError::BodyTooLarge { len, max });
        }
        Ok(HttpResponse {
          status,
          headers,
          body: bytes.to_vec(),
        })
      })
    })
  }

  fn execute_stream(&self, req: HttpRequest) -> Effect<StreamingHttpResponse, HttpError, ()> {
    let client = self.inner.clone();
    Effect::new_async(move |_r: &mut ()| {
      let req = req.clone();
      Box::pin(async move {
        let max = req
          .max_body_bytes
          .unwrap_or(HttpRequest::DEFAULT_MAX_BODY_BYTES);
        let method = match req.method {
          HttpMethod::Get => ::reqwest::Method::GET,
          HttpMethod::Post => ::reqwest::Method::POST,
          HttpMethod::Put => ::reqwest::Method::PUT,
          HttpMethod::Delete => ::reqwest::Method::DELETE,
          HttpMethod::Patch => ::reqwest::Method::PATCH,
        };
        let mut rb = client.request(method, &req.url);
        if let Some(t) = req.timeout {
          rb = rb.timeout(t);
        }
        for (k, v) in &req.headers {
          let name = HeaderName::from_bytes(k.as_bytes())
            .map_err(|e| HttpError::InvalidRequest(format!("bad header name {k:?}: {e}")))?;
          let value = HeaderValue::from_str(v)
            .map_err(|e| HttpError::InvalidRequest(format!("bad header value for {k:?}: {e}")))?;
          rb = rb.header(name, value);
        }
        if let Some(b) = &req.body {
          rb = rb.body(b.clone());
        }
        let mut resp = rb.send().await.map_err(HttpError::from)?;
        let status = resp.status().as_u16();
        let mut headers = Vec::new();
        for (k, v) in resp.headers().iter() {
          if let Ok(s) = v.to_str() {
            headers.push((k.as_str().to_string(), s.to_string()));
          }
        }

        let (body, sender) = stream_from_channel::<u8, HttpError, ()>(16);
        tokio::spawn(async move {
          let mut total = 0usize;
          loop {
            let chunk_result = resp.chunk().await;
            match chunk_result {
              Ok(Some(bytes)) => {
                total += bytes.len();
                if total > max {
                  sender.fail(HttpError::BodyTooLarge { len: total, max });
                  return;
                }
                let chunk = Chunk::from_vec(bytes.to_vec());
                let send = send_chunk(&sender, chunk);
                if id_effect::run_blocking(send, ()).is_err() {
                  return;
                }
              }
              Ok(None) => {
                let _ = id_effect::run_blocking(end_stream(sender), ());
                return;
              }
              Err(e) => {
                sender.fail(HttpError::from(e));
                return;
              }
            }
          }
        });

        Ok(StreamingHttpResponse {
          status,
          headers,
          body,
        })
      })
    })
  }
}

/// Default reqwest-backed [`HttpClient`] provider (crate-internal [`ProviderSpec`]).
#[derive(::id_effect::ProviderSpecDerive)]
#[provides(HttpClientKey)]
pub(crate) struct ReqwestHttpClientProvider;

impl ReqwestHttpClientProvider {
  fn new() -> Arc<dyn HttpClient> {
    Arc::new(ReqwestHttpClient::default_client())
  }
}

/// Whether `env` includes an HTTP client capability.
#[inline]
pub fn env_has_http_client(env: &Env) -> bool {
  env.has::<HttpClientKey>()
}

/// Replace the HTTP client in `env` (tests and custom wiring).
#[inline]
pub fn env_set_http_client(env: &mut Env, client: Arc<dyn HttpClient>) {
  env.insert::<HttpClientKey>(client);
}

/// Register the default reqwest-backed [`HttpClient`] provider.
#[inline]
pub fn provide_reqwest_http_client() -> ProviderBox {
  provide!(ReqwestHttpClientProvider)
}

/// Execute using the installed [`HttpClient`] capability.
#[inline]
pub fn execute<R>(req: HttpRequest) -> Effect<HttpResponse, HttpError, R>
where
  R: Needs<HttpClientKey> + 'static,
{
  Effect::new_async(move |r: &mut R| {
    let client = r.need().clone();
    let inner = client.execute(req);
    Box::pin(async move { inner.run(&mut ()).await })
  })
}

/// Streamed execute using the installed [`HttpClient`] capability.
#[inline]
pub fn execute_stream<R>(req: HttpRequest) -> Effect<StreamingHttpResponse, HttpError, R>
where
  R: Needs<HttpClientKey> + 'static,
{
  Effect::new_async(move |r: &mut R| {
    let client = r.need().clone();
    let inner = client.execute_stream(req);
    Box::pin(async move { inner.run(&mut ()).await })
  })
}

#[cfg(test)]
mod tests {
  use super::*;

  mod http_request {
    use super::*;

    mod get {
      use super::*;

      #[test]
      fn sets_get_method_and_empty_optional_fields() {
        let r = HttpRequest::get("https://a.test/");
        assert_eq!(r.method, HttpMethod::Get);
        assert_eq!(r.url, "https://a.test/");
        assert!(r.headers.is_empty());
        assert!(r.body.is_none());
        assert!(r.timeout.is_none());
        assert!(r.max_body_bytes.is_none());
      }
    }

    mod post {
      use super::*;

      #[test]
      fn sets_post_method_and_body() {
        let body = vec![1, 2, 3];
        let r = HttpRequest::post("https://a.test/p", body.clone());
        assert_eq!(r.method, HttpMethod::Post);
        assert_eq!(r.body, Some(body));
      }
    }

    #[test]
    fn default_max_body_bytes_constant_is_ten_mib() {
      assert_eq!(HttpRequest::DEFAULT_MAX_BODY_BYTES, 10 * 1024 * 1024);
    }
  }

  mod body_as_chunk_stream {
    use super::*;

    #[test]
    fn splits_body_into_multiple_chunks() {
      let body = vec![1, 2, 3, 4, 5];
      let collected =
        id_effect::run_blocking(body_as_chunk_stream(body, 2).run_collect(), ()).unwrap();
      assert_eq!(collected, vec![1, 2, 3, 4, 5]);
    }
  }

  mod response_body_chunk {
    use super::*;

    #[test]
    fn yields_empty_chunk_when_body_empty() {
      let resp = HttpResponse {
        status: 204,
        headers: vec![],
        body: vec![],
      };
      let c = response_body_chunk(&resp);
      assert!(c.is_empty());
    }

    #[test]
    fn yields_non_empty_chunk_matching_body() {
      let resp = HttpResponse {
        status: 200,
        headers: vec![],
        body: vec![7, 8],
      };
      let c = response_body_chunk(&resp);
      assert_eq!(c.len(), 2);
    }
  }
}

/// Reqwest-specific HTTP helpers (migrated from `id_effect_platform::http::reqwest`).
pub mod reqwest;

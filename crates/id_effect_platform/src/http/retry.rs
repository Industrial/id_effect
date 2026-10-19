//! Client-level retry: [`RetryingHttpClient`] decorates any [`HttpClient`] with one
//! [`RetryPolicy`], configured once for every call made through it.
//!
//! The policy wraps a core [`Schedule`] (stepped with a [`RetryCause`]) and the set of methods it
//! retries. An attempt is retried only when it failed with a connect error or answered `5xx` or
//! `429`; any other outcome (success, `4xx`, timeout, invalid request, body too large) is
//! returned at once. A `Retry-After` header (delta-seconds) lengthens the next delay to at least
//! that long; one longer than [`RetryPolicy::max_retry_after`] stops the retries. When the
//! schedule is done the last outcome is returned unchanged: the last `5xx` / `429` response, or
//! the last connect error.
//!
//! [`RetryPolicy::default`] is the 0.10 client default (D10): GET, PUT and DELETE (the
//! idempotent [`HttpMethod`]s), 3 attempts, exponential backoff from 100 ms with jitter.
//! Non-idempotent methods (POST, PATCH) are retried only when listed with
//! [`RetryPolicy::with_methods`].
//!
//! Delays are slept on the running fiber's executor clock (virtual time on `TestExecutor`).

use std::sync::Arc;
use std::time::Duration;

use id_effect::{Effect, Schedule, fail, succeed};

use super::{HttpClient, HttpMethod, HttpRequest, HttpResponse, StreamingHttpResponse};
use crate::error::HttpError;

/// Why an attempt is retryable: the input a [`RetryPolicy`] schedule is stepped with.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RetryCause {
  /// The response status (`5xx` or `429`), or `None` for a connect error.
  pub status: Option<u16>,
  /// The response's `Retry-After` delay (delta-seconds form), if it sent one.
  pub retry_after: Option<Duration>,
}

/// A client-wide retry policy: which methods are retried and the [`Schedule`] spacing the
/// attempts. See the [module docs](self).
#[derive(Clone, Debug)]
pub struct RetryPolicy {
  schedule: Schedule<(), RetryCause>,
  methods: Vec<HttpMethod>,
  max_retry_after: Duration,
}

impl RetryPolicy {
  /// Default total attempts (the first one plus two retries).
  pub const DEFAULT_MAX_ATTEMPTS: u32 = 3;
  /// Default first backoff delay (then doubled per retry, jittered by `[0.8, 1.2]`).
  pub const DEFAULT_BASE_DELAY: Duration = Duration::from_millis(100);
  /// Default longest `Retry-After` the policy waits for.
  pub const DEFAULT_MAX_RETRY_AFTER: Duration = Duration::from_secs(30);

  /// The idempotent methods retried by default.
  pub const IDEMPOTENT_METHODS: [HttpMethod; 3] =
    [HttpMethod::Get, HttpMethod::Put, HttpMethod::Delete];

  /// A policy driven by `schedule` (stepped with each [`RetryCause`]; its delays space the
  /// attempts), retrying the idempotent methods.
  pub fn new<Out: 'static>(schedule: Schedule<Out, RetryCause>) -> Self {
    Self {
      schedule: schedule.map(|_| ()),
      methods: Self::IDEMPOTENT_METHODS.to_vec(),
      max_retry_after: Self::DEFAULT_MAX_RETRY_AFTER,
    }
  }

  /// At most `max_attempts` attempts in total, `base * 2^k` apart with jitter.
  pub fn exponential(max_attempts: u32, base: Duration) -> Self {
    let retries = u64::from(max_attempts.saturating_sub(1));
    Self::new(
      Schedule::exponential(base)
        .jittered()
        .intersect(Schedule::recurs(retries)),
    )
  }

  /// Never retries.
  pub fn none() -> Self {
    Self::new(Schedule::recurs(0))
  }

  /// Retries exactly `methods` (replacing the idempotent default; list `Post` / `Patch` to opt
  /// non-idempotent requests in).
  pub fn with_methods(mut self, methods: impl IntoIterator<Item = HttpMethod>) -> Self {
    self.methods = methods.into_iter().collect();
    self
  }

  /// Stops retrying when a `Retry-After` asks for longer than `max`.
  pub fn with_max_retry_after(mut self, max: Duration) -> Self {
    self.max_retry_after = max;
    self
  }

  /// `true` when requests with `method` are retried.
  pub fn retries(&self, method: &HttpMethod) -> bool {
    self.methods.contains(method)
  }

  /// The longest `Retry-After` the policy waits for.
  pub fn max_retry_after(&self) -> Duration {
    self.max_retry_after
  }

  /// The policy schedule over one attempt's failure: the user schedule for retryable causes
  /// (its delay raised to `Retry-After`), done for anything else.
  fn schedule_for<T: Send + 'static>(&self) -> Schedule<(), Failed<T>> {
    let inner = self.schedule.clone();
    let first = inner.clone();
    let cap = self.max_retry_after;
    Schedule::from_fn(
      move || first.start(),
      move |now, failed: &Failed<T>, s| match failed {
        Failed::Fatal(_) => (s, (), None),
        Failed::Retry(cause, _) => {
          let (s, out, delay) = inner.step(now, cause, s).into_parts();
          let delay = match cause.retry_after {
            Some(wait) if wait > cap => None,
            Some(wait) => delay.map(|d| d.max(wait)),
            None => delay,
          };
          (s, out, delay)
        }
      },
    )
  }
}

impl Default for RetryPolicy {
  /// The D10 default: idempotent methods, 3 attempts, exponential from 100 ms with jitter.
  fn default() -> Self {
    Self::exponential(Self::DEFAULT_MAX_ATTEMPTS, Self::DEFAULT_BASE_DELAY)
  }
}

/// One attempt's typed failure inside the retry loop.
enum Failed<T> {
  /// Retryable; the outcome is kept so an exhausted policy returns it unchanged.
  Retry(RetryCause, Result<T, HttpError>),
  /// Not retryable: returned as is.
  Fatal(HttpError),
}

/// The delta-seconds `Retry-After` in `headers` (an HTTP-date is ignored).
fn retry_after(headers: &[(String, String)]) -> Option<Duration> {
  headers
    .iter()
    .find(|(k, _)| k.eq_ignore_ascii_case("retry-after"))
    .and_then(|(_, v)| v.trim().parse::<u64>().ok())
    .map(Duration::from_secs)
}

/// A response's status and headers.
type Meta<T> = fn(&T) -> (u16, &[(String, String)]);

/// Sorts one attempt's result into success, retryable or fatal.
fn classify<T: Send + 'static>(res: Result<T, HttpError>, meta: Meta<T>) -> Effect<T, Failed<T>> {
  match res {
    Ok(t) => {
      let (status, headers) = meta(&t);
      if status == 429 || (500..600).contains(&status) {
        let cause = RetryCause {
          status: Some(status),
          retry_after: retry_after(headers),
        };
        fail(Failed::Retry(cause, Ok(t)))
      } else {
        succeed(t)
      }
    }
    Err(HttpError::Reqwest(e)) if e.is_connect() => {
      let cause = RetryCause {
        status: None,
        retry_after: None,
      };
      fail(Failed::Retry(cause, Err(HttpError::Reqwest(e))))
    }
    Err(e) => fail(Failed::Fatal(e)),
  }
}

fn buffered_meta(r: &HttpResponse) -> (u16, &[(String, String)]) {
  (r.status, &r.headers)
}

fn streaming_meta(r: &StreamingHttpResponse) -> (u16, &[(String, String)]) {
  (r.status, &r.headers)
}

/// An [`HttpClient`] that retries `inner`'s calls as its [`RetryPolicy`] allows (the default
/// [`ReqwestHttpClientProvider`](super::ReqwestHttpClientProvider) layer installs one).
#[derive(Debug)]
pub struct RetryingHttpClient<C> {
  inner: Arc<C>,
  policy: RetryPolicy,
}

impl<C> Clone for RetryingHttpClient<C> {
  fn clone(&self) -> Self {
    Self {
      inner: Arc::clone(&self.inner),
      policy: self.policy.clone(),
    }
  }
}

impl<C: HttpClient> RetryingHttpClient<C> {
  /// Decorates `inner` with `policy`.
  #[inline]
  pub fn new(inner: C, policy: RetryPolicy) -> Self {
    Self {
      inner: Arc::new(inner),
      policy,
    }
  }

  /// The policy applied to every call.
  #[inline]
  pub fn policy(&self) -> &RetryPolicy {
    &self.policy
  }

  fn run<T: Send + 'static>(
    &self,
    req: HttpRequest,
    call: fn(&C, HttpRequest) -> Effect<T, HttpError>,
    meta: Meta<T>,
  ) -> Effect<T, HttpError> {
    if !self.policy.retries(&req.method) {
      return call(&self.inner, req);
    }
    let inner = Arc::clone(&self.inner);
    Effect::retry_or_else(
      move || {
        call(&inner, req.clone())
          .map(Ok)
          .catch(|e| succeed(Err(e)))
          .flat_map(move |r| classify(r, meta))
      },
      self.policy.schedule_for::<T>(),
      |failed, ()| match failed {
        Failed::Retry(_, Ok(t)) => succeed(t),
        Failed::Retry(_, Err(e)) | Failed::Fatal(e) => fail(e),
      },
    )
  }
}

impl<C: HttpClient> HttpClient for RetryingHttpClient<C> {
  fn execute(&self, req: HttpRequest) -> Effect<HttpResponse, HttpError> {
    self.run(req, C::execute, buffered_meta)
  }

  fn execute_stream(&self, req: HttpRequest) -> Effect<StreamingHttpResponse, HttpError> {
    self.run(req, C::execute_stream, streaming_meta)
  }
}

#[cfg(test)]
mod tests {
  use std::sync::Mutex;
  use std::sync::atomic::{AtomicU32, Ordering};

  use super::*;
  use crate::http::HttpBody;
  use id_effect::{Cause, Exit};

  fn run_blocking<A: Send + 'static>(eff: Effect<A, HttpError>) -> Result<A, HttpError> {
    match eff.block_on() {
      Exit::Success(a) => Ok(a),
      Exit::Failure(Cause::Fail(e)) => Err(e),
      Exit::Failure(c) => panic!("unexpected cause: {c:?}"),
    }
  }

  /// Answers each call with the next scripted outcome (the last one repeats).
  struct Scripted {
    script: Mutex<Vec<Result<u16, HttpError>>>,
    calls: Arc<AtomicU32>,
  }

  impl Scripted {
    fn new(script: Vec<Result<u16, HttpError>>) -> (Self, Arc<AtomicU32>) {
      let calls = Arc::new(AtomicU32::new(0));
      let s = Self {
        script: Mutex::new(script),
        calls: Arc::clone(&calls),
      };
      (s, calls)
    }

    fn next(&self) -> Result<u16, HttpError> {
      self.calls.fetch_add(1, Ordering::SeqCst);
      let mut script = self.script.lock().unwrap();
      if script.len() > 1 {
        script.remove(0)
      } else {
        script[0].clone()
      }
    }
  }

  impl HttpClient for Scripted {
    fn execute(&self, _req: HttpRequest) -> Effect<HttpResponse, HttpError> {
      match self.next() {
        Ok(status) => succeed(HttpResponse {
          status,
          headers: vec![],
          body: vec![],
        }),
        Err(e) => fail(e),
      }
    }

    fn execute_stream(&self, _req: HttpRequest) -> Effect<StreamingHttpResponse, HttpError> {
      match self.next() {
        Ok(status) => succeed(StreamingHttpResponse {
          status,
          headers: vec![],
          body: HttpBody::empty(),
        }),
        Err(e) => fail(e),
      }
    }
  }

  /// No delays, so the loop never touches the timer.
  fn immediate(retries: u64) -> RetryPolicy {
    RetryPolicy::new(Schedule::recurs(retries))
  }

  #[test]
  fn exhausted_policy_returns_the_last_retryable_response() {
    let (client, calls) = Scripted::new(vec![Ok(503)]);
    let client = RetryingHttpClient::new(client, immediate(2));
    let res = run_blocking(client.execute(HttpRequest::get("s://x"))).unwrap();
    assert_eq!(res.status, 503);
    assert_eq!(calls.load(Ordering::SeqCst), 3);
  }

  #[test]
  fn too_many_requests_is_retried_and_client_errors_are_not() {
    let (client, calls) = Scripted::new(vec![Ok(429), Ok(404)]);
    let client = RetryingHttpClient::new(client, immediate(5));
    let res = run_blocking(client.execute_stream(HttpRequest::get("s://x"))).unwrap();
    assert_eq!(res.status, 404);
    assert_eq!(calls.load(Ordering::SeqCst), 2);
  }

  #[test]
  fn non_transport_errors_are_not_retried() {
    let (client, calls) = Scripted::new(vec![Err(HttpError::InvalidRequest("bad".into()))]);
    let client = RetryingHttpClient::new(client, immediate(5));
    let err = run_blocking(client.execute(HttpRequest::get("s://x"))).unwrap_err();
    assert!(matches!(err, HttpError::InvalidRequest(_)));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
  }

  #[test]
  fn methods_outside_the_policy_are_called_once() {
    let (client, calls) = Scripted::new(vec![Ok(500)]);
    let client = RetryingHttpClient::new(client, immediate(5).with_methods([HttpMethod::Get]));
    let mut req = HttpRequest::get("s://x");
    req.method = HttpMethod::Delete;
    assert_eq!(run_blocking(client.execute(req)).unwrap().status, 500);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(
      client.clone().policy().max_retry_after(),
      Duration::from_secs(30)
    );
  }

  #[test]
  fn none_never_retries_and_default_retries_only_idempotent_methods() {
    let (client, calls) = Scripted::new(vec![Ok(502)]);
    let client = RetryingHttpClient::new(client, RetryPolicy::none());
    assert_eq!(
      run_blocking(client.execute(HttpRequest::get("s://x")))
        .unwrap()
        .status,
      502
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    let policy = RetryPolicy::default();
    for m in RetryPolicy::IDEMPOTENT_METHODS {
      assert!(policy.retries(&m));
    }
    assert!(!policy.retries(&HttpMethod::Post));
    assert!(!policy.retries(&HttpMethod::Patch));
  }

  #[test]
  fn retry_after_parses_delta_seconds_only() {
    let h = |v: &str| vec![("Retry-After".to_string(), v.to_string())];
    assert_eq!(retry_after(&h(" 7 ")), Some(Duration::from_secs(7)));
    assert_eq!(retry_after(&h("Wed, 21 Oct 2015 07:28:00 GMT")), None);
    assert_eq!(retry_after(&[]), None);
  }

  #[tokio::test]
  async fn connect_errors_are_retried() {
    use id_effect_tokio::TokioRuntime;

    struct Counting(crate::http::ReqwestHttpClient, Arc<AtomicU32>);

    impl HttpClient for Counting {
      fn execute(&self, req: HttpRequest) -> Effect<HttpResponse, HttpError> {
        self.1.fetch_add(1, Ordering::SeqCst);
        self.0.execute(req)
      }

      fn execute_stream(&self, req: HttpRequest) -> Effect<StreamingHttpResponse, HttpError> {
        self.0.execute_stream(req)
      }
    }

    let calls = Arc::new(AtomicU32::new(0));
    let local = crate::http::HttpClientConfig::default().allow_hosts(["127.0.0.1"]);
    let inner = Counting(
      crate::http::ReqwestHttpClient::from_config(&local),
      Arc::clone(&calls),
    );
    let client = RetryingHttpClient::new(inner, immediate(2));
    let exit = TokioRuntime::current()
      .expect("tokio")
      .run(client.execute(HttpRequest::get("http://127.0.0.1:1/refused")))
      .await;
    assert!(
      matches!(&exit, Exit::Failure(Cause::Fail(HttpError::Reqwest(e))) if e.is_connect()),
      "{exit:?}"
    );
    assert_eq!(calls.load(Ordering::SeqCst), 3);
  }
}

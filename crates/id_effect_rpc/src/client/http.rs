//! `POST /rpc` calls through id_effect_platform's [`HttpClientService`], in the 0.12 wire
//! format: a [`WireCall`] envelope out, a [`WireResult`] (or a Problem) back.
//!
//! [`RpcClient::call`] batches: the calls a program makes in the same tick — the children of
//! one [`all`](id_effect::all), for example — go out as one batch request (design note §2.2),
//! through an `id_effect` [`RequestResolver`] and its batch window (bounded: at most 64 yields
//! of the executor). A window's calls are split by client and URL, then into requests of at
//! most [`CLIENT_BATCH_MAX_CALLS`] calls and [`CLIENT_BATCH_MAX_BYTES`] bytes; a request with a
//! single call is a plain envelope. Every call gets its own result, decoded with its own
//! schema. [`RpcClient::call_unbatched`] opts a call out: it is sent at once, on its own.

use std::convert::Infallible;
use std::hash::{Hash, Hasher};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};

use id_effect::schema::Schema;
use id_effect::schema::data::EffectData;
use id_effect::{
  BatchResults, Concurrency, Effect, Exit, HList, HNil, Request, RequestResolver, Selector, all,
  fail, need, succeed,
};
use id_effect_axum::route::Problem;
use id_effect_platform::error::HttpError;
use id_effect_platform::http::{HttpClient, HttpClientService, HttpMethod, HttpRequest};
use serde_json::Value;

use super::{RPC_HEADER, RpcClient, RpcClientError};
use crate::serialization::{
  RpcSerializationError, StructWireJson, decode_payload, encode_struct_fields,
};
use crate::wire::{CallId, MAX_BATCH_BYTES, MAX_BATCH_CALLS, WireCall, WireExit, WireResult};

/// Most calls the client puts in one batch request (the server default).
pub const CLIENT_BATCH_MAX_CALLS: usize = MAX_BATCH_CALLS;

/// Most body bytes of one batch request (the server default).
pub const CLIENT_BATCH_MAX_BYTES: usize = MAX_BATCH_BYTES;

impl RpcClient {
  /// Call the procedure `tag` with a schema-encoded `payload` (`payload_fields` names the
  /// members of the encoded object) and decode its output with `success_schema`. Calls made in
  /// the same tick are sent as one batch ([module docs](self));
  /// [`call_unbatched`](Self::call_unbatched) sends one on its own.
  ///
  /// A typed or reserved failure is [`RpcClientError::Failure`] (the `{"_tag": …}` value); a
  /// refusal before the procedure ran is [`RpcClientError::Refused`] with the server's Problem
  /// (for a batch: the refusal of the whole request, given to each of its calls).
  pub fn call<A, IA, EA, B, IB, EB, R, I>(
    &self,
    tag: impl Into<String>,
    payload: A,
    payload_schema: &Schema<A, IA, EA>,
    payload_fields: &[&str],
    success_schema: &Schema<B, IB, EB>,
  ) -> Effect<B, RpcClientError, R>
  where
    R: Selector<HttpClientService, I> + HList,
    A: Send + 'static,
    B: Send + 'static,
    EA: EffectData + 'static,
    EB: EffectData + 'static,
    IA: Send + Sync + 'static + StructWireJson,
    IB: Send + Sync + 'static,
  {
    let payload = match encode_struct_fields(payload_schema, payload_fields, payload) {
      Ok(v) => v,
      Err(e) => return fail(RpcClientError::Serialization(e)),
    };
    let tag: Arc<str> = Arc::from(tag.into());
    let url: Arc<str> = Arc::from(self.config().url());
    let success_schema = success_schema.clone();
    need::<HttpClientService>().flat_map(move |client| {
      let pending = Pending {
        seq: NEXT_SEQ.fetch_add(1, Ordering::Relaxed),
        url,
        client: client.0.clone(),
        tag,
        payload: Arc::new(payload),
      };
      // Every call is distinct (its `seq`): a request cache would only fill up.
      id_effect::request(pending, batcher())
        .with_request_caching(false)
        .map_err(|never: Infallible| match never {})
        .flat_map(move |answer| match answer.decode(&success_schema) {
          Ok(b) => succeed(b),
          Err(e) => fail(e),
        })
        .narrow()
    })
  }

  /// Call `tag` over postcard (feature `postcard`, design note §7): `payload` and the output
  /// cross the wire as postcard bytes of their serde form, which the server reads with the
  /// procedure's typed codec (`WireSchema::with_serde`); `A` and `B` serialize like the
  /// procedure's input and output types. Sent at once (batches are JSON). A procedure without a
  /// typed codec answers [`RpcClientError::Refused`] with a `415` Problem.
  #[cfg(feature = "postcard")]
  pub fn call_postcard<A, B, R, I>(
    &self,
    tag: impl Into<String>,
    payload: &A,
  ) -> Effect<B, RpcClientError, R>
  where
    R: Selector<HttpClientService, I> + HList,
    A: serde::Serialize + ?Sized,
    B: serde::de::DeserializeOwned + Send + 'static,
  {
    use crate::codec::Codec as _;
    use crate::codec::postcard::{MEDIA_TYPE, Postcard, encode_call};
    let protocol = |e: crate::codec::CodecError| {
      RpcClientError::Serialization(RpcSerializationError::Protocol(e.to_string()))
    };
    let body =
      match Postcard::encode(payload).and_then(|bytes| encode_call(&WireCall::new(tag, bytes))) {
        Ok(body) => body,
        Err(e) => return fail(protocol(e)),
      };
    let req = post_as(self.config().url(), body, MEDIA_TYPE);
    need::<HttpClientService>().flat_map(move |client| {
      client
        .execute(req)
        .map_err(RpcClientError::Http)
        .flat_map(
          move |resp| match decode_postcard_response(resp.status, &resp.body) {
            Ok(b) => succeed(b),
            Err(e) => fail(e),
          },
        )
        .narrow()
    })
  }

  /// [`call`](Self::call), sent at once in a request of its own (no batch window).
  pub fn call_unbatched<A, IA, EA, B, IB, EB, R, I>(
    &self,
    tag: impl Into<String>,
    payload: A,
    payload_schema: &Schema<A, IA, EA>,
    payload_fields: &[&str],
    success_schema: &Schema<B, IB, EB>,
  ) -> Effect<B, RpcClientError, R>
  where
    R: Selector<HttpClientService, I> + HList,
    A: Send + 'static,
    B: Send + 'static,
    EA: EffectData + 'static,
    EB: EffectData + 'static,
    IA: Send + Sync + 'static + StructWireJson,
    IB: Send + Sync + 'static,
  {
    let payload = match encode_struct_fields(payload_schema, payload_fields, payload) {
      Ok(v) => v,
      Err(e) => return fail(RpcClientError::Serialization(e)),
    };
    let wire = WireCall::new(tag, payload);
    // A tag and a `serde_json::Value` always serialize.
    let body = serde_json::to_vec(&wire).unwrap_or_default();
    let success_schema = success_schema.clone();
    let req = post(self.config().url(), body);
    need::<HttpClientService>().flat_map(move |client| {
      client
        .execute(req)
        .map_err(RpcClientError::Http)
        .flat_map(
          move |resp| match decode_response(&success_schema, resp.status, &resp.body) {
            Ok(b) => id_effect::succeed(b),
            Err(e) => fail(e),
          },
        )
        .narrow()
    })
  }
}

fn post(url: String, body: Vec<u8>) -> HttpRequest {
  post_as(url, body, "application/json")
}

fn post_as(url: String, body: Vec<u8>, content_type: &str) -> HttpRequest {
  HttpRequest {
    method: HttpMethod::Post,
    url,
    headers: vec![
      ("content-type".to_owned(), content_type.to_owned()),
      (RPC_HEADER.to_owned(), "1".to_owned()),
    ],
    body: Some(body),
    timeout: None,
    max_body_bytes: None,
  }
}

/// Decode a response: a [`WireResult`] (success against `schema`, failure as
/// [`RpcClientError::Failure`]), a Problem as [`RpcClientError::Refused`], anything else by
/// status.
pub fn decode_response<B, IB, EB>(
  schema: &Schema<B, IB, EB>,
  status: u16,
  body: &[u8],
) -> Result<B, RpcClientError>
where
  B: 'static,
  IB: 'static,
  EB: EffectData + 'static,
{
  Answer::of_response(status, body).decode(schema)
}

/// Decode a postcard response: a postcard result (its success decoded as `B`), else a JSON
/// Problem or the status.
#[cfg(feature = "postcard")]
pub fn decode_postcard_response<B>(status: u16, body: &[u8]) -> Result<B, RpcClientError>
where
  B: serde::de::DeserializeOwned,
{
  use crate::codec::Codec as _;
  use crate::codec::postcard::{Postcard, decode_result};
  let protocol = |e: crate::codec::CodecError| {
    RpcClientError::Serialization(RpcSerializationError::Protocol(e.to_string()))
  };
  match decode_result(body) {
    Ok(result) => match result.exit {
      WireExit::Success { value } => Postcard::decode(&value).map_err(protocol),
      WireExit::Failure { error } => Err(RpcClientError::Failure(error)),
    },
    Err(_) => match Answer::of_refusal(status, body) {
      Answer::Refused(problem) => Err(RpcClientError::Refused(problem)),
      Answer::Status(status) => Err(RpcClientError::UnexpectedStatus(status)),
      _ => Err(RpcClientError::Serialization(
        RpcSerializationError::Protocol(format!(
          "a 200 response that is not a postcard rpc result ({} bytes)",
          body.len()
        )),
      )),
    },
  }
}

const NO_RESULT: &str = "the batch response has no result for this call";

/// What one call got back, before its output is decoded (cloned to every call a whole-request
/// answer concerns).
#[derive(Clone, Debug)]
enum Answer {
  Exit(WireExit),
  Refused(Box<Problem>),
  Status(u16),
  Protocol(String),
  Http(HttpError),
}

impl Answer {
  /// The answer of a single-call response.
  fn of_response(status: u16, body: &[u8]) -> Self {
    if let Ok(result) = serde_json::from_slice::<WireResult>(body) {
      return Self::Exit(result.exit);
    }
    Self::of_refusal(status, body)
  }

  /// A response that carries no result: a Problem, else its status.
  fn of_refusal(status: u16, body: &[u8]) -> Self {
    if let Ok(problem) = serde_json::from_slice::<Problem>(body) {
      return Self::Refused(Box::new(problem));
    }
    if status != 200 {
      return Self::Status(status);
    }
    Self::Protocol(format!(
      "a 200 response that is not an rpc result ({} bytes)",
      body.len()
    ))
  }

  fn decode<B, IB, EB>(self, schema: &Schema<B, IB, EB>) -> Result<B, RpcClientError>
  where
    B: 'static,
    IB: 'static,
    EB: EffectData + 'static,
  {
    match self {
      Self::Exit(WireExit::Success { value }) => {
        decode_payload(schema, &value).map_err(RpcClientError::Serialization)
      }
      Self::Exit(WireExit::Failure { error }) => Err(RpcClientError::Failure(error)),
      Self::Refused(problem) => Err(RpcClientError::Refused(problem)),
      Self::Status(status) => Err(RpcClientError::UnexpectedStatus(status)),
      Self::Protocol(why) => Err(RpcClientError::Serialization(
        RpcSerializationError::Protocol(why),
      )),
      Self::Http(err) => Err(RpcClientError::Http(err)),
    }
  }
}

static NEXT_SEQ: AtomicU64 = AtomicU64::new(0);

/// One call waiting in the batch window; equal only to itself (`seq`), so calls never merge.
#[derive(Clone)]
struct Pending {
  seq: u64,
  url: Arc<str>,
  client: Arc<dyn HttpClient>,
  tag: Arc<str>,
  payload: Arc<Value>,
}

impl PartialEq for Pending {
  fn eq(&self, other: &Self) -> bool {
    self.seq == other.seq
  }
}

impl Eq for Pending {}

impl Hash for Pending {
  fn hash<H: Hasher>(&self, state: &mut H) {
    self.seq.hash(state);
  }
}

impl Request for Pending {
  type Output = Answer;
  type Error = Infallible;
}

/// The process-wide batch window of [`RpcClient::call`].
fn batcher() -> &'static RequestResolver<Pending> {
  static BATCHER: OnceLock<RequestResolver<Pending>> = OnceLock::new();
  BATCHER.get_or_init(|| {
    RequestResolver::make_batched(|calls| {
      send_window(calls, CLIENT_BATCH_MAX_CALLS, CLIENT_BATCH_MAX_BYTES)
    })
  })
}

/// One request's calls: their index in the window, tag and payload.
type Chunk = Vec<(usize, Arc<str>, Arc<Value>)>;

/// A client, a URL and the calls of one request to it.
type Outgoing = (Arc<dyn HttpClient>, Arc<str>, Chunk);

/// Per client and URL while splitting: its requests so far and the bytes of the last one.
type Group = (Arc<dyn HttpClient>, Arc<str>, Vec<Chunk>, usize);

/// Split a window by client and URL, then into requests within the caps.
fn chunks(calls: &[Pending], max_calls: usize, max_bytes: usize) -> Vec<Outgoing> {
  let mut groups: Vec<Group> = Vec::new();
  for (index, call) in calls.iter().enumerate() {
    // A call's share of the array: its encoding, an id and a comma.
    let size =
      serde_json::to_vec(&WireCall::new(&*call.tag, &*call.payload)).map_or(0, |v| v.len()) + 16;
    let at = match groups
      .iter()
      .position(|(client, url, ..)| Arc::ptr_eq(client, &call.client) && *url == call.url)
    {
      Some(at) => at,
      None => {
        groups.push((call.client.clone(), call.url.clone(), vec![Vec::new()], 2));
        groups.len() - 1
      }
    };
    let (_, _, requests, bytes) = &mut groups[at];
    let full = requests
      .last()
      .is_some_and(|c| !c.is_empty() && (c.len() >= max_calls || *bytes + size > max_bytes));
    if full {
      requests.push(Vec::new());
      *bytes = 2;
    }
    *bytes += size;
    if let Some(chunk) = requests.last_mut() {
      chunk.push((index, call.tag.clone(), call.payload.clone()));
    }
  }
  groups
    .into_iter()
    .flat_map(|(client, url, requests, _)| {
      requests
        .into_iter()
        .map(move |chunk| (client.clone(), url.clone(), chunk))
    })
    .collect()
}

/// Answer a batch window: one request per chunk, all at once.
fn send_window(
  calls: Vec<Pending>,
  max_calls: usize,
  max_bytes: usize,
) -> Effect<BatchResults<Pending>, Infallible, HNil> {
  let n = calls.len();
  let sends: Vec<_> = chunks(&calls, max_calls, max_bytes)
    .into_iter()
    .map(|(client, url, chunk)| send_chunk(&*client, url.to_string(), chunk))
    .collect();
  all(sends, Concurrency::Unbounded).map(move |answered| {
    let mut answers: Vec<Option<Answer>> = vec![None; n];
    for (index, answer) in answered.into_iter().flatten() {
      answers[index] = Some(answer);
    }
    answers
      .into_iter()
      .map(|a| Ok(a.unwrap_or_else(|| Answer::Protocol(NO_RESULT.to_owned()))))
      .collect()
  })
}

/// The exits of a batch response, by position in the request (ids are 1-based positions).
fn exits_by_position(results: Vec<WireResult>, len: usize) -> Vec<Answer> {
  let mut by_id: Vec<Option<WireExit>> = vec![None; len];
  for result in results {
    let slot = result
      .id
      .and_then(|id| usize::try_from(id.get()).ok())
      .and_then(|id| id.checked_sub(1))
      .filter(|slot| *slot < len);
    if let Some(slot) = slot {
      by_id[slot] = Some(result.exit);
    }
  }
  by_id
    .into_iter()
    .map(|exit| exit.map_or_else(|| Answer::Protocol(NO_RESULT.to_owned()), Answer::Exit))
    .collect()
}

/// Send one request: a plain envelope for one call, a batch array for more.
fn send_chunk(
  client: &dyn HttpClient,
  url: String,
  chunk: Chunk,
) -> Effect<Vec<(usize, Answer)>, Infallible, HNil> {
  let single = chunk.len() == 1;
  let encoded = if single {
    let (_, tag, payload) = &chunk[0];
    serde_json::to_vec(&WireCall::new(&**tag, &**payload))
  } else {
    let calls: Vec<WireCall<&Value>> = chunk
      .iter()
      .zip(1_u64..)
      .map(|((_, tag, payload), id)| {
        let call = WireCall::new(&**tag, &**payload);
        // At most a few thousand calls: far below 2^53.
        match CallId::new(id) {
          Ok(id) => call.with_id(id),
          Err(_) => call,
        }
      })
      .collect();
    serde_json::to_vec(&calls)
  };
  // Tags and `serde_json::Value`s always serialize.
  let body = encoded.unwrap_or_default();
  let indexes: Vec<usize> = chunk.iter().map(|(index, ..)| *index).collect();
  client
    .execute(post(url, body))
    .exit::<Infallible>()
    .map(move |exit| {
      let answers: Vec<Answer> = match exit {
        Exit::Success(resp) if single => vec![Answer::of_response(resp.status, &resp.body)],
        Exit::Success(resp) => match serde_json::from_slice::<Vec<WireResult>>(&resp.body) {
          Ok(results) if resp.status == 200 => exits_by_position(results, indexes.len()),
          _ => vec![Answer::of_refusal(resp.status, &resp.body); indexes.len()],
        },
        Exit::Failure(cause) => {
          let answer = cause.fail_option().map_or_else(
            || Answer::Protocol("the http client was interrupted".to_owned()),
            Answer::Http,
          );
          vec![answer; indexes.len()]
        }
      };
      indexes.into_iter().zip(answers).collect()
    })
}

#[cfg(test)]
mod tests {
  use super::*;
  use id_effect::hlist;
  use id_effect::schema;
  use id_effect_platform::http::{HttpClient, HttpResponse};
  use std::sync::{Arc, Mutex};

  struct MockHttp {
    response: HttpResponse,
    last: Mutex<Option<HttpRequest>>,
  }

  impl HttpClient for MockHttp {
    fn execute(
      &self,
      req: HttpRequest,
    ) -> Effect<HttpResponse, id_effect_platform::error::HttpError> {
      *self.last.lock().unwrap() = Some(req);
      id_effect::succeed(self.response.clone())
    }

    fn execute_stream(
      &self,
      _req: HttpRequest,
    ) -> Effect<id_effect_platform::http::StreamingHttpResponse, id_effect_platform::error::HttpError>
    {
      fail(id_effect_platform::error::HttpError::InvalidRequest(
        "mock stream".to_owned(),
      ))
    }
  }

  fn call_with(
    status: u16,
    body: Vec<u8>,
  ) -> (Result<(String, i64), RpcClientError>, Arc<MockHttp>) {
    let payload = schema::struct_("name", schema::string::<()>(), "n", schema::i64::<()>());
    let success = schema::struct_(
      "message",
      schema::string::<()>(),
      "count",
      schema::i64::<()>(),
    );
    let mock = Arc::new(MockHttp {
      response: HttpResponse {
        status,
        headers: vec![],
        body,
      },
      last: Mutex::new(None),
    });
    let env = hlist![HttpClientService(mock.clone() as Arc<dyn HttpClient>)];
    let out = RpcClient::new("http://127.0.0.1:8080")
      .call(
        "Greeter.greet",
        ("Ada".to_owned(), 1_i64),
        &payload,
        &["name", "n"],
        &success,
      )
      .provide_all(env)
      .block_on()
      .into_result();
    (typed(out), mock)
  }

  fn typed<A>(out: Result<A, id_effect::Cause<RpcClientError>>) -> Result<A, RpcClientError> {
    out.map_err(|cause| cause.fail_option().expect("a typed failure"))
  }

  #[test]
  fn call_posts_a_wire_call_to_the_dispatch_url() {
    let body = br#"{"tag":"Greeter.greet","exit":{"_tag":"Success","value":{"message":"Hello!","count":1}}}"#;
    let (out, mock) = call_with(200, body.to_vec());
    assert_eq!(out.expect("call").0, "Hello!");
    let req = mock.last.lock().unwrap().clone().unwrap();
    assert_eq!(req.url, "http://127.0.0.1:8080/rpc");
    assert!(req.headers.contains(&("x-rpc".to_owned(), "1".to_owned())));
    let sent: serde_json::Value = serde_json::from_slice(req.body.as_deref().unwrap()).unwrap();
    assert_eq!(
      sent,
      serde_json::json!({"tag": "Greeter.greet", "payload": {"name": "Ada", "n": 1}})
    );
  }

  #[test]
  fn failures_decode_by_shape_then_status() {
    let failure = br#"{"tag":"Greeter.greet","exit":{"_tag":"Failure","error":{"_tag":"NotFound","message":"nope"}}}"#;
    let (out, _) = call_with(404, failure.to_vec());
    let err = out.unwrap_err();
    assert_eq!(err.failure_tag(), Some("NotFound"));

    let (out, _) = call_with(404, br#"{"status":404,"title":"Not Found"}"#.to_vec());
    assert!(matches!(out, Err(RpcClientError::Refused(p)) if p.status.as_u16() == 404));

    let (out, _) = call_with(502, b"bad gateway".to_vec());
    assert!(matches!(out, Err(RpcClientError::UnexpectedStatus(502))));

    let (out, _) = call_with(200, b"not json".to_vec());
    assert!(matches!(out, Err(RpcClientError::Serialization(_))));

    let bad = br#"{"tag":"Greeter.greet","exit":{"_tag":"Success","value":{"message":1}}}"#;
    let (out, _) = call_with(200, bad.to_vec());
    assert!(matches!(out, Err(RpcClientError::Serialization(_))));
  }

  #[test]
  fn unencodable_payload_fails_before_sending() {
    let payload = schema::struct_("name", schema::string::<()>(), "n", schema::i64::<()>());
    let success = schema::string::<()>();
    let mock = Arc::new(MockHttp {
      response: HttpResponse {
        status: 200,
        headers: vec![],
        body: vec![],
      },
      last: Mutex::new(None),
    });
    let env = hlist![HttpClientService(mock.clone() as Arc<dyn HttpClient>)];
    let out = RpcClient::new("http://h")
      .call(
        "A.b",
        ("x".to_owned(), 1_i64),
        &payload,
        &["only_one"],
        &success,
      )
      .provide_all(env)
      .block_on()
      .into_result();
    assert!(matches!(typed(out), Err(RpcClientError::Serialization(_))));
    assert!(mock.last.lock().unwrap().is_none());
  }

  fn mock(status: u16, body: &[u8]) -> Arc<MockHttp> {
    Arc::new(MockHttp {
      response: HttpResponse {
        status,
        headers: vec![],
        body: body.to_vec(),
      },
      last: Mutex::new(None),
    })
  }

  fn pending(client: &Arc<MockHttp>, url: &str, text: &str) -> Pending {
    Pending {
      seq: NEXT_SEQ.fetch_add(1, Ordering::Relaxed),
      url: Arc::from(url),
      client: client.clone() as Arc<dyn HttpClient>,
      tag: Arc::from("A.b"),
      payload: Arc::new(Value::String(text.to_owned())),
    }
  }

  #[test]
  fn windows_split_by_client_url_count_and_bytes() {
    let (a, b) = (mock(200, b""), mock(200, b""));
    let calls: Vec<Pending> = vec![
      pending(&a, "u1", "x"),
      pending(&a, "u1", "x"),
      pending(&a, "u1", "x"),
      pending(&a, "u2", "x"),
      pending(&b, "u1", "x"),
      pending(&a, "u1", &"y".repeat(200)),
    ];
    let sizes = |max_calls, max_bytes| -> Vec<Vec<usize>> {
      chunks(&calls, max_calls, max_bytes)
        .into_iter()
        .map(|(_, _, chunk)| chunk.iter().map(|(i, ..)| *i).collect())
        .collect()
    };
    assert_eq!(sizes(50, 1 << 20), [vec![0, 1, 2, 5], vec![3], vec![4]]);
    assert_eq!(
      sizes(2, 1 << 20),
      [vec![0, 1], vec![2, 5], vec![3], vec![4]]
    );
    // 200 bytes do not fit beside the others; a call over the cap still goes, alone.
    assert_eq!(sizes(50, 150), [vec![0, 1, 2], vec![5], vec![3], vec![4]]);
    assert!(Pending::eq(&calls[0], &calls[0].clone()) && calls[0] != calls[1]);
  }

  fn answers(status: u16, body: &[u8], n: usize) -> Vec<(usize, Answer)> {
    let client = mock(status, body);
    let chunk: Chunk = (0..n)
      .map(|i| (i + 10, Arc::from("A.b"), Arc::new(Value::from(i))))
      .collect();
    let out = send_chunk(&*client, "http://h/rpc".to_owned(), chunk)
      .block_on()
      .into_result();
    let sent: Value = serde_json::from_slice(
      client
        .last
        .lock()
        .unwrap()
        .as_ref()
        .unwrap()
        .body
        .as_deref()
        .unwrap(),
    )
    .unwrap();
    assert_eq!(sent.is_array(), n > 1);
    out.ok().unwrap()
  }

  #[test]
  fn batch_responses_map_by_id_and_refusals_reach_every_call() {
    let ok = br#"[{"id":2,"tag":"A.b","exit":{"_tag":"Success","value":"two"}},{"id":9,"tag":"A.b","exit":{"_tag":"Success","value":"x"}},{"tag":"A.b","exit":{"_tag":"Success","value":"x"}},{"id":1,"tag":"A.b","exit":{"_tag":"Failure","error":{"_tag":"Nope"}}}]"#;
    let out = answers(200, ok, 3);
    assert_eq!(
      out.iter().map(|(i, _)| *i).collect::<Vec<_>>(),
      [10, 11, 12]
    );
    assert!(matches!(&out[0].1, Answer::Exit(WireExit::Failure { .. })));
    assert!(matches!(&out[1].1, Answer::Exit(WireExit::Success { value }) if value == "two"));
    assert!(matches!(&out[2].1, Answer::Protocol(why) if why == NO_RESULT));

    let problem = br#"{"status":429,"title":"Too Many Requests"}"#;
    for (_, answer) in answers(429, problem, 2) {
      let err = answer.decode(&schema::string::<()>()).unwrap_err();
      assert!(matches!(err, RpcClientError::Refused(p) if p.status.as_u16() == 429));
    }
    for (_, answer) in answers(502, b"bad gateway", 2) {
      assert!(matches!(answer, Answer::Status(502)));
    }
    for (_, answer) in answers(200, b"[1]", 2) {
      assert!(matches!(answer, Answer::Protocol(_)));
    }
    let single = answers(
      200,
      br#"{"tag":"A.b","exit":{"_tag":"Success","value":"one"}}"#,
      1,
    );
    assert!(matches!(
      &single[0].1,
      Answer::Exit(WireExit::Success { .. })
    ));
  }

  struct Broken;

  impl HttpClient for Broken {
    fn execute(&self, _req: HttpRequest) -> Effect<HttpResponse, HttpError> {
      fail(HttpError::InvalidRequest("refused".to_owned()))
    }

    fn execute_stream(
      &self,
      _req: HttpRequest,
    ) -> Effect<id_effect_platform::http::StreamingHttpResponse, HttpError> {
      id_effect::die("no streams")
    }
  }

  #[test]
  fn transport_failures_reach_every_call_of_the_window() {
    let chunk: Chunk = vec![(0, Arc::from("A.b"), Arc::new(Value::Null)); 2];
    let out = send_chunk(&Broken, "http://h".to_owned(), chunk)
      .block_on()
      .into_result()
      .ok()
      .unwrap();
    for (_, answer) in out {
      let err = answer.decode(&schema::string::<()>()).unwrap_err();
      assert!(matches!(
        err,
        RpcClientError::Http(HttpError::InvalidRequest(_))
      ));
    }
    // A window whose answers lack a call fills it with a protocol failure.
    let window = vec![
      pending(&mock(200, b"[]"), "u", "a"),
      pending(&mock(200, b"[]"), "u", "b"),
    ];
    let results = send_window(window, 50, 1 << 20)
      .block_on()
      .into_result()
      .ok()
      .unwrap();
    assert!(results.iter().all(|r| matches!(r, Ok(Answer::Protocol(_)))));
  }

  #[cfg(feature = "postcard")]
  #[test]
  fn postcard_responses_decode_results_problems_and_statuses() {
    use crate::codec::Codec as _;
    use crate::codec::postcard::{Postcard, encode_result};
    let ok = encode_result(&WireResult::new(
      None,
      "A.b",
      WireExit::Success {
        value: Postcard::encode(&7_i64).unwrap(),
      },
    ))
    .unwrap();
    assert_eq!(decode_postcard_response::<i64>(200, &ok).unwrap(), 7);
    assert!(matches!(
      decode_postcard_response::<String>(200, &ok),
      Err(RpcClientError::Serialization(_))
    ));
    let problem = br#"{"status":415,"title":"Unsupported Media Type"}"#;
    assert!(matches!(
      decode_postcard_response::<i64>(415, problem),
      Err(RpcClientError::Refused(p)) if p.status.as_u16() == 415
    ));
    assert!(matches!(
      decode_postcard_response::<i64>(502, b"bad gateway"),
      Err(RpcClientError::UnexpectedStatus(502))
    ));
    assert!(matches!(
      decode_postcard_response::<i64>(200, b"\xff"),
      Err(RpcClientError::Serialization(_))
    ));
    // A payload that does not encode fails before sending.
    let client = mock(200, &ok);
    let env = hlist![HttpClientService(client.clone() as Arc<dyn HttpClient>)];
    let huge = vec![0_u8; 70_000];
    let out = RpcClient::new("http://h")
      .call_postcard::<_, i64, _, _>("A.b", &huge)
      .provide_all(env)
      .block_on()
      .into_result();
    assert!(matches!(typed(out), Err(RpcClientError::Serialization(_))));
    assert!(client.last.lock().unwrap().is_none());
  }

  #[test]
  fn unbatched_calls_send_at_once() {
    let client = mock(
      200,
      br#"{"tag":"Greeter.greet","exit":{"_tag":"Success","value":{"message":"Hi","count":2}}}"#,
    );
    let payload = schema::struct_("name", schema::string::<()>(), "n", schema::i64::<()>());
    let success = schema::struct_(
      "message",
      schema::string::<()>(),
      "count",
      schema::i64::<()>(),
    );
    let env = hlist![HttpClientService(client.clone() as Arc<dyn HttpClient>)];
    let out = RpcClient::new("http://h")
      .call_unbatched(
        "Greeter.greet",
        ("Ada".to_owned(), 2_i64),
        &payload,
        &["name", "n"],
        &success,
      )
      .provide_all(env)
      .block_on()
      .into_result();
    assert_eq!(typed(out).unwrap(), ("Hi".to_owned(), 2));
    let bad = RpcClient::new("http://h")
      .call_unbatched("A.b", ("x".to_owned(), 1_i64), &payload, &["one"], &success)
      .provide_all(hlist![HttpClientService(client as Arc<dyn HttpClient>)])
      .block_on()
      .into_result();
    assert!(matches!(typed(bad), Err(RpcClientError::Serialization(_))));
  }
}

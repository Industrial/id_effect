//! ai.llm #5: the `Embedder` capability against wiremock OpenAI and Ollama servers (no live
//! vendor call, D8): one response shape, bounded batches split in order, typed dimension
//! errors, retries and spans as for chat, and input text never recorded (R3).
#![cfg(all(feature = "embeddings", feature = "openai", feature = "ollama"))]

use base64::{Engine as _, engine::general_purpose::STANDARD};
use id_effect::{
  Effect, HCons, HNil, TracingConfig, install_tracing_layer, snapshot_tracing, tracing_hook,
};
use id_effect_ai::testing::ScriptedModel;
use id_effect_ai::{
  AI_SPAN, AiConfig, AiError, AiLimits, EmbedRequest, EmbedResponse, Embedder, EmbedderService,
  Embedding, EmbeddingError, EmbeddingLimits, OllamaEmbedder, OpenAiEmbedder, Usage,
  cosine_similarity, embed, provide_ollama_embedder, provide_openai_embedder,
};
use id_effect_platform::http::HttpClientConfig;
use id_effect_tokio::TokioRuntime;
use serde_json::{Value, json};
use wiremock::matchers::{any, body_partial_json, header, method, path};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

const KEY: &str = "sk-embed-test";

/// OpenAI and Ollama both at `uri` (the local mock server, allowed by the SSRF policy).
fn config(uri: &str) -> AiConfig {
  AiConfig::default()
    .with_openai_api_key(KEY)
    .with_openai_base_url(uri)
    .with_ollama_base_url(uri)
    .with_http(HttpClientConfig::default().allow_hosts(["127.0.0.1"]))
}

async fn run<A: Send + 'static>(eff: Effect<A, EmbeddingError>) -> Result<A, EmbeddingError> {
  TokioRuntime::current()
    .expect("tokio")
    .run(eff)
    .await
    .into_result()
    .map_err(|c| c.fail_option().expect("typed failure"))
}

fn b64(v: &[f32]) -> String {
  STANDARD.encode(v.iter().flat_map(|x| x.to_le_bytes()).collect::<Vec<u8>>())
}

fn inputs_of(req: &Request) -> Vec<String> {
  let body: Value = serde_json::from_slice(&req.body).expect("json body");
  serde_json::from_value(body["input"].clone()).expect("input array")
}

/// Answers every input with `[len(text), 1]`, in the vendor's own format; usage counts one
/// token per input.
struct LengthVectors {
  openai: bool,
}

impl Respond for LengthVectors {
  fn respond(&self, req: &Request) -> ResponseTemplate {
    let inputs = inputs_of(req);
    let vectors: Vec<Vec<f32>> = inputs.iter().map(|t| vec![t.len() as f32, 1.0]).collect();
    let body = if self.openai {
      json!({
        "data": vectors.iter().enumerate().map(|(i, v)| json!({"index": i, "embedding": b64(v)})).collect::<Vec<_>>(),
        "usage": {"prompt_tokens": inputs.len(), "total_tokens": inputs.len()}
      })
    } else {
      json!({"model": "m", "embeddings": vectors, "prompt_eval_count": inputs.len()})
    };
    ResponseTemplate::new(200).set_body_json(body)
  }
}

#[tokio::test]
async fn embeddings_openai_and_ollama_same_shape() {
  let server = MockServer::start().await;
  // OpenAI answers base64 vectors, out of order; Ollama float arrays.
  Mock::given(method("POST"))
    .and(path("/v1/embeddings"))
    .and(header("Authorization", format!("Bearer {KEY}").as_str()))
    .and(body_partial_json(json!({
      "model": "text-embedding-3-small",
      "input": ["alpha", "beta"],
      "encoding_format": "base64"
    })))
    .respond_with(ResponseTemplate::new(200).set_body_json(json!({
      "object": "list",
      "data": [
        {"object": "embedding", "index": 1, "embedding": b64(&[0.0, 1.0, 0.0])},
        {"object": "embedding", "index": 0, "embedding": b64(&[1.0, 0.0, 0.0])}
      ],
      "usage": {"prompt_tokens": 4, "total_tokens": 4}
    })))
    .expect(1)
    .mount(&server)
    .await;
  Mock::given(method("POST"))
    .and(path("/api/embed"))
    .and(body_partial_json(
      json!({"model": "nomic-embed-text", "input": ["alpha", "beta"]}),
    ))
    .respond_with(ResponseTemplate::new(200).set_body_json(json!({
      "model": "nomic-embed-text",
      "embeddings": [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0]],
      "prompt_eval_count": 4
    })))
    .expect(1)
    .mount(&server)
    .await;
  let cfg = config(&server.uri());
  let request = |model: &str| EmbedRequest::new(model, ["alpha", "beta"]);

  let openai = run(
    embed::<HCons<EmbedderService, HNil>, _>(request("text-embedding-3-small")).provide_layer(
      provide_openai_embedder::<EmbeddingError>(cfg.http_client(), cfg.clone()).expect("openai"),
    ),
  )
  .await
  .expect("openai embed");
  let ollama = run(
    embed::<HCons<EmbedderService, HNil>, _>(request("nomic-embed-text")).provide_layer(
      provide_ollama_embedder::<EmbeddingError>(cfg.http_client(), cfg.clone()),
    ),
  )
  .await
  .expect("ollama embed");

  let expected = EmbedResponse::new(vec![
    Embedding::new(vec![1.0, 0.0, 0.0]),
    Embedding::new(vec![0.0, 1.0, 0.0]),
  ])
  .with_usage(Usage::new(4, 0));
  assert_eq!(openai, expected);
  assert_eq!(ollama, expected);
  assert_eq!(openai.dimensions(), Some(3));

  // The test double answers in the same shape, deterministically.
  let scripted = run(ScriptedModel::new().embed(request("m").with_dimensions(3)))
    .await
    .expect("scripted");
  assert_eq!(scripted.embeddings.len(), 2);
  assert_eq!(scripted.dimensions(), Some(3));
  assert_eq!(
    scripted.embeddings[0],
    ScriptedModel::embedding_of("alpha", 3)
  );
}

#[tokio::test]
async fn embedding_batch_over_cap_split() {
  let texts = ["a", "bb", "ccc", "dddd", "eeeee"];
  let server = MockServer::start().await;
  Mock::given(method("POST"))
    .and(path("/v1/embeddings"))
    .respond_with(LengthVectors { openai: true })
    .expect(3)
    .mount(&server)
    .await;
  Mock::given(method("POST"))
    .and(path("/api/embed"))
    .respond_with(LengthVectors { openai: false })
    .expect(2)
    .mount(&server)
    .await;
  let cfg = config(&server.uri());
  let openai = OpenAiEmbedder::from_config(&cfg)
    .expect("openai")
    .with_limits(EmbeddingLimits::default().with_max_batch_size(2));
  let ollama = OllamaEmbedder::from_config(&cfg)
    .with_limits(EmbeddingLimits::default().with_max_batch_size(3));

  let embedders: [&dyn Embedder; 2] = [&openai, &ollama];
  for embedder in embedders {
    let resp = run(embedder.embed(EmbedRequest::new("m", texts)))
      .await
      .expect("split embed");
    let lengths: Vec<f32> = resp.embeddings.iter().map(|e| e.as_slice()[0]).collect();
    assert_eq!(lengths, vec![1.0, 2.0, 3.0, 4.0, 5.0], "input order kept");
    assert_eq!(resp.usage, Usage::new(5, 0), "usage summed over batches");
  }

  // The vendor saw batches of at most the cap, in order.
  let batches: Vec<(String, Vec<String>)> = server
    .received_requests()
    .await
    .expect("recorded")
    .iter()
    .map(|r| (r.url.path().to_string(), inputs_of(r)))
    .collect();
  let sizes = |p: &str| -> Vec<usize> {
    batches
      .iter()
      .filter(|(path, _)| path == p)
      .map(|(_, inputs)| inputs.len())
      .collect()
  };
  assert_eq!(sizes("/v1/embeddings"), vec![2, 2, 1]);
  assert_eq!(sizes("/api/embed"), vec![3, 2]);
  let sent: Vec<String> = batches
    .iter()
    .filter(|(path, _)| path == "/v1/embeddings")
    .flat_map(|(_, inputs)| inputs.clone())
    .collect();
  assert_eq!(sent, texts);

  // Over the input cap nothing is sent.
  let capped =
    OllamaEmbedder::from_config(&cfg).with_limits(EmbeddingLimits::default().with_max_inputs(4));
  assert_eq!(
    run(capped.embed(EmbedRequest::new("m", texts))).await,
    Err(EmbeddingError::Ai(AiError::LimitExceeded {
      limit: "embedding_inputs",
      max: 4
    }))
  );
  assert_eq!(
    EmbeddingLimits::default().max_batch_size,
    EmbeddingLimits::DEFAULT_MAX_BATCH_SIZE
  );
}

#[tokio::test]
async fn embedding_dimension_mismatch_is_typed_error() {
  let server = MockServer::start().await;
  // Asked for 3 dimensions, the server answers 2.
  Mock::given(method("POST"))
    .and(path("/v1/embeddings"))
    .and(body_partial_json(json!({"dimensions": 3})))
    .respond_with(ResponseTemplate::new(200).set_body_json(json!({
      "data": [{"index": 0, "embedding": [0.5, 0.5]}]
    })))
    .expect(1)
    .mount(&server)
    .await;
  // A ragged response, and one vector short.
  Mock::given(method("POST"))
    .and(path("/api/embed"))
    .and(body_partial_json(json!({"model": "ragged"})))
    .respond_with(
      ResponseTemplate::new(200)
        .set_body_json(json!({"embeddings": [[1.0, 2.0], [1.0, 2.0, 3.0]]})),
    )
    .mount(&server)
    .await;
  Mock::given(method("POST"))
    .and(path("/api/embed"))
    .and(body_partial_json(json!({"model": "short"})))
    .respond_with(ResponseTemplate::new(200).set_body_json(json!({"embeddings": [[1.0]]})))
    .mount(&server)
    .await;
  let cfg = config(&server.uri());
  let openai = OpenAiEmbedder::from_config(&cfg).expect("openai");
  let ollama = OllamaEmbedder::from_config(&cfg);

  assert_eq!(
    run(openai.embed(EmbedRequest::one("text-embedding-3-small", "x").with_dimensions(3))).await,
    Err(EmbeddingError::DimensionMismatch {
      expected: 3,
      actual: 2
    })
  );
  assert_eq!(
    run(ollama.embed(EmbedRequest::new("ragged", ["a", "b"]))).await,
    Err(EmbeddingError::DimensionMismatch {
      expected: 2,
      actual: 3
    })
  );
  assert_eq!(
    run(ollama.embed(EmbedRequest::new("short", ["a", "b"]))).await,
    Err(EmbeddingError::CountMismatch {
      sent: 2,
      received: 1
    })
  );
  // Similarity never compares vectors of different lengths.
  let a = Embedding::new(vec![1.0, 0.0]);
  let b = Embedding::new(vec![1.0, 0.0, 0.0]);
  assert_eq!(
    a.cosine(&b),
    Err(EmbeddingError::DimensionMismatch {
      expected: 2,
      actual: 3
    })
  );
  assert!(matches!(
    cosine_similarity(b.as_slice(), a.as_slice()),
    Err(EmbeddingError::DimensionMismatch { .. })
  ));
}

#[tokio::test]
async fn embedding_input_not_logged() {
  const SECRET: &str = "patient 4711 has condition X";
  let server = MockServer::start().await;
  Mock::given(method("POST"))
    .and(path("/v1/embeddings"))
    .respond_with(LengthVectors { openai: true })
    .mount(&server)
    .await;
  Mock::given(method("POST"))
    .and(path("/api/embed"))
    .respond_with(LengthVectors { openai: false })
    .mount(&server)
    .await;
  // Even with the prompt-logging opt-out taken, embedding input is never recorded.
  let cfg = config(&server.uri()).dangerous_log_prompts();
  let openai = OpenAiEmbedder::from_config(&cfg).expect("openai");
  let ollama = OllamaEmbedder::from_config(&cfg);
  let req = EmbedRequest::new("embed-model", [SECRET]);

  // One traced call per vendor (the tracing buffer keeps the latest `ai.request` span).
  for (vendor, embedder) in [("openai", &openai as &dyn Embedder), ("ollama", &ollama)] {
    install_tracing_layer(TracingConfig::enabled());
    run(embedder.embed(req.clone()).with_span_hook(tracing_hook()))
      .await
      .expect("embed");
    let snap = snapshot_tracing();
    install_tracing_layer(TracingConfig::default());
    let span = snap
      .spans
      .iter()
      .find(|s| s.name == AI_SPAN)
      .expect("ai.request span");
    let get = |k: &str| span.annotations.get(k).map(String::as_str);
    assert_eq!(get("ai.vendor"), Some(vendor));
    assert_eq!(get("ai.operation"), Some("embed"));
    assert_eq!(get("ai.model"), Some("embed-model"));
    assert_eq!(get("ai.embedding.inputs"), Some("1"));
    assert_eq!(get("ai.embedding.dimensions"), Some("2"));
    assert_eq!(get("ai.usage.input_tokens"), Some("1"));
    assert!(!span.annotations.contains_key("ai.prompt"));
    for s in &snap.spans {
      for (key, value) in &s.annotations {
        assert!(
          !value.contains(SECRET),
          "{vendor}: input recorded under {key}"
        );
      }
    }
  }
  // Nor in Debug output or validation errors.
  assert!(!format!("{req:?}").contains(SECRET));
  let invalid = EmbedRequest::new("m", [SECRET, ""]);
  let err = run(openai.embed(invalid)).await.unwrap_err();
  assert!(!err.to_string().contains(SECRET), "{err}");
}

#[tokio::test]
async fn embedding_transient_status_retried_and_failures_typed() {
  let server = MockServer::start().await;
  Mock::given(method("POST"))
    .and(path("/v1/embeddings"))
    .respond_with(ResponseTemplate::new(503).set_body_string("busy"))
    .up_to_n_times(1)
    .with_priority(1)
    .mount(&server)
    .await;
  Mock::given(method("POST"))
    .and(path("/v1/embeddings"))
    .respond_with(LengthVectors { openai: true })
    .with_priority(2)
    .mount(&server)
    .await;
  Mock::given(method("POST"))
    .and(path("/api/embed"))
    .respond_with(ResponseTemplate::new(401))
    .mount(&server)
    .await;
  let cfg = config(&server.uri());
  let resp = run(
    OpenAiEmbedder::from_config(&cfg)
      .expect("openai")
      .embed(EmbedRequest::one("m", "abc")),
  )
  .await
  .expect("retried after 503");
  assert_eq!(resp.embeddings[0].as_slice(), &[3.0, 1.0]);
  assert_eq!(
    run(OllamaEmbedder::from_config(&cfg).embed(EmbedRequest::one("m", "abc"))).await,
    Err(EmbeddingError::Ai(AiError::Unauthorized))
  );
  // The response cap applies to embeddings too.
  let small = cfg.with_limits(AiLimits::default().with_max_response_bytes(8));
  assert!(matches!(
    run(
      OpenAiEmbedder::from_config(&small)
        .expect("openai")
        .embed(EmbedRequest::one("m", "abc"))
    )
    .await,
    Err(EmbeddingError::Ai(AiError::LimitExceeded { .. }))
  ));
}

#[tokio::test]
async fn embedding_base_url_obeys_ssrf_policy() {
  let server = MockServer::start().await;
  Mock::given(any())
    .respond_with(ResponseTemplate::new(200))
    .expect(0)
    .mount(&server)
    .await;
  // The default policy: loopback refused before anything is sent.
  let cfg = AiConfig::default()
    .with_openai_api_key(KEY)
    .with_openai_base_url(server.uri())
    .with_ollama_base_url(server.uri());
  let req = EmbedRequest::one("m", "x");
  for embedder in [
    &OpenAiEmbedder::from_config(&cfg).expect("openai") as &dyn Embedder,
    &OllamaEmbedder::from_config(&cfg),
  ] {
    assert!(matches!(
      run(embedder.embed(req.clone())).await,
      Err(EmbeddingError::Ai(AiError::Blocked(_)))
    ));
  }
}

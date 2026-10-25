# Embeddings and in-app similarity

An embedding turns a text into a vector of numbers so that texts with similar meaning land close
together. `id_effect_ai` (features `embeddings` plus `openai` and/or `ollama`) treats the
embedding model like the chat model: `Embedder` is a capability in the typed environment, the
vendor is a layer chosen at the edge, every request goes through the platform HTTP client, and
tests use the deterministic `ScriptedModel`.

This chapter does **not** add a vector database. The pattern here is in-app similarity over a
bounded set: a few hundred or a few thousand vectors the application already owns (labelled
examples, FAQ entries, product categories), kept in memory or in an ordinary table, ranked with a
linear scan. When the set outgrows that, the vectors belong in a store built for it; the
`Embedder` capability stays the same.

| Feature | What it adds |
|---------|--------------|
| `embeddings` | `Embedder`, `EmbedRequest`, `EmbedResponse`, `Embedding`, `EmbeddingError`, `EmbeddingLimits`, `cosine_similarity`, `dot_product`, `top_k_similar` |
| `embeddings` + `openai` | `OpenAiEmbedder` (`/v1/embeddings`, also OpenAI-compatible servers) |
| `embeddings` + `ollama` | `OllamaEmbedder` (`/api/embed` on a self-hosted Ollama) |
| `embeddings` + `test-util` | deterministic embeddings from `testing::ScriptedModel` |

## One shape for every provider

An `EmbedRequest` is a model id and a list of input texts; the answer is an `EmbedResponse`: one
`Embedding` per input, **in input order**, and the token `usage`. OpenAI and Ollama return
different JSON (OpenAI base64-packed vectors with an `index`, Ollama float arrays), and both
adapters turn it into the same value, so code written against `Embedder` does not know which
vendor it talks to.

A program asks for `EmbedderService` and calls `embed::<R, _>(req)`, which returns an
`Effect<EmbedResponse, EmbeddingError, R>`. In tests and examples the provider is
`ScriptedModel`, whose embeddings are a deterministic hash of the words and character
trigrams of each text: the same text always gives the same vector, and texts that share words
are closer than texts that share none.

```rust
use id_effect::{Effect, HCons, HNil};
use id_effect_ai::testing::ScriptedModel;
use id_effect_ai::{EmbedRequest, EmbedderService, EmbeddingError, embed, top_k_similar};

type Env = HCons<EmbedderService, HNil>;

const CATEGORIES: [&str; 3] = [
  "refund request for a damaged order",
  "password reset and login problems",
  "question about an invoice or billing",
];

/// The category closest to `ticket`: embed the categories and the ticket in one request,
/// then rank the categories by cosine similarity to the ticket.
fn route(ticket: &str) -> Effect<&'static str, EmbeddingError, Env> {
  let inputs = CATEGORIES.iter().copied().chain([ticket]);
  let req = EmbedRequest::new("text-embedding-3-small", inputs).with_dimensions(256);
  embed::<Env, _>(req).flat_map(|resp| {
    let (categories, ticket) = resp.embeddings.split_at(CATEGORIES.len());
    match top_k_similar(&ticket[0], categories, 1) {
      Ok(best) => id_effect::succeed(CATEGORIES[best[0].0]),
      Err(e) => id_effect::fail(e),
    }
  })
}

let model = ScriptedModel::new();
let category = route("I cannot login, the password reset mail never arrives")
  .provide_layer(model.embedder_layer::<EmbeddingError>())
  .block_on()
  .into_result()
  .expect("route");
assert_eq!(category, "password reset and login problems");
```

In a real application the category vectors are computed once (at start-up, or when the set
changes) and kept; only the ticket is embedded per request.

## Vendors at the edge

`OpenAiEmbedder` needs `OPENAI_API_KEY`; `OllamaEmbedder` needs no key, but its default base URL
(`http://localhost:11434`) is a loopback address, which the platform client's SSRF policy refuses
until the host is allowed by name:

```rust,no_run
use id_effect_ai::{AiConfig, AiError, EmbeddingError, provide_ollama_embedder, provide_openai_embedder};
use id_effect_platform::http::HttpClientConfig;

let cfg = AiConfig::from_env();
let openai = provide_openai_embedder::<EmbeddingError>(cfg.http_client(), cfg.clone())?;

let local = AiConfig::default()
  .with_ollama_base_url("http://ollama.internal:11434")
  .with_http(HttpClientConfig::default().allow_hosts(["ollama.internal"]));
let ollama = provide_ollama_embedder::<EmbeddingError>(local.http_client(), local);
# let _ = (openai, ollama);
# Ok::<(), AiError>(())
```

Ollama's chat speaks the OpenAI wire format, so a self-hosted setup pairs `OllamaEmbedder` with
`OpenAiLanguageModel` pointed at the same host. Anthropic serves no embeddings endpoint: pair
Claude chat with either embedder.

## Bounded batches

Vendors cap the inputs of one call, and a large batch makes a large response. `Embedder::embed`
applies `EmbeddingLimits`:

- more than `max_inputs` inputs (default 4096) fail with
  `AiError::LimitExceeded { limit: "embedding_inputs", .. }` before anything is sent;
- the rest are split into vendor calls of at most `max_batch_size` inputs (default 64; OpenAI's
  own cap of 2048 is never exceeded), run in order. The results are concatenated in input order
  and their usage is summed, so the caller sees one response.

```rust
use id_effect_ai::testing::ScriptedModel;
use id_effect_ai::{AiError, EmbedRequest, Embedder, EmbeddingError, EmbeddingLimits};

let texts: Vec<String> = (0..10).map(|i| format!("document {i}")).collect();
let resp = ScriptedModel::new()
  .embed(EmbedRequest::new("m", texts.clone()))
  .block_on()
  .into_result()
  .expect("embed");
assert_eq!(resp.embeddings.len(), 10);

let limits = EmbeddingLimits::default().with_max_batch_size(16).with_max_inputs(5);
assert_eq!((limits.max_batch_size, limits.max_inputs), (16, 5));
// `OpenAiEmbedder::with_limits(limits)` / `OllamaEmbedder::with_limits(limits)` apply them;
// a request over `max_inputs` is refused without a vendor call:
let refused = EmbeddingError::from(AiError::LimitExceeded { limit: "embedding_inputs", max: 5 });
assert!(refused.to_string().contains("embedding_inputs"));
```

## Dimensions are checked, never guessed

Vectors from different models, or from one model asked for different sizes, cannot be compared.
`EmbedRequest::with_dimensions(n)` asks the vendor for `n` components (OpenAI `text-embedding-3`
and Ollama shorten their vectors to it) and makes `n` the expected length. Every vector of a
response, across all its batches, must have one length, `n` when it was set; anything else is
`EmbeddingError::DimensionMismatch { expected, actual }`. A vendor that answers with fewer or more
vectors than inputs is `EmbeddingError::CountMismatch`.

The similarity helpers check too: `cosine_similarity`, `dot_product` and `top_k_similar` refuse
vectors of different lengths instead of comparing a prefix.

```rust
use id_effect_ai::{Embedding, EmbeddingError, cosine_similarity, dot_product};

let a = Embedding::new(vec![1.0, 0.0]);
let b = Embedding::new(vec![0.6, 0.8]);
assert!((a.cosine(&b).expect("same length") - 0.6).abs() < 1e-6);
assert_eq!(dot_product(&[1.0, 2.0], &[3.0, 4.0]), Ok(11.0));
assert_eq!(
  cosine_similarity(&[1.0, 0.0], &[1.0, 0.0, 0.0]),
  Err(EmbeddingError::DimensionMismatch { expected: 2, actual: 3 })
);
```

Store the model id and the dimension next to every vector you keep, and re-embed the set when
either changes.

## Retries, spans and privacy

Embedding calls behave like chat calls: 429/502/503/504 are retried with backoff, 401/403 are
`AiError::Unauthorized`, the response is capped by `AiLimits::max_response_bytes` and the
request by `AiLimits::request_timeout`. Each vendor call runs in an `ai.request` span with
`ai.vendor`, `ai.operation = "embed"`, `ai.model`, `ai.embedding.inputs`,
`ai.embedding.dimensions` and `ai.usage.input_tokens`.

**Input text is never recorded.** Not on a span (even when `AiConfig::dangerous_log_prompts` is
taken for chat), not in `Debug` of an `EmbedRequest` (it prints `<N inputs redacted>`), not in a
validation error (an empty input is named by its position). Embedding inputs are often whole
documents, customer messages or records; they stay out of the trace backend.

## In the Pipeline

The Pipeline reference app (ADR 0015 D1) uses embeddings as one stage of a typed effect pipeline:
a stage that needs `EmbedderService` in its environment, so the type says it calls a model and the
edge decides which one. The stage embeds the incoming item, ranks it against the bounded set of
labelled examples it loaded at start-up (`top_k_similar`), and passes the label and score on;
below a threshold the item goes to a human instead. Tests provide `ScriptedModel` and assert the
routing with no vendor and no network.

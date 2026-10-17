//! `S3Store` against a real S3-compatible server: the `id_effect_test` [`S3`] container
//! (SeaweedFS, pinned by digest — release decision D11). Round trip, listing and delete, a
//! streamed multipart upload larger than a part, and presigned GET / PUT URLs used with a plain
//! HTTP client until they expire (web.storage#3, #5, #7).
//!
//! Without Docker every test prints `SKIP` and passes, unless `ID_EFFECT_REQUIRE_DOCKER=1` (the
//! CI `containers-integration` job), where a missing Docker daemon fails it.

use std::collections::BTreeSet;
use std::time::Duration;

use bytes::Bytes;
use id_effect::{Cause, Effect, Exit, HNil, Stream};
use id_effect_storage::{
  ObjectKey, ObjectStorageEnv, ObjectStore, S3Config, S3Store, SignedMethod, StorageError, Url,
  delete, exists, get, get_stream, head, list, put_stream, signed_url,
};
use id_effect_test::{ContainerError, ContainerImage, S3, S3Container, require_docker};
use id_effect_tokio::TokioRuntime;
use object_store::aws::AmazonS3Builder;
use object_store::path::Path;
use object_store::signer::{Method, Signer};
use reqwest::StatusCode;

async fn run<A, E>(eff: Effect<A, E>) -> Result<A, E>
where
  A: Send + 'static,
  E: Send + std::fmt::Debug + 'static,
{
  match TokioRuntime::current().expect("tokio").run(eff).await {
    Exit::Success(a) => Ok(a),
    Exit::Failure(Cause::Fail(e)) => Err(e),
    Exit::Failure(c) => panic!("unexpected cause: {c:?}"),
  }
}

async fn ok<A: Send + 'static>(eff: Effect<A, StorageError>) -> A {
  run(eff).await.expect("storage effect failed")
}

fn key(k: &str) -> ObjectKey {
  ObjectKey::new(k).unwrap()
}

async fn collect(stream: Stream<Bytes, StorageError>) -> Result<Vec<Bytes>, StorageError> {
  run(stream.run_collect()).await
}

async fn listed(store: &S3Store, prefix: Option<&str>) -> BTreeSet<String> {
  let prefix = prefix.map(key);
  ok(store.list(prefix.as_ref()).run_collect())
    .await
    .into_iter()
    .map(|m| m.key.to_string())
    .collect()
}

/// A started S3 server with the bucket `bucket`, or `None` (after printing SKIP) without Docker.
async fn server(bucket: &str) -> Option<S3Container> {
  if !require_docker() {
    return None;
  }
  let s3 = run(S3::new().start::<ContainerError, HNil>())
    .await
    .expect("the S3 container starts");
  create_bucket(&s3, bucket).await;
  Some(s3)
}

/// `PUT /<bucket>` (CreateBucket), presigned with object_store's SigV4 signer: `S3Store` reads
/// and writes objects, it never creates buckets.
async fn create_bucket(s3: &S3Container, bucket: &str) {
  let signer = AmazonS3Builder::new()
    .with_endpoint(s3.endpoint())
    .with_allow_http(true)
    .with_bucket_name(bucket)
    .with_region("us-east-1")
    .with_access_key_id(s3.access_key_id())
    .with_secret_access_key(s3.secret_access_key())
    .build()
    .unwrap();
  let url = signer
    .signed_url(Method::PUT, &Path::from(""), Duration::from_secs(60))
    .await
    .unwrap();
  let created = reqwest::Client::new().put(url).send().await.unwrap();
  assert!(
    created.status().is_success(),
    "create bucket {bucket}: {} {}",
    created.status(),
    created.text().await.unwrap_or_default()
  );
}

fn config(s3: &S3Container, bucket: &str) -> S3Config {
  S3Config::new(bucket)
    .with_endpoint(s3.endpoint())
    .with_allow_http(true)
    .with_credentials(s3.access_key_id(), s3.secret_access_key())
}

async fn remove(s3: S3Container) {
  run(s3.container().remove::<ContainerError, HNil>())
    .await
    .expect("remove the S3 container");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn s3_store_round_trips_lists_and_deletes() {
  let Some(s3) = server("statements").await else {
    return;
  };
  let store = S3Store::new(&config(&s3, "statements")).unwrap();
  let a = key("docs/a.txt");

  // Missing objects.
  assert!(!ok(store.exists(&a)).await);
  assert!(
    matches!(run(store.get(&a)).await, Err(StorageError::NotFound { key }) if key == "docs/a.txt")
  );
  assert!(matches!(
    run(store.head(&a)).await,
    Err(StorageError::NotFound { .. })
  ));
  assert!(matches!(
    collect(store.get_stream(&a)).await,
    Err(StorageError::NotFound { .. })
  ));
  ok(store.delete(&a)).await;

  // Put, read back, replace.
  let put = ok(store.put(&a, Bytes::from_static(b"alpha"))).await;
  assert_eq!((put.key.as_str(), put.size), ("docs/a.txt", 5));
  assert!(put.e_tag.is_some(), "S3 returns an entity tag");
  assert!(ok(store.exists(&a)).await);
  assert_eq!(ok(store.get(&a)).await, "alpha");
  let meta = ok(store.head(&a)).await;
  assert_eq!((meta.key.clone(), meta.size), (a.clone(), 5));
  assert_eq!(meta.e_tag, put.e_tag);
  ok(store.put(&a, Bytes::from_static(b"beta"))).await;
  assert_eq!(
    collect(store.get_stream(&a)).await.unwrap().concat(),
    b"beta"
  );

  // A short streamed body is one PUT; an empty one an empty object.
  let small = key("docs/small.bin");
  let body = Stream::from_vec(vec![Bytes::from_static(b"sm"), Bytes::from_static(b"all")]);
  assert_eq!(ok(store.put_stream(&small, body)).await.size, 5);
  assert_eq!(ok(store.get(&small)).await, "small");
  let empty = key("docs/empty");
  assert_eq!(ok(store.put_stream(&empty, Stream::empty())).await.size, 0);
  assert_eq!(ok(store.get(&empty)).await, "");

  // Keys a URL must escape round-trip, listing included.
  let odd = key("odd/a#1 b%c ü?.txt");
  ok(store.put(&odd, Bytes::from_static(b"odd"))).await;
  assert_eq!(ok(store.get(&odd)).await, "odd");

  // Listing: whole-segment prefixes, recursive.
  ok(store.put(&key("docsx/c"), Bytes::from_static(b"c"))).await;
  ok(store.put(&key("docs/sub/d"), Bytes::from_static(b"d"))).await;
  assert_eq!(
    listed(&store, Some("docs")).await,
    ["docs/a.txt", "docs/empty", "docs/small.bin", "docs/sub/d"]
      .map(String::from)
      .into()
  );
  let all = listed(&store, None).await;
  assert_eq!(all.len(), 6, "{all:?}");
  assert!(all.contains(odd.as_str()));
  assert!(listed(&store, Some("nothing")).await.is_empty());

  // Delete is idempotent.
  ok(store.delete(&a)).await;
  assert!(!ok(store.exists(&a)).await);
  ok(store.delete(&a)).await;
  ok(store.delete(&key("docs/sub/d"))).await;
  assert_eq!(
    listed(&store, Some("docs")).await,
    ["docs/empty", "docs/small.bin"].map(String::from).into()
  );
  remove(s3).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_streamed_upload_larger_than_a_part_is_a_multipart_upload() {
  let Some(s3) = server("uploads").await else {
    return;
  };
  let store = S3Store::new(&config(&s3, "uploads"))
    .unwrap()
    .with_part_size(S3Store::MIN_PART_SIZE);
  // 12 MiB in 256 KiB items: parts of 5, 5 and 2 MiB.
  let item = 256 * 1024;
  let items: Vec<Bytes> = (0..48_u8).map(|i| Bytes::from(vec![i; item])).collect();
  let size = (48 * item) as u64;
  assert!(size > 2 * S3Store::MIN_PART_SIZE as u64);
  let k = key("exports/2026-10.bin");

  let put = ok(store.put_stream(&k, Stream::from_vec(items))).await;
  assert_eq!(put.size, size);
  let e_tag = put.e_tag.expect("an entity tag");
  assert!(
    e_tag.trim_matches('"').ends_with("-3"),
    "a three-part multipart upload's entity tag: {e_tag}"
  );
  assert_eq!(ok(store.head(&k)).await.size, size);

  // Streamed back in network chunks, never as one buffer, byte for byte.
  let chunks = collect(store.get_stream(&k)).await.unwrap();
  assert!(chunks.len() > 1, "one chunk: the download was buffered");
  let mut offset = 0;
  for chunk in &chunks {
    for (i, byte) in chunk.iter().enumerate() {
      assert_eq!(
        usize::from(*byte),
        (offset + i) / item,
        "byte {}",
        offset + i
      );
    }
    offset += chunk.len();
  }
  assert_eq!(offset as u64, size);

  // A body that fails mid-upload aborts it and leaves no object.
  let broken = key("exports/broken.bin");
  let body = Stream::from_vec(vec![Bytes::from(vec![0_u8; S3Store::MIN_PART_SIZE + 1])]).chain(
    Stream::fail(StorageError::Backend {
      message: "client went away".into(),
    }),
  );
  assert!(matches!(
    run(store.put_stream(&broken, body)).await,
    Err(StorageError::Backend { message }) if message == "client went away"
  ));
  assert!(!ok(store.exists(&broken)).await);
  remove(s3).await;
}

/// The URL with its `X-Amz-Signature` changed.
fn tampered(url: &Url) -> Url {
  let pairs: Vec<(String, String)> = url
    .query_pairs()
    .into_owned()
    .map(|(k, v)| {
      if k == "X-Amz-Signature" {
        let flipped = if v.starts_with('0') { "1" } else { "0" };
        (k, format!("{flipped}{}", &v[1..]))
      } else {
        (k, v)
      }
    })
    .collect();
  let mut url = url.clone();
  url.query_pairs_mut().clear().extend_pairs(pairs);
  url
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn presigned_urls_work_without_credentials_and_expire() {
  let Some(s3) = server("shared").await else {
    return;
  };
  let store = S3Store::new(&config(&s3, "shared")).unwrap();
  let http = reqwest::Client::new();
  let k = key("reports/q3 summary.pdf");
  let minute = Duration::from_secs(60);

  // A presigned PUT uploads without credentials.
  let put_url = ok(store.signed_url(SignedMethod::Put, &k, minute)).await;
  let uploaded = http
    .put(put_url.clone())
    .body("quarterly")
    .send()
    .await
    .unwrap();
  assert_eq!(uploaded.status(), StatusCode::OK);
  assert_eq!(ok(store.get(&k)).await, "quarterly");

  // A presigned GET downloads it; the bare URL, a tampered signature or another method do not.
  let get_url = ok(store.signed_url(SignedMethod::Get, &k, minute)).await;
  let got = http.get(get_url.clone()).send().await.unwrap();
  assert_eq!(got.status(), StatusCode::OK);
  assert_eq!(got.bytes().await.unwrap(), "quarterly");
  let mut bare = get_url.clone();
  bare.set_query(None);
  assert_eq!(
    http.get(bare).send().await.unwrap().status(),
    StatusCode::FORBIDDEN
  );
  assert_eq!(
    http.get(tampered(&get_url)).send().await.unwrap().status(),
    StatusCode::FORBIDDEN
  );
  let wrong_method = http.put(get_url).body("overwrite").send().await.unwrap();
  assert_eq!(wrong_method.status(), StatusCode::FORBIDDEN);
  assert_eq!(ok(store.get(&k)).await, "quarterly");

  // Through the capability, and expiring: valid at first, refused once its second has passed.
  let env = || id_effect_storage::object_storage_env(store.clone());
  let short = ok(
    signed_url::<ObjectStorageEnv, _>(SignedMethod::Get, k.clone(), Duration::from_secs(1))
      .provide_all(env()),
  )
  .await;
  assert_eq!(
    http.get(short.clone()).send().await.unwrap().status(),
    StatusCode::OK
  );
  tokio::time::sleep(Duration::from_secs(3)).await;
  assert_eq!(
    http.get(short).send().await.unwrap().status(),
    StatusCode::FORBIDDEN,
    "an expired presigned URL is refused"
  );
  remove(s3).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_layer_provides_the_capability_and_bad_credentials_are_denied() {
  let Some(s3) = server("layered").await else {
    return;
  };
  let k = key("in/layer.txt");
  let body = Stream::from_vec(vec![
    Bytes::from_static(b"via "),
    Bytes::from_static(b"layer"),
  ]);
  let program = put_stream::<ObjectStorageEnv, _>(k.clone(), body)
    .flat_map({
      let k = k.clone();
      move |_| get(k)
    })
    .flat_map({
      let k = k.clone();
      move |body| head(k).map(move |meta| (body, meta.size))
    })
    .flat_map({
      let k = k.clone();
      move |got| get_stream(k).map(move |stream| (got, stream))
    });
  let ((body, size), stream) =
    ok(program.provide_layer(S3Store::layer(config(&s3, "layered")))).await;
  assert_eq!((body, size), (Bytes::from_static(b"via layer"), 9));
  assert_eq!(collect(stream).await.unwrap().concat(), b"via layer");

  let listed = ok(
    list::<ObjectStorageEnv, _>(Some(key("in")))
      .flat_map(|stream| stream.run_collect().narrow())
      .provide_layer(S3Store::layer(config(&s3, "layered"))),
  )
  .await;
  assert_eq!(listed.len(), 1);
  let removed = delete::<ObjectStorageEnv, _>(k.clone()).flat_map({
    let k = k.clone();
    move |()| exists(k)
  });
  assert!(!ok(removed.provide_layer(S3Store::layer(config(&s3, "layered")))).await);

  let wrong =
    S3Store::new(&config(&s3, "layered").with_credentials(s3.access_key_id(), "not-the-secret"))
      .unwrap();
  assert!(
    matches!(
      run(wrong.put(&k, Bytes::from_static(b"x"))).await,
      Err(StorageError::PermissionDenied { .. })
    ),
    "a wrong secret is refused"
  );
  remove(s3).await;
}

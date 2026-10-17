//! The in-memory and local file-system backends behave the same through the `ObjectStore`
//! capability (web.storage#1, #2, #5, #7, #8).

use std::collections::BTreeSet;
use std::time::Duration;

use bytes::Bytes;
use id_effect::{Cause, Effect, Exit, Stream};
use id_effect_storage::{
  LocalStore, MemoryStore, ObjectKey, ObjectMeta, ObjectStorage, ObjectStorageEnv, ObjectStore,
  PutOutcome, SignedMethod, StorageError, Url, UrlSigner, delete, exists, get, get_stream, head,
  list, object_storage_env, provide_object_storage, put, put_stream, signed_url,
};

fn run<A: Send + 'static>(eff: Effect<A, StorageError>) -> Result<A, StorageError> {
  match eff.block_on() {
    Exit::Success(a) => Ok(a),
    Exit::Failure(Cause::Fail(e)) => Err(e),
    Exit::Failure(c) => panic!("unexpected cause: {c:?}"),
  }
}

fn key(k: &str) -> ObjectKey {
  ObjectKey::new(k).unwrap()
}

fn collect(stream: Stream<Bytes, StorageError>) -> Result<Vec<u8>, StorageError> {
  run(stream.run_collect()).map(|chunks| chunks.concat())
}

fn signer() -> UrlSigner {
  UrlSigner::new(
    *b"conformance-secret-0123456789abc",
    "https://files.example.com/o/".parse().unwrap(),
  )
  .unwrap()
}

fn listed(store: &dyn ObjectStore, prefix: Option<&str>) -> BTreeSet<String> {
  let prefix = prefix.map(key);
  run(store.list(prefix.as_ref()).run_collect())
    .unwrap()
    .into_iter()
    .map(|m| m.key.to_string())
    .collect()
}

/// The behaviour every backend shares; `store` is empty and uses 16-byte parts.
fn conform(store: &dyn ObjectStore, signing: bool) {
  let a = key("docs/a.txt");

  // Missing objects.
  assert!(!run(store.exists(&a)).unwrap());
  assert!(matches!(run(store.get(&a)), Err(StorageError::NotFound { key }) if key == "docs/a.txt"));
  assert!(matches!(
    run(store.head(&a)),
    Err(StorageError::NotFound { .. })
  ));
  assert!(matches!(
    collect(store.get_stream(&a)),
    Err(StorageError::NotFound { .. })
  ));
  assert_eq!(run(store.delete(&a)), Ok(()));

  // Put, read back, replace.
  let put = run(store.put(&a, Bytes::from_static(b"alpha"))).unwrap();
  assert_eq!((put.key.as_str(), put.size), ("docs/a.txt", 5));
  assert!(run(store.exists(&a)).unwrap());
  assert_eq!(run(store.get(&a)).unwrap(), "alpha");
  let meta = run(store.head(&a)).unwrap();
  assert_eq!((meta.key.clone(), meta.size), (a.clone(), 5));
  run(store.put(&a, Bytes::from_static(b"beta"))).unwrap();
  assert_eq!(collect(store.get_stream(&a)).unwrap(), b"beta");

  // Streamed upload over several parts, streamed download.
  let big = key("docs/big.bin");
  let items: Vec<Bytes> = (0..10_u8).map(|i| Bytes::from(vec![i; 7])).collect();
  let expected = items.concat();
  let out = run(store.put_stream(&big, Stream::from_vec(items))).unwrap();
  assert_eq!(out.size, 70);
  assert_eq!(run(store.head(&big)).unwrap().size, 70);
  assert_eq!(collect(store.get_stream(&big)).unwrap(), expected);

  // An empty stream is an empty object.
  let empty = key("docs/empty");
  assert_eq!(
    run(store.put_stream(&empty, Stream::empty())).unwrap().size,
    0
  );
  assert_eq!(run(store.get(&empty)).unwrap(), "");

  // Keys with characters a file name or URL must escape round-trip, listing included.
  let odd = key("odd/a#1 b%c ü?.txt");
  run(store.put(&odd, Bytes::from_static(b"odd"))).unwrap();
  assert_eq!(run(store.get(&odd)).unwrap(), "odd");

  // Listing: whole-segment prefixes, recursive.
  run(store.put(&key("docsx/c"), Bytes::from_static(b"c"))).unwrap();
  run(store.put(&key("docs/sub/d"), Bytes::from_static(b"d"))).unwrap();
  assert_eq!(
    listed(store, Some("docs")),
    ["docs/a.txt", "docs/big.bin", "docs/empty", "docs/sub/d"]
      .map(String::from)
      .into()
  );
  assert_eq!(listed(store, None).len(), 6);
  assert!(listed(store, None).contains(odd.as_str()));
  assert!(listed(store, Some("nothing")).is_empty());

  // Delete is idempotent.
  run(store.delete(&a)).unwrap();
  assert!(!run(store.exists(&a)).unwrap());
  run(store.delete(&a)).unwrap();
  run(store.delete(&key("docs/sub/d"))).unwrap();
  assert_eq!(
    listed(store, Some("docs")),
    ["docs/big.bin", "docs/empty"].map(String::from).into()
  );

  // Signed URLs.
  let url = run(store.signed_url(SignedMethod::Get, &big, Duration::from_secs(60)));
  if signing {
    assert_eq!(signer().verify(SignedMethod::Get, &url.unwrap()), Ok(big));
  } else {
    assert_eq!(url, Err(StorageError::SigningUnavailable));
  }
}

#[test]
fn memory_and_local_conform() {
  conform(&MemoryStore::new().with_part_size(16), false);
  conform(
    &MemoryStore::new()
      .with_part_size(16)
      .with_url_signer(signer()),
    true,
  );

  let dir = tempfile::tempdir().unwrap();
  let local = LocalStore::new(dir.path().join("plain")).unwrap();
  assert_eq!(local.root(), dir.path().join("plain"));
  conform(&local.with_part_size(16), false);
  let signed = LocalStore::new(dir.path().join("signed"))
    .unwrap()
    .with_part_size(16)
    .with_url_signer(signer());
  conform(&signed, true);
}

#[test]
fn local_store_keeps_objects_as_files_under_its_root() {
  let dir = tempfile::tempdir().unwrap();
  let store = LocalStore::new(dir.path()).unwrap();
  run(store.put(&key("a/b/c.txt"), Bytes::from_static(b"file"))).unwrap();
  assert_eq!(
    std::fs::read(dir.path().join("a/b/c.txt")).unwrap(),
    b"file"
  );
  // A second store on the same root sees the object.
  let again = LocalStore::new(dir.path()).unwrap();
  assert_eq!(run(again.get(&key("a/b/c.txt"))).unwrap(), "file");
  run(again.delete(&key("a/b/c.txt"))).unwrap();
  assert!(
    !dir.path().join("a").exists(),
    "empty directories are removed"
  );
  assert!(dir.path().exists());
}

#[test]
fn local_download_streams_in_bounded_chunks() {
  let dir = tempfile::tempdir().unwrap();
  let store = LocalStore::new(dir.path()).unwrap();
  let k = key("large.bin");
  let data: Vec<u8> = (0..100 * 1024)
    .map(|i| u8::try_from(i % 256).unwrap())
    .collect();
  std::fs::write(dir.path().join("large.bin"), &data).unwrap();
  let chunks = run(store.get_stream(&k).run_collect()).unwrap();
  assert!(chunks.len() > 1, "one chunk: the whole file was buffered");
  assert!(chunks.iter().all(|c| c.len() <= 8 * 1024));
  assert_eq!(chunks.concat(), data);
}

#[test]
fn local_store_rejects_a_root_that_is_a_file() {
  let dir = tempfile::tempdir().unwrap();
  let file = dir.path().join("file");
  std::fs::write(&file, b"x").unwrap();
  assert!(matches!(
    LocalStore::new(&file),
    Err(StorageError::Config { .. })
  ));
  assert!(matches!(
    LocalStore::layer(file).build().block_on(),
    Exit::Failure(Cause::Fail(StorageError::Config { .. }))
  ));
}

#[test]
fn the_capability_functions_use_the_provided_store() {
  let k = key("cap/x.txt");
  let program = put::<ObjectStorageEnv, _>(k.clone(), Bytes::from_static(b"via-cap"))
    .flat_map({
      let k = k.clone();
      move |_| exists(k)
    })
    .flat_map({
      let k = k.clone();
      move |found| get(k).map(move |b| (found, b))
    });
  let env = object_storage_env(MemoryStore::new().with_url_signer(signer()));
  let (found, body) = run(program.provide_all(env.clone())).unwrap();
  assert!(found);
  assert_eq!(body, "via-cap");

  let meta = run(head::<ObjectStorageEnv, _>(k.clone()).provide_all(env.clone())).unwrap();
  assert_eq!(meta.size, 7);
  let streamed =
    run(get_stream::<ObjectStorageEnv, _>(k.clone()).provide_all(env.clone())).unwrap();
  assert_eq!(collect(streamed).unwrap(), b"via-cap");
  let keys = run(list::<ObjectStorageEnv, _>(Some(key("cap"))).provide_all(env.clone())).unwrap();
  assert_eq!(run(keys.run_collect()).unwrap().len(), 1);
  let url = run(
    signed_url::<ObjectStorageEnv, _>(SignedMethod::Put, k.clone(), Duration::from_secs(5))
      .provide_all(env.clone()),
  )
  .unwrap();
  assert_eq!(signer().verify(SignedMethod::Put, &url), Ok(k.clone()));
  run(delete::<ObjectStorageEnv, _>(k.clone()).provide_all(env.clone())).unwrap();
  assert!(!run(exists::<ObjectStorageEnv, _>(k).provide_all(env)).unwrap());
}

#[test]
fn layers_provide_the_capability() {
  let k = key("layered.txt");
  let body = Stream::from_vec(vec![
    Bytes::from_static(b"lay"),
    Bytes::from_static(b"ered"),
  ]);
  let program = put_stream::<ObjectStorageEnv, _>(k.clone(), body).flat_map(move |_| get(k));
  assert_eq!(
    run(program.provide_layer(MemoryStore::layer())).unwrap(),
    "layered"
  );

  let dir = tempfile::tempdir().unwrap();
  let k = key("on/disk.txt");
  let program =
    put::<ObjectStorageEnv, _>(k.clone(), Bytes::from_static(b"disk")).flat_map(move |_| get(k));
  assert_eq!(
    run(program.provide_layer(LocalStore::layer(dir.path().join("root")))).unwrap(),
    "disk"
  );

  let store = MemoryStore::default();
  let shared = store.clone();
  let k = key("shared");
  let program = put::<ObjectStorageEnv, _>(k.clone(), Bytes::from_static(b"s"));
  run(program.provide_layer(provide_object_storage(store))).unwrap();
  assert_eq!(run(shared.get(&k)).unwrap(), "s", "clones share objects");
  assert_eq!(shared.part_size(), id_effect_storage::DEFAULT_PART_SIZE);
  let _: ObjectStorage = ObjectStorage::new(shared);
}

/// An application's own backend: read-only over another store.
struct ReadOnly(MemoryStore);

impl ObjectStore for ReadOnly {
  fn put(&self, key: &ObjectKey, _: Bytes) -> Effect<PutOutcome, StorageError> {
    id_effect::fail(StorageError::PermissionDenied {
      key: key.to_string(),
      message: "read-only".into(),
    })
  }
  fn put_stream(
    &self,
    key: &ObjectKey,
    _: Stream<Bytes, StorageError>,
  ) -> Effect<PutOutcome, StorageError> {
    self.put(key, Bytes::new())
  }
  fn get(&self, key: &ObjectKey) -> Effect<Bytes, StorageError> {
    self.0.get(key)
  }
  fn get_stream(&self, key: &ObjectKey) -> Stream<Bytes, StorageError> {
    self.0.get_stream(key)
  }
  fn head(&self, key: &ObjectKey) -> Effect<ObjectMeta, StorageError> {
    self.0.head(key)
  }
  fn delete(&self, key: &ObjectKey) -> Effect<(), StorageError> {
    self.put(key, Bytes::new()).map(|_| ())
  }
  fn list(&self, prefix: Option<&ObjectKey>) -> Stream<ObjectMeta, StorageError> {
    self.0.list(prefix)
  }
  fn signed_url(
    &self,
    method: SignedMethod,
    key: &ObjectKey,
    expires_in: Duration,
  ) -> Effect<Url, StorageError> {
    self.0.signed_url(method, key, expires_in)
  }
}

#[test]
fn an_application_backend_is_swappable_behind_the_capability() {
  let inner = MemoryStore::new();
  let k = key("seeded");
  run(inner.put(&k, Bytes::from_static(b"seed"))).unwrap();
  let env = object_storage_env(ReadOnly(inner));
  assert_eq!(
    run(get::<ObjectStorageEnv, _>(k.clone()).provide_all(env.clone())).unwrap(),
    "seed"
  );
  assert!(run(exists::<ObjectStorageEnv, _>(k.clone()).provide_all(env.clone())).unwrap());
  assert!(!run(exists::<ObjectStorageEnv, _>(key("other")).provide_all(env.clone())).unwrap());
  assert!(matches!(
    run(put::<ObjectStorageEnv, _>(k, Bytes::from_static(b"x")).provide_all(env)),
    Err(StorageError::PermissionDenied { .. })
  ));
}

#[test]
fn a_zero_part_size_means_one_byte_parts() {
  let store = MemoryStore::new().with_part_size(0);
  assert_eq!(store.part_size(), 1);
  let k = key("bytes");
  let out = run(store.put_stream(
    &k,
    Stream::from_vec(vec![Bytes::from_static(b"abc"), Bytes::from_static(b"de")]),
  ))
  .unwrap();
  assert_eq!(out.size, 5);
  assert_eq!(run(store.get(&k)).unwrap(), "abcde");
  let local_dir = tempfile::tempdir().unwrap();
  let local = LocalStore::new(local_dir.path()).unwrap().with_part_size(0);
  assert_eq!(local.part_size(), 1);
}

#[test]
fn a_failing_body_leaves_no_object() {
  let store = MemoryStore::new().with_part_size(4);
  let k = key("broken");
  let body = Stream::from_vec(vec![Bytes::from_static(b"12345678")]).chain(Stream::fail(
    StorageError::Backend {
      message: "client went away".into(),
    },
  ));
  assert!(matches!(
    run(store.put_stream(&k, body)),
    Err(StorageError::Backend { message }) if message == "client went away"
  ));
  assert!(!run(store.exists(&k)).unwrap());
}

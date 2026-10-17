//! The [`ObjectStore`] capability: put / get / head / delete / list over swappable backends, with
//! streaming upload and download as core [`Stream`]s and signed URLs (web.storage#1, #5, #7).
//!
//! The capability is [`ObjectStorage`]; [`provide_object_storage`] and the backends' `layer`
//! constructors are the [`Layer`]s that provide it ([`ObjectStorageEnv`]). The backends —
//! [`MemoryStore`](crate::MemoryStore), [`LocalStore`](crate::LocalStore) and, with the `s3`
//! feature, `S3Store` — all run on [`object_store`](https://docs.rs/object_store) and behave
//! the same: `delete` of a missing key succeeds, `get` / `head` of one fails with
//! [`StorageError::NotFound`].
//!
//! ## Streaming without buffering the object
//!
//! [`ObjectStore::put_stream`] pulls the body one item at a time and keeps at most one part
//! ([`DEFAULT_PART_SIZE`] unless the backend is configured otherwise) plus one body item in
//! memory: a body shorter than a part is sent with one `PUT`, a longer one as a multipart upload
//! with one part in flight, aborted if the body or a part fails. [`ObjectStore::get_stream`]
//! yields the object as the backend reads it (8 KiB file reads, network chunks for S3).

use std::fmt;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use bytes::Bytes;
use futures_util::stream::{self as fstream, BoxStream, StreamExt, TryStreamExt};
use id_effect::{
  Effect, HCons, HList, HNil, Layer, Selector, Stream, fail, from_async, hlist, need, succeed,
  suspend,
};
use object_store::path::Path;
use object_store::{GetOptions, MultipartUpload, PutMultipartOptions, PutOptions, PutPayload};
use percent_encoding::percent_decode_str;
use tokio::sync::Mutex;
use url::Url;

use crate::ObjectKey;
use crate::signed::{SignedMethod, UrlSigner};

/// The multipart part size a backend uses unless configured otherwise (8 MiB).
pub const DEFAULT_PART_SIZE: usize = 8 * 1024 * 1024;

/// Why a storage operation failed.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum StorageError {
  /// No object has this key.
  NotFound {
    /// The key (the backend's path when no key is at hand, e.g. while listing).
    key: String,
  },
  /// The object exists and the operation required it not to.
  AlreadyExists {
    /// The key.
    key: String,
  },
  /// The backend refused the credentials or the operation.
  PermissionDenied {
    /// The key.
    key: String,
    /// The backend's message.
    message: String,
  },
  /// A key the backend cannot store, or a stored key that is not a valid [`ObjectKey`].
  InvalidKey {
    /// The key or backend path.
    key: String,
    /// Why it is invalid.
    reason: String,
  },
  /// The backend cannot sign URLs (a local or in-memory store without a [`UrlSigner`]).
  SigningUnavailable,
  /// The backend does not support the operation.
  Unsupported {
    /// The backend's message.
    message: String,
  },
  /// The backend could not be configured (bad root directory, bucket, credentials, secret).
  Config {
    /// What is wrong.
    message: String,
  },
  /// Any other backend failure (I/O, network, protocol).
  Backend {
    /// The backend's message.
    message: String,
  },
}

impl fmt::Display for StorageError {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    match self {
      StorageError::NotFound { key } => write!(f, "object {key:?} not found"),
      StorageError::AlreadyExists { key } => write!(f, "object {key:?} already exists"),
      StorageError::PermissionDenied { key, message } => {
        write!(f, "permission denied for object {key:?}: {message}")
      }
      StorageError::InvalidKey { key, reason } => write!(f, "invalid object key {key:?}: {reason}"),
      StorageError::SigningUnavailable => f.write_str("this store cannot sign URLs"),
      StorageError::Unsupported { message } => write!(f, "unsupported operation: {message}"),
      StorageError::Config { message } => write!(f, "storage configuration: {message}"),
      StorageError::Backend { message } => write!(f, "storage backend: {message}"),
    }
  }
}

impl std::error::Error for StorageError {}

/// `e` as a [`StorageError`], naming `key` when the operation had one.
pub(crate) fn from_backend(key: Option<&ObjectKey>, e: object_store::Error) -> StorageError {
  use object_store::Error as E;
  let key_or = |path: &str| key.map_or_else(|| path.to_owned(), ToString::to_string);
  match &e {
    E::NotFound { path, .. } => StorageError::NotFound { key: key_or(path) },
    E::AlreadyExists { path, .. } => StorageError::AlreadyExists { key: key_or(path) },
    E::PermissionDenied { path, .. } | E::Unauthenticated { path, .. } => {
      StorageError::PermissionDenied {
        key: key_or(path),
        message: e.to_string(),
      }
    }
    E::InvalidPath { .. } => StorageError::InvalidKey {
      key: key_or(""),
      reason: e.to_string(),
    },
    E::NotSupported { .. } | E::NotImplemented { .. } => StorageError::Unsupported {
      message: e.to_string(),
    },
    _ => StorageError::Backend {
      message: e.to_string(),
    },
  }
}

/// What a store knows about an object.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct ObjectMeta {
  /// The key.
  pub key: ObjectKey,
  /// Size in bytes.
  pub size: u64,
  /// When the object was last written.
  pub last_modified: SystemTime,
  /// The entity tag, if the backend has one.
  pub e_tag: Option<String>,
  /// The version, if the backend versions objects.
  pub version: Option<String>,
}

/// The result of a put.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct PutOutcome {
  /// The key written.
  pub key: ObjectKey,
  /// Bytes written.
  pub size: u64,
  /// The entity tag, if the backend has one.
  pub e_tag: Option<String>,
  /// The version, if the backend versions objects.
  pub version: Option<String>,
}

/// Capability: object storage operations as [`Effect`] values and [`Stream`]s.
pub trait ObjectStore: Send + Sync + 'static {
  /// Stores `body` under `key`, replacing any object there.
  fn put(&self, key: &ObjectKey, body: Bytes) -> Effect<PutOutcome, StorageError>;

  /// Stores the bytes of `body` under `key` without holding the whole object in memory (see
  /// the [module docs](self)). A failing `body` fails the put and leaves no object behind.
  fn put_stream(
    &self,
    key: &ObjectKey,
    body: Stream<Bytes, StorageError>,
  ) -> Effect<PutOutcome, StorageError>;

  /// The whole object, in one buffer (use [`get_stream`](Self::get_stream) for large objects).
  fn get(&self, key: &ObjectKey) -> Effect<Bytes, StorageError>;

  /// The object as a stream of byte chunks, read as it is pulled. The object is opened on the
  /// first pull, which fails with [`StorageError::NotFound`] if it does not exist.
  fn get_stream(&self, key: &ObjectKey) -> Stream<Bytes, StorageError>;

  /// The object's metadata.
  fn head(&self, key: &ObjectKey) -> Effect<ObjectMeta, StorageError>;

  /// Whether an object has this key.
  fn exists(&self, key: &ObjectKey) -> Effect<bool, StorageError> {
    self.head(key).map(|_| true).catch(|e| match e {
      StorageError::NotFound { .. } => succeed(false),
      e => fail(e),
    })
  }

  /// Removes the object; succeeds if there is none.
  fn delete(&self, key: &ObjectKey) -> Effect<(), StorageError>;

  /// The objects under `prefix` (whole segments: `a` covers `a/b` but not `ab/c`), or all
  /// objects, recursively and in no particular order.
  fn list(&self, prefix: Option<&ObjectKey>) -> Stream<ObjectMeta, StorageError>;

  /// A URL that lets its holder perform `method` on `key` for `expires_in`, without
  /// credentials: presigned for S3, [`UrlSigner`]-signed for the other backends.
  fn signed_url(
    &self,
    method: SignedMethod,
    key: &ObjectKey,
    expires_in: Duration,
  ) -> Effect<Url, StorageError>;
}

/// How a backend signs URLs.
#[derive(Clone, Debug)]
pub(crate) enum Signing {
  None,
  Hmac(UrlSigner),
  #[cfg(feature = "s3")]
  Presign(Arc<dyn object_store::signer::Signer>),
}

/// How [`ObjectKey`]s map to backend paths.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum KeyMapping {
  /// The key is the path, verbatim (object stores).
  Verbatim,
  /// Each segment is percent-encoded as `object_store` names files, so listing returns the keys
  /// that were written (file systems).
  Encoded,
}

/// The [`ObjectStore`] implementation every backend shares: an `object_store` store, its URL
/// signing and its part size.
#[derive(Clone)]
pub(crate) struct Backend {
  store: Arc<dyn object_store::ObjectStore>,
  signing: Signing,
  keys: KeyMapping,
  part_size: usize,
}

impl fmt::Debug for Backend {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("Backend")
      .field("store", &self.store.to_string())
      .field("signing", &self.signing)
      .field("part_size", &self.part_size)
      .finish()
  }
}

impl Backend {
  pub(crate) fn new(store: Arc<dyn object_store::ObjectStore>, keys: KeyMapping) -> Self {
    Self {
      store,
      signing: Signing::None,
      keys,
      part_size: DEFAULT_PART_SIZE,
    }
  }

  pub(crate) fn set_signing(&mut self, signing: Signing) {
    self.signing = signing;
  }

  pub(crate) fn set_part_size(&mut self, part_size: usize) {
    self.part_size = part_size.max(1);
  }

  pub(crate) fn part_size(&self) -> usize {
    self.part_size
  }

  fn path(&self, key: &ObjectKey) -> Result<Path, StorageError> {
    match self.keys {
      KeyMapping::Verbatim => Path::parse(key.as_str()).map_err(|e| StorageError::InvalidKey {
        key: key.to_string(),
        reason: e.to_string(),
      }),
      KeyMapping::Encoded => Ok(key.segments().collect()),
    }
  }

  fn key(&self, path: &Path) -> Result<ObjectKey, StorageError> {
    let invalid = |reason: String| StorageError::InvalidKey {
      key: path.to_string(),
      reason,
    };
    let raw = match self.keys {
      KeyMapping::Verbatim => path.as_ref().to_owned(),
      KeyMapping::Encoded => path
        .parts()
        .map(|part| {
          percent_decode_str(part.as_ref())
            .decode_utf8()
            .map(|s| s.into_owned())
        })
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| invalid(e.to_string()))?
        .join("/"),
    };
    ObjectKey::new(raw).map_err(|e| invalid(e.to_string()))
  }

  fn meta(&self, meta: object_store::ObjectMeta) -> Result<ObjectMeta, StorageError> {
    Ok(ObjectMeta {
      key: self.key(&meta.location)?,
      size: meta.size,
      last_modified: meta.last_modified.into(),
      e_tag: meta.e_tag,
      version: meta.version,
    })
  }
}

impl ObjectStore for Backend {
  fn put(&self, key: &ObjectKey, body: Bytes) -> Effect<PutOutcome, StorageError> {
    let path = match self.path(key) {
      Ok(p) => p,
      Err(e) => return fail(e),
    };
    let size = body.len();
    Upload::new(self, key, path).put(body.into(), size)
  }

  fn put_stream(
    &self,
    key: &ObjectKey,
    body: Stream<Bytes, StorageError>,
  ) -> Effect<PutOutcome, StorageError> {
    let path = match self.path(key) {
      Ok(p) => p,
      Err(e) => return fail(e),
    };
    Upload::new(self, key, path).run(body)
  }

  fn get(&self, key: &ObjectKey) -> Effect<Bytes, StorageError> {
    let path = match self.path(key) {
      Ok(p) => p,
      Err(e) => return fail(e),
    };
    let (store, key) = (Arc::clone(&self.store), key.clone());
    from_async(move || async move {
      let got = store.get_opts(&path, GetOptions::default()).await;
      let bytes = match got {
        Ok(got) => got.bytes().await,
        Err(e) => Err(e),
      };
      bytes.map_err(|e| from_backend(Some(&key), e))
    })
  }

  fn get_stream(&self, key: &ObjectKey) -> Stream<Bytes, StorageError> {
    let path = match self.path(key) {
      Ok(p) => p,
      Err(e) => return Stream::fail(e),
    };
    let (store, key) = (Arc::clone(&self.store), key.clone());
    let opened = fstream::once(async move {
      match store.get_opts(&path, GetOptions::default()).await {
        Ok(got) => Ok(
          got
            .into_stream()
            .map_err(move |e| from_backend(Some(&key), e)),
        ),
        Err(e) => Err(from_backend(Some(&key), e)),
      }
    })
    .try_flatten()
    .boxed();
    bridge(opened)
  }

  fn head(&self, key: &ObjectKey) -> Effect<ObjectMeta, StorageError> {
    let path = match self.path(key) {
      Ok(p) => p,
      Err(e) => return fail(e),
    };
    let (this, key) = (self.clone(), key.clone());
    from_async(move || async move {
      let got = this
        .store
        .get_opts(&path, GetOptions::new().with_head(true))
        .await
        .map_err(|e| from_backend(Some(&key), e))?;
      this.meta(got.meta)
    })
  }

  fn delete(&self, key: &ObjectKey) -> Effect<(), StorageError> {
    let path = match self.path(key) {
      Ok(p) => p,
      Err(e) => return fail(e),
    };
    let (store, key) = (Arc::clone(&self.store), key.clone());
    from_async(move || async move {
      let mut deleted = store.delete_stream(fstream::once(async move { Ok(path) }).boxed());
      while let Some(outcome) = deleted.next().await {
        match outcome {
          Ok(_) | Err(object_store::Error::NotFound { .. }) => {}
          Err(e) => return Err(from_backend(Some(&key), e)),
        }
      }
      Ok(())
    })
  }

  fn list(&self, prefix: Option<&ObjectKey>) -> Stream<ObjectMeta, StorageError> {
    let prefix = match prefix.map(|p| self.path(p)).transpose() {
      Ok(p) => p,
      Err(e) => return Stream::fail(e),
    };
    let this = self.clone();
    let listed = self
      .store
      .list(prefix.as_ref())
      .map(move |meta| {
        meta
          .map_err(|e| from_backend(None, e))
          .and_then(|m| this.meta(m))
      })
      .boxed();
    bridge(listed)
  }

  fn signed_url(
    &self,
    method: SignedMethod,
    key: &ObjectKey,
    expires_in: Duration,
  ) -> Effect<Url, StorageError> {
    match &self.signing {
      Signing::None => fail(StorageError::SigningUnavailable),
      Signing::Hmac(signer) => {
        let (signer, key) = (signer.clone(), key.clone());
        suspend(move || succeed(signer.sign(method, &key, SystemTime::now() + expires_in)))
      }
      #[cfg(feature = "s3")]
      Signing::Presign(signer) => {
        let path = match self.path(key) {
          Ok(p) => p,
          Err(e) => return fail(e),
        };
        let (signer, key) = (Arc::clone(signer), key.clone());
        let method = match method {
          SignedMethod::Get => object_store::signer::Method::GET,
          SignedMethod::Put => object_store::signer::Method::PUT,
        };
        from_async(move || async move {
          signer
            .signed_url(method, &path, expires_in)
            .await
            .map_err(|e| from_backend(Some(&key), e))
        })
      }
    }
  }
}

/// A core [`Stream`] over `source`: each pull awaits one item, so nothing is read ahead.
fn bridge<T: Send + 'static>(
  source: BoxStream<'static, Result<T, StorageError>>,
) -> Stream<T, StorageError> {
  let source = Arc::new(Mutex::new(source));
  Stream::from_pull(move |_| {
    let source = Arc::clone(&source);
    from_async(move || async move {
      match source.lock().await.next().await {
        Some(Ok(item)) => Ok(Some(vec![item])),
        Some(Err(e)) => Err(e),
        None => Ok(None),
      }
    })
  })
}

/// Body bytes pulled and not yet sent: at most one part plus one body item.
#[derive(Debug, Default)]
struct Pending {
  items: std::collections::VecDeque<Bytes>,
  len: usize,
}

impl Pending {
  fn push(&mut self, bytes: Bytes) {
    if !bytes.is_empty() {
      self.len += bytes.len();
      self.items.push_back(bytes);
    }
  }

  /// The first `n` bytes (all of them when fewer) as one payload, without copying.
  fn take(&mut self, n: usize) -> PutPayload {
    let mut out = Vec::new();
    let mut want = n.min(self.len);
    self.len -= want;
    while want > 0 {
      let Some(mut bytes) = self.items.pop_front() else {
        break;
      };
      if bytes.len() > want {
        out.push(bytes.split_to(want));
        self.items.push_front(bytes);
        want = 0;
      } else {
        want -= bytes.len();
        out.push(bytes);
      }
    }
    out.into_iter().collect()
  }
}

type Body = Stream<Bytes, StorageError>;
type SharedUpload = Arc<Mutex<Box<dyn MultipartUpload>>>;

/// One put: the store, the destination and the part size.
struct Upload {
  store: Arc<dyn object_store::ObjectStore>,
  key: ObjectKey,
  path: Path,
  part_size: usize,
}

impl Upload {
  fn new(backend: &Backend, key: &ObjectKey, path: Path) -> Arc<Self> {
    Arc::new(Self {
      store: Arc::clone(&backend.store),
      key: key.clone(),
      path,
      part_size: backend.part_size,
    })
  }
}

trait UploadExt {
  fn put(self, payload: PutPayload, size: usize) -> Effect<PutOutcome, StorageError>;
  fn run(self, body: Body) -> Effect<PutOutcome, StorageError>;
  fn parts(
    self,
    upload: SharedUpload,
    pending: Pending,
    rest: Option<Body>,
    sent: u64,
  ) -> Effect<PutOutcome, StorageError>;
}

impl UploadExt for Arc<Upload> {
  /// One `PUT` of `payload`.
  fn put(self, payload: PutPayload, size: usize) -> Effect<PutOutcome, StorageError> {
    from_async(move || async move {
      let put = self
        .store
        .put_opts(&self.path, payload, PutOptions::default())
        .await
        .map_err(|e| from_backend(Some(&self.key), e))?;
      Ok(PutOutcome {
        key: self.key.clone(),
        size: size as u64,
        e_tag: put.e_tag,
        version: put.version,
      })
    })
  }

  /// Fills the first part; a body that ends first is one `PUT`, a longer one a multipart upload.
  fn run(self, body: Body) -> Effect<PutOutcome, StorageError> {
    fill(body, Pending::default(), self.part_size).flat_map(move |(mut pending, rest)| match rest {
      None => {
        let size = pending.len;
        self.put(pending.take(size), size)
      }
      Some(rest) => {
        let this = Arc::clone(&self);
        from_async(move || async move {
          this
            .store
            .put_multipart_opts(&this.path, PutMultipartOptions::default())
            .await
            .map_err(|e| from_backend(Some(&this.key), e))
        })
        .flat_map(move |upload| {
          let upload: SharedUpload = Arc::new(Mutex::new(upload));
          let on_failure = Arc::clone(&upload);
          self
            .parts(upload, pending, Some(rest), 0)
            .catch(move |e| abort(on_failure, e))
        })
      }
    })
  }

  /// Sends every full part (and the last, short one once the body has ended), refilling from the
  /// body in between; completes the upload at the end.
  fn parts(
    self,
    upload: SharedUpload,
    mut pending: Pending,
    rest: Option<Body>,
    sent: u64,
  ) -> Effect<PutOutcome, StorageError> {
    if pending.len >= self.part_size || (rest.is_none() && pending.len > 0) {
      let n = pending.len.min(self.part_size);
      let payload = pending.take(n);
      let (part_upload, key) = (Arc::clone(&upload), self.key.clone());
      return from_async(move || async move {
        let part = part_upload.lock().await.put_part(payload);
        part.await.map_err(|e| from_backend(Some(&key), e))
      })
      .flat_map(move |()| self.parts(upload, pending, rest, sent + n as u64));
    }
    match rest {
      Some(body) => {
        let part_size = self.part_size;
        fill(body, pending, part_size)
          .flat_map(move |(pending, rest)| self.parts(upload, pending, rest, sent))
      }
      None => from_async(move || async move {
        let done = upload
          .lock()
          .await
          .complete()
          .await
          .map_err(|e| from_backend(Some(&self.key), e))?;
        Ok(PutOutcome {
          key: self.key.clone(),
          size: sent,
          e_tag: done.e_tag,
          version: done.version,
        })
      }),
    }
  }
}

/// Pulls `body` one item at a time until `pending` holds a full part (returning the rest of the
/// body) or the body ends (returning `None`).
fn fill(
  body: Body,
  mut pending: Pending,
  part_size: usize,
) -> Effect<(Pending, Option<Body>), StorageError> {
  body
    .next_chunk_with_size(1)
    .flat_map(move |next| match next {
      None => succeed((pending, None)),
      Some((chunk, rest)) => {
        for bytes in chunk.into_vec() {
          pending.push(bytes);
        }
        if pending.len >= part_size {
          succeed((pending, Some(rest)))
        } else {
          fill(rest, pending, part_size)
        }
      }
    })
}

/// Aborts `upload` (best effort: the original failure is what the caller sees) and fails with `e`.
fn abort(upload: SharedUpload, e: StorageError) -> Effect<PutOutcome, StorageError> {
  from_async(move || async move {
    let _ = upload.lock().await.abort().await;
    Err(e)
  })
}

/// Implements [`ObjectStore`] for a backend type by delegating to its `backend` field.
macro_rules! delegate_object_store {
  ($ty:ty) => {
    impl $crate::store::ObjectStore for $ty {
      fn put(
        &self,
        key: &$crate::ObjectKey,
        body: ::bytes::Bytes,
      ) -> ::id_effect::Effect<$crate::PutOutcome, $crate::StorageError> {
        self.backend.put(key, body)
      }

      fn put_stream(
        &self,
        key: &$crate::ObjectKey,
        body: ::id_effect::Stream<::bytes::Bytes, $crate::StorageError>,
      ) -> ::id_effect::Effect<$crate::PutOutcome, $crate::StorageError> {
        self.backend.put_stream(key, body)
      }

      fn get(
        &self,
        key: &$crate::ObjectKey,
      ) -> ::id_effect::Effect<::bytes::Bytes, $crate::StorageError> {
        self.backend.get(key)
      }

      fn get_stream(
        &self,
        key: &$crate::ObjectKey,
      ) -> ::id_effect::Stream<::bytes::Bytes, $crate::StorageError> {
        self.backend.get_stream(key)
      }

      fn head(
        &self,
        key: &$crate::ObjectKey,
      ) -> ::id_effect::Effect<$crate::ObjectMeta, $crate::StorageError> {
        self.backend.head(key)
      }

      fn delete(&self, key: &$crate::ObjectKey) -> ::id_effect::Effect<(), $crate::StorageError> {
        self.backend.delete(key)
      }

      fn list(
        &self,
        prefix: Option<&$crate::ObjectKey>,
      ) -> ::id_effect::Stream<$crate::ObjectMeta, $crate::StorageError> {
        self.backend.list(prefix)
      }

      fn signed_url(
        &self,
        method: $crate::SignedMethod,
        key: &$crate::ObjectKey,
        expires_in: ::std::time::Duration,
      ) -> ::id_effect::Effect<::url::Url, $crate::StorageError> {
        self.backend.signed_url(method, key, expires_in)
      }
    }
  };
}
pub(crate) use delegate_object_store;

/// The [`ObjectStore`] capability: a shared handle to the installed backend.
#[derive(Clone)]
pub struct ObjectStorage(pub Arc<dyn ObjectStore>);

impl ObjectStorage {
  /// Wrap `store` as the capability.
  #[inline]
  pub fn new(store: impl ObjectStore) -> Self {
    Self(Arc::new(store))
  }
}

impl std::ops::Deref for ObjectStorage {
  type Target = dyn ObjectStore;

  fn deref(&self) -> &Self::Target {
    &*self.0
  }
}

impl fmt::Debug for ObjectStorage {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_tuple("ObjectStorage")
      .field(&"<dyn ObjectStore>")
      .finish()
  }
}

/// The minimal typed environment holding only [`ObjectStorage`] (the output of the storage
/// layers).
pub type ObjectStorageEnv = HCons<ObjectStorage, HNil>;

/// An [`ObjectStorageEnv`] wrapping `store` (for `.provide_all(..)` in tests and entry points).
#[inline]
pub fn object_storage_env(store: impl ObjectStore) -> ObjectStorageEnv {
  hlist![ObjectStorage::new(store)]
}

/// A [`Layer`] providing `store` as the [`ObjectStorage`] capability.
#[inline]
pub fn provide_object_storage<E: Send + 'static>(
  store: impl ObjectStore,
) -> Layer<ObjectStorageEnv, E> {
  Layer::succeed(object_storage_env(store))
}

/// A [`Layer`] that builds its store with `make` each time it is built (fallible backends).
pub(crate) fn object_storage_layer<S: ObjectStore>(
  make: impl Fn() -> Result<S, StorageError> + Send + Sync + 'static,
) -> Layer<ObjectStorageEnv, StorageError> {
  Layer::effect(move || match make() {
    Ok(store) => succeed(object_storage_env(store)),
    Err(e) => fail(e),
  })
}

/// [`ObjectStore::put`] via the [`ObjectStorage`] in `R`.
#[inline]
pub fn put<R, I>(key: ObjectKey, body: Bytes) -> Effect<PutOutcome, StorageError, R>
where
  R: Selector<ObjectStorage, I> + HList,
{
  need::<ObjectStorage>().flat_map(move |s| s.put(&key, body).narrow())
}

/// [`ObjectStore::put_stream`] via the [`ObjectStorage`] in `R`.
#[inline]
pub fn put_stream<R, I>(key: ObjectKey, body: Body) -> Effect<PutOutcome, StorageError, R>
where
  R: Selector<ObjectStorage, I> + HList,
{
  need::<ObjectStorage>().flat_map(move |s| s.put_stream(&key, body).narrow())
}

/// [`ObjectStore::get`] via the [`ObjectStorage`] in `R`.
#[inline]
pub fn get<R, I>(key: ObjectKey) -> Effect<Bytes, StorageError, R>
where
  R: Selector<ObjectStorage, I> + HList,
{
  need::<ObjectStorage>().flat_map(move |s| s.get(&key).narrow())
}

/// [`ObjectStore::get_stream`] of the [`ObjectStorage`] in `R` (pull it like any stream).
#[inline]
pub fn get_stream<R, I>(key: ObjectKey) -> Effect<Body, StorageError, R>
where
  R: Selector<ObjectStorage, I> + HList,
{
  need::<ObjectStorage>().map(move |s| s.get_stream(&key))
}

/// [`ObjectStore::head`] via the [`ObjectStorage`] in `R`.
#[inline]
pub fn head<R, I>(key: ObjectKey) -> Effect<ObjectMeta, StorageError, R>
where
  R: Selector<ObjectStorage, I> + HList,
{
  need::<ObjectStorage>().flat_map(move |s| s.head(&key).narrow())
}

/// [`ObjectStore::exists`] via the [`ObjectStorage`] in `R`.
#[inline]
pub fn exists<R, I>(key: ObjectKey) -> Effect<bool, StorageError, R>
where
  R: Selector<ObjectStorage, I> + HList,
{
  need::<ObjectStorage>().flat_map(move |s| s.exists(&key).narrow())
}

/// [`ObjectStore::delete`] via the [`ObjectStorage`] in `R`.
#[inline]
pub fn delete<R, I>(key: ObjectKey) -> Effect<(), StorageError, R>
where
  R: Selector<ObjectStorage, I> + HList,
{
  need::<ObjectStorage>().flat_map(move |s| s.delete(&key).narrow())
}

/// [`ObjectStore::list`] of the [`ObjectStorage`] in `R`.
#[inline]
pub fn list<R, I>(
  prefix: Option<ObjectKey>,
) -> Effect<Stream<ObjectMeta, StorageError>, StorageError, R>
where
  R: Selector<ObjectStorage, I> + HList,
{
  need::<ObjectStorage>().map(move |s| s.list(prefix.as_ref()))
}

/// [`ObjectStore::signed_url`] via the [`ObjectStorage`] in `R`.
#[inline]
pub fn signed_url<R, I>(
  method: SignedMethod,
  key: ObjectKey,
  expires_in: Duration,
) -> Effect<Url, StorageError, R>
where
  R: Selector<ObjectStorage, I> + HList,
{
  need::<ObjectStorage>().flat_map(move |s| s.signed_url(method, &key, expires_in).narrow())
}

#[cfg(test)]
mod tests {
  use super::*;

  use std::sync::Mutex as StdMutex;
  use std::sync::atomic::{AtomicU64, Ordering};

  use async_trait::async_trait;
  use id_effect::{Cause, Exit};
  use object_store::memory::InMemory;
  use object_store::{
    CopyOptions, GetResult, ListResult, PutResult, UploadPart, path::Path as OsPath,
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

  /// Records, for every multipart part, how many body bytes had been produced when it was sent.
  #[derive(Debug)]
  struct RecordingStore {
    inner: InMemory,
    produced: Arc<AtomicU64>,
    parts: Arc<StdMutex<Vec<(u64, usize)>>>,
    fail_part: Option<usize>,
    aborted: Arc<AtomicU64>,
  }

  impl RecordingStore {
    fn new(produced: Arc<AtomicU64>) -> Self {
      Self {
        inner: InMemory::new(),
        produced,
        parts: Arc::default(),
        fail_part: None,
        aborted: Arc::default(),
      }
    }
  }

  impl fmt::Display for RecordingStore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
      f.write_str("RecordingStore")
    }
  }

  #[derive(Debug)]
  struct RecordingUpload {
    inner: Box<dyn MultipartUpload>,
    produced: Arc<AtomicU64>,
    parts: Arc<StdMutex<Vec<(u64, usize)>>>,
    fail_part: Option<usize>,
    aborted: Arc<AtomicU64>,
  }

  #[async_trait]
  impl MultipartUpload for RecordingUpload {
    fn put_part(&mut self, data: PutPayload) -> UploadPart {
      let mut parts = self.parts.lock().unwrap();
      if self.fail_part == Some(parts.len()) {
        return Box::pin(async {
          Err(object_store::Error::Generic {
            store: "recording",
            source: "part refused".into(),
          })
        });
      }
      parts.push((self.produced.load(Ordering::SeqCst), data.content_length()));
      self.inner.put_part(data)
    }

    async fn complete(&mut self) -> object_store::Result<PutResult> {
      self.inner.complete().await
    }

    async fn abort(&mut self) -> object_store::Result<()> {
      self.aborted.fetch_add(1, Ordering::SeqCst);
      self.inner.abort().await
    }
  }

  #[async_trait]
  impl object_store::ObjectStore for RecordingStore {
    async fn put_opts(
      &self,
      location: &OsPath,
      payload: PutPayload,
      opts: PutOptions,
    ) -> object_store::Result<PutResult> {
      self.inner.put_opts(location, payload, opts).await
    }

    async fn put_multipart_opts(
      &self,
      location: &OsPath,
      opts: PutMultipartOptions,
    ) -> object_store::Result<Box<dyn MultipartUpload>> {
      Ok(Box::new(RecordingUpload {
        inner: self.inner.put_multipart_opts(location, opts).await?,
        produced: Arc::clone(&self.produced),
        parts: Arc::clone(&self.parts),
        fail_part: self.fail_part,
        aborted: Arc::clone(&self.aborted),
      }))
    }

    async fn get_opts(
      &self,
      location: &OsPath,
      options: GetOptions,
    ) -> object_store::Result<GetResult> {
      self.inner.get_opts(location, options).await
    }

    fn delete_stream(
      &self,
      locations: BoxStream<'static, object_store::Result<OsPath>>,
    ) -> BoxStream<'static, object_store::Result<OsPath>> {
      self.inner.delete_stream(locations)
    }

    fn list(
      &self,
      prefix: Option<&OsPath>,
    ) -> BoxStream<'static, object_store::Result<object_store::ObjectMeta>> {
      self.inner.list(prefix)
    }

    async fn list_with_delimiter(
      &self,
      prefix: Option<&OsPath>,
    ) -> object_store::Result<ListResult> {
      self.inner.list_with_delimiter(prefix).await
    }

    async fn copy_opts(
      &self,
      from: &OsPath,
      to: &OsPath,
      options: CopyOptions,
    ) -> object_store::Result<()> {
      self.inner.copy_opts(from, to, options).await
    }
  }

  /// `items` items of `item_len` bytes (`i % 251` each), counting bytes into `produced` as they
  /// are pulled; fails on item `fail_at` if set.
  fn counted_body(
    items: usize,
    item_len: usize,
    produced: Arc<AtomicU64>,
    fail_at: Option<usize>,
  ) -> Body {
    let mut next = 0_usize;
    Stream::from_pull(move |_| {
      if next == items {
        return succeed(None);
      }
      if fail_at == Some(next) {
        return fail(StorageError::Backend {
          message: "body broke".into(),
        });
      }
      let byte = u8::try_from(next % 251).unwrap();
      next += 1;
      produced.fetch_add(item_len as u64, Ordering::SeqCst);
      succeed(Some(vec![Bytes::from(vec![byte; item_len])]))
    })
  }

  fn recording_backend(store: RecordingStore, part_size: usize) -> Backend {
    let mut backend = Backend::new(Arc::new(store), KeyMapping::Verbatim);
    backend.set_part_size(part_size);
    backend
  }

  #[test]
  fn stream_upload_never_buffers_whole_object() {
    const PART: usize = 1024;
    const ITEM: usize = 100;
    const ITEMS: usize = 100;
    let produced = Arc::new(AtomicU64::new(0));
    let store = RecordingStore::new(Arc::clone(&produced));
    let parts = Arc::clone(&store.parts);
    let backend = recording_backend(store, PART);
    let k = key("big/object.bin");

    let body = counted_body(ITEMS, ITEM, Arc::clone(&produced), None);
    let out = run(backend.put_stream(&k, body)).unwrap();
    let total = ITEMS * ITEM;
    assert_eq!(out.size, total as u64);

    let parts = parts.lock().unwrap().clone();
    assert_eq!(parts.len(), total.div_ceil(PART), "{parts:?}");
    let mut sent = 0_u64;
    for (produced_then, len) in &parts {
      // When a part leaves, the body has run ahead of what was sent by at most this part plus
      // one item — never the whole object.
      let buffered = produced_then - sent;
      assert!(
        buffered <= (PART + ITEM) as u64,
        "buffered {buffered} bytes: {parts:?}"
      );
      assert!(*len <= PART);
      sent += *len as u64;
    }
    assert!(parts[..parts.len() - 1].iter().all(|(_, len)| *len == PART));

    let back = run(backend.get(&k)).unwrap();
    let expected: Vec<u8> = (0..ITEMS)
      .flat_map(|i| vec![u8::try_from(i % 251).unwrap(); ITEM])
      .collect();
    assert_eq!(back.as_ref(), &expected[..]);
  }

  #[test]
  fn short_stream_is_one_put() {
    let produced = Arc::new(AtomicU64::new(0));
    let store = RecordingStore::new(Arc::clone(&produced));
    let parts = Arc::clone(&store.parts);
    let backend = recording_backend(store, 1024);
    let k = key("small.bin");
    let out = run(backend.put_stream(&k, counted_body(3, 10, produced, None))).unwrap();
    assert_eq!(out.size, 30);
    assert!(parts.lock().unwrap().is_empty());
    assert_eq!(run(backend.get(&k)).unwrap().len(), 30);
  }

  #[test]
  fn failing_body_aborts_the_upload_and_writes_nothing() {
    let produced = Arc::new(AtomicU64::new(0));
    let store = RecordingStore::new(Arc::clone(&produced));
    let aborted = Arc::clone(&store.aborted);
    let backend = recording_backend(store, 64);
    let k = key("broken.bin");
    let err = run(backend.put_stream(&k, counted_body(10, 50, produced, Some(5)))).unwrap_err();
    assert_eq!(
      err,
      StorageError::Backend {
        message: "body broke".into()
      }
    );
    assert_eq!(aborted.load(Ordering::SeqCst), 1);
    assert!(matches!(
      run(backend.head(&k)),
      Err(StorageError::NotFound { .. })
    ));
  }

  #[test]
  fn failing_part_aborts_the_upload() {
    let produced = Arc::new(AtomicU64::new(0));
    let mut store = RecordingStore::new(Arc::clone(&produced));
    store.fail_part = Some(1);
    let aborted = Arc::clone(&store.aborted);
    let backend = recording_backend(store, 64);
    let err =
      run(backend.put_stream(&key("p.bin"), counted_body(10, 50, produced, None))).unwrap_err();
    assert!(matches!(err, StorageError::Backend { .. }), "{err:?}");
    assert_eq!(aborted.load(Ordering::SeqCst), 1);
  }

  #[test]
  fn pending_take_splits_items_without_losing_bytes() {
    let mut p = Pending::default();
    p.push(Bytes::from_static(b"abc"));
    p.push(Bytes::new());
    p.push(Bytes::from_static(b"defg"));
    assert_eq!(p.len, 7);
    assert_eq!(Bytes::from(p.take(5)).as_ref(), b"abcde");
    assert_eq!(p.len, 2);
    assert_eq!(Bytes::from(p.take(10)).as_ref(), b"fg");
    assert_eq!(p.len, 0);
    assert!(p.items.is_empty());
  }

  #[test]
  fn backend_errors_map_to_typed_variants() {
    let k = key("a/b");
    let src = || -> Box<dyn std::error::Error + Send + Sync> { "x".into() };
    let path = || "p".to_string();
    let cases = [
      (
        object_store::Error::NotFound {
          path: path(),
          source: src(),
        },
        "NotFound",
      ),
      (
        object_store::Error::AlreadyExists {
          path: path(),
          source: src(),
        },
        "AlreadyExists",
      ),
      (
        object_store::Error::PermissionDenied {
          path: path(),
          source: src(),
        },
        "PermissionDenied",
      ),
      (
        object_store::Error::Unauthenticated {
          path: path(),
          source: src(),
        },
        "PermissionDenied",
      ),
      (
        object_store::Error::NotSupported { source: src() },
        "Unsupported",
      ),
      (
        object_store::Error::NotImplemented {
          operation: "op".into(),
          implementer: "i".into(),
        },
        "Unsupported",
      ),
      (
        object_store::Error::Generic {
          store: "s",
          source: src(),
        },
        "Backend",
      ),
      (
        object_store::Error::InvalidPath {
          source: OsPath::parse("a//b").unwrap_err(),
        },
        "InvalidKey",
      ),
    ];
    for (e, variant) in cases {
      let mapped = from_backend(Some(&k), e);
      assert!(format!("{mapped:?}").starts_with(variant), "{mapped:?}");
      assert!(!mapped.to_string().is_empty());
    }
    assert_eq!(
      from_backend(
        None,
        object_store::Error::NotFound {
          path: path(),
          source: src()
        }
      ),
      StorageError::NotFound { key: "p".into() }
    );
    for e in [
      StorageError::SigningUnavailable,
      StorageError::Config {
        message: "m".into(),
      },
      StorageError::InvalidKey {
        key: "k".into(),
        reason: "r".into(),
      },
    ] {
      assert!(!e.to_string().is_empty());
    }
  }

  #[test]
  fn listing_a_key_the_crate_cannot_represent_fails_typed() {
    let verbatim = Backend::new(Arc::new(InMemory::new()), KeyMapping::Verbatim);
    assert_eq!(
      verbatim.key(&OsPath::parse("a/%2E").unwrap()),
      Ok(key("a/%2E"))
    );
    assert!(matches!(
      verbatim.key(&OsPath::from("a".repeat(ObjectKey::MAX_LEN + 1))),
      Err(StorageError::InvalidKey { .. })
    ));
    let encoded = Backend::new(Arc::new(InMemory::new()), KeyMapping::Encoded);
    assert!(matches!(
      encoded.key(&OsPath::parse("a/%2E").unwrap()),
      Err(StorageError::InvalidKey { .. })
    ));
    assert!(matches!(
      encoded.key(&OsPath::parse("a/%FF").unwrap()),
      Err(StorageError::InvalidKey { .. })
    ));
  }

  #[test]
  fn object_storage_debug_hides_the_backend() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<ObjectStorage>();
    let backend = Backend::new(Arc::new(InMemory::new()), KeyMapping::Verbatim);
    assert!(format!("{backend:?}").contains("Backend"));
    let s = format!("{:?}", ObjectStorage::new(backend));
    assert_eq!(s, "ObjectStorage(\"<dyn ObjectStore>\")");
  }
}

//! [`MemoryStore`]: an in-memory backend for tests and local development (web.storage#8).

use std::sync::Arc;

use id_effect::Layer;
use object_store::memory::InMemory;

use crate::signed::UrlSigner;
use crate::store::{
  Backend, KeyMapping, ObjectStorageEnv, Signing, delegate_object_store, provide_object_storage,
};

/// An in-memory [`ObjectStore`](crate::ObjectStore). Clones share the same objects.
///
/// It signs URLs only when given a [`UrlSigner`] ([`with_url_signer`](Self::with_url_signer)).
#[derive(Clone, Debug)]
pub struct MemoryStore {
  backend: Backend,
}

impl Default for MemoryStore {
  fn default() -> Self {
    Self::new()
  }
}

impl MemoryStore {
  /// An empty store.
  pub fn new() -> Self {
    Self {
      backend: Backend::new(Arc::new(InMemory::new()), KeyMapping::Verbatim),
    }
  }

  /// Signs URLs with `signer` (the service serving its base URL verifies them).
  #[must_use]
  pub fn with_url_signer(mut self, signer: UrlSigner) -> Self {
    self.backend.set_signing(Signing::Hmac(signer));
    self
  }

  /// Sends streamed uploads in parts of `bytes` bytes (at least 1).
  #[must_use]
  pub fn with_part_size(mut self, bytes: usize) -> Self {
    self.backend.set_part_size(bytes);
    self
  }

  /// The part size of streamed uploads, in bytes.
  pub fn part_size(&self) -> usize {
    self.backend.part_size()
  }

  /// The layer: a new, empty [`MemoryStore`] as the [`ObjectStorage`](crate::ObjectStorage)
  /// capability.
  pub fn layer<E: Send + 'static>() -> Layer<ObjectStorageEnv, E> {
    provide_object_storage(Self::new())
  }
}

delegate_object_store!(MemoryStore);

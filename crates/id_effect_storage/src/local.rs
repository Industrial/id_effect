//! [`LocalStore`]: objects as files under a root directory (web.storage#2).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use id_effect::Layer;
use object_store::local::LocalFileSystem;

use crate::signed::UrlSigner;
use crate::store::{
  Backend, KeyMapping, ObjectStorageEnv, Signing, StorageError, delegate_object_store,
  object_storage_layer,
};

/// An [`ObjectStore`](crate::ObjectStore) keeping each object in a file under a root directory.
///
/// Key segments become directories and the file name, percent-encoded where a file name could
/// not hold them; a key can never leave the root ([`ObjectKey`](crate::ObjectKey) has no `..`).
/// Writes go to a staging file renamed into place, so readers never see a partial object.
/// Directories left empty by a delete are removed. It signs URLs only when given a
/// [`UrlSigner`] ([`with_url_signer`](Self::with_url_signer)).
#[derive(Clone, Debug)]
pub struct LocalStore {
  backend: Backend,
  root: PathBuf,
}

impl LocalStore {
  /// A store rooted at `root`, created if missing.
  ///
  /// # Errors
  ///
  /// [`StorageError::Config`] when `root` cannot be created or is not a directory.
  pub fn new(root: impl AsRef<Path>) -> Result<Self, StorageError> {
    let root = root.as_ref();
    let config = |message: String| StorageError::Config { message };
    std::fs::create_dir_all(root)
      .map_err(|e| config(format!("cannot create {}: {e}", root.display())))?;
    let fs = LocalFileSystem::new_with_prefix(root)
      .map_err(|e| config(e.to_string()))?
      .with_automatic_cleanup(true);
    Ok(Self {
      backend: Backend::new(Arc::new(fs), KeyMapping::Encoded),
      root: root.to_path_buf(),
    })
  }

  /// The root directory.
  pub fn root(&self) -> &Path {
    &self.root
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

  /// The layer: a [`LocalStore`] rooted at `root` as the
  /// [`ObjectStorage`](crate::ObjectStorage) capability, built (and `root` created) on each
  /// layer build.
  pub fn layer(root: impl Into<PathBuf>) -> Layer<ObjectStorageEnv, StorageError> {
    let root = root.into();
    object_storage_layer(move || Self::new(&root))
  }
}

delegate_object_store!(LocalStore);

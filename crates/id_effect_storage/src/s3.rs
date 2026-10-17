//! [`S3Store`]: AWS S3 and S3-compatible services (MinIO, Ceph, R2, ...) with presigned URLs
//! (web.storage#3, #7). Feature `s3`.
//!
//! ```
//! use id_effect_storage::{S3Config, S3Store};
//!
//! let config = S3Config::new("statements")
//!   .with_endpoint("http://localhost:9000")
//!   .with_allow_http(true)
//!   .with_credentials("minio", "minio-secret");
//! let store = S3Store::new(&config).unwrap();
//! assert_eq!(store.bucket(), "statements");
//! ```

use std::fmt;
use std::sync::Arc;

use id_effect::Layer;
use object_store::aws::AmazonS3Builder;

use crate::store::{
  Backend, DEFAULT_PART_SIZE, KeyMapping, ObjectStorageEnv, Signing, StorageError,
  delegate_object_store, object_storage_layer,
};

/// Where an [`S3Store`] connects and how it authenticates.
///
/// Unset fields keep the `object_store` defaults (region `us-east-1`, path-style requests,
/// HTTPS only); [`from_env`](Self::from_env) fills them from the `AWS_*` environment variables
/// first. The secret key never appears in `Debug` output.
#[derive(Clone)]
pub struct S3Config {
  bucket: String,
  from_env: bool,
  region: Option<String>,
  endpoint: Option<String>,
  credentials: Option<(String, String)>,
  allow_http: bool,
  virtual_hosted_style: bool,
}

impl fmt::Debug for S3Config {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("S3Config")
      .field("bucket", &self.bucket)
      .field("from_env", &self.from_env)
      .field("region", &self.region)
      .field("endpoint", &self.endpoint)
      .field(
        "access_key_id",
        &self.credentials.as_ref().map(|(id, _)| id),
      )
      .field("allow_http", &self.allow_http)
      .field("virtual_hosted_style", &self.virtual_hosted_style)
      .finish_non_exhaustive()
  }
}

impl S3Config {
  /// The bucket `bucket`, with every other setting at its default.
  pub fn new(bucket: impl Into<String>) -> Self {
    Self {
      bucket: bucket.into(),
      from_env: false,
      region: None,
      endpoint: None,
      credentials: None,
      allow_http: false,
      virtual_hosted_style: false,
    }
  }

  /// The bucket `bucket`, reading `AWS_REGION`, `AWS_ENDPOINT`, `AWS_ACCESS_KEY_ID`,
  /// `AWS_SECRET_ACCESS_KEY` and the other `AWS_*` variables `object_store` knows; the `with_*`
  /// settings override them.
  pub fn from_env(bucket: impl Into<String>) -> Self {
    Self {
      from_env: true,
      ..Self::new(bucket)
    }
  }

  /// The region (`us-east-1` by default).
  #[must_use]
  pub fn with_region(mut self, region: impl Into<String>) -> Self {
    self.region = Some(region.into());
    self
  }

  /// A custom endpoint, for S3-compatible services (`http://localhost:9000`).
  #[must_use]
  pub fn with_endpoint(mut self, endpoint: impl Into<String>) -> Self {
    self.endpoint = Some(endpoint.into());
    self
  }

  /// Static credentials.
  #[must_use]
  pub fn with_credentials(
    mut self,
    access_key_id: impl Into<String>,
    secret_access_key: impl Into<String>,
  ) -> Self {
    self.credentials = Some((access_key_id.into(), secret_access_key.into()));
    self
  }

  /// Allows a plain `http://` endpoint (local S3-compatible services).
  #[must_use]
  pub fn with_allow_http(mut self, allow: bool) -> Self {
    self.allow_http = allow;
    self
  }

  /// Uses virtual-hosted-style requests (`bucket.host/key`) instead of path-style
  /// (`host/bucket/key`).
  #[must_use]
  pub fn with_virtual_hosted_style(mut self, virtual_hosted: bool) -> Self {
    self.virtual_hosted_style = virtual_hosted;
    self
  }

  /// The bucket.
  pub fn bucket(&self) -> &str {
    &self.bucket
  }
}

/// An [`ObjectStore`](crate::ObjectStore) over an S3 bucket. Keys are S3 keys, verbatim;
/// signed URLs are SigV4 presigned URLs (at most 7 days).
#[derive(Clone, Debug)]
pub struct S3Store {
  backend: Backend,
  bucket: String,
}

impl S3Store {
  /// S3's smallest multipart part, except the last (5 MiB): the floor of
  /// [`with_part_size`](Self::with_part_size).
  pub const MIN_PART_SIZE: usize = 5 * 1024 * 1024;

  /// A store for `config`'s bucket. No request is made until the first operation.
  ///
  /// # Errors
  ///
  /// [`StorageError::Config`] when the configuration is incomplete or invalid (no bucket, a
  /// malformed endpoint).
  pub fn new(config: &S3Config) -> Result<Self, StorageError> {
    if config.bucket.is_empty() {
      return Err(StorageError::Config {
        message: "S3 bucket name is empty".into(),
      });
    }
    let mut builder = if config.from_env {
      AmazonS3Builder::from_env()
    } else {
      AmazonS3Builder::new()
    };
    builder = builder
      .with_bucket_name(&config.bucket)
      .with_allow_http(config.allow_http)
      .with_virtual_hosted_style_request(config.virtual_hosted_style);
    if let Some(region) = &config.region {
      builder = builder.with_region(region);
    }
    if let Some(endpoint) = &config.endpoint {
      builder = builder.with_endpoint(endpoint);
    }
    if let Some((id, secret)) = &config.credentials {
      builder = builder
        .with_access_key_id(id)
        .with_secret_access_key(secret);
    }
    let s3 = Arc::new(builder.build().map_err(|e| StorageError::Config {
      message: e.to_string(),
    })?);
    let mut backend = Backend::new(s3.clone(), KeyMapping::Verbatim);
    backend.set_signing(Signing::Presign(s3));
    backend.set_part_size(DEFAULT_PART_SIZE.max(Self::MIN_PART_SIZE));
    Ok(Self {
      backend,
      bucket: config.bucket.clone(),
    })
  }

  /// The bucket.
  pub fn bucket(&self) -> &str {
    &self.bucket
  }

  /// Sends streamed uploads in parts of `bytes` bytes, at least
  /// [`MIN_PART_SIZE`](Self::MIN_PART_SIZE).
  #[must_use]
  pub fn with_part_size(mut self, bytes: usize) -> Self {
    self.backend.set_part_size(bytes.max(Self::MIN_PART_SIZE));
    self
  }

  /// The part size of streamed uploads, in bytes.
  pub fn part_size(&self) -> usize {
    self.backend.part_size()
  }

  /// The layer: an [`S3Store`] for `config` as the [`ObjectStorage`](crate::ObjectStorage)
  /// capability, built on each layer build.
  pub fn layer(config: S3Config) -> Layer<ObjectStorageEnv, StorageError> {
    object_storage_layer(move || Self::new(&config))
  }
}

delegate_object_store!(S3Store);

#[cfg(test)]
mod tests {
  use super::*;

  use std::time::Duration;

  use id_effect::{Cause, Exit};

  use crate::{ObjectKey, ObjectStore, SignedMethod};

  fn minio() -> S3Config {
    S3Config::new("statements")
      .with_region("eu-west-1")
      .with_endpoint("http://localhost:9000")
      .with_allow_http(true)
      .with_credentials("minio", "minio-secret")
  }

  #[test]
  fn presigned_urls_are_signed_offline_and_carry_the_expiry() {
    let store = S3Store::new(&minio()).unwrap();
    let key = ObjectKey::new("2026/10/acct-1.csv").unwrap();
    let url = match store
      .signed_url(SignedMethod::Get, &key, Duration::from_secs(300))
      .block_on()
    {
      Exit::Success(url) => url,
      Exit::Failure(c) => panic!("signing failed: {c:?}"),
    };
    assert!(
      url
        .as_str()
        .starts_with("http://localhost:9000/statements/2026/10/acct-1.csv?"),
      "{url}"
    );
    let query: Vec<(String, String)> = url.query_pairs().into_owned().collect();
    assert!(
      query.contains(&("X-Amz-Expires".into(), "300".into())),
      "{query:?}"
    );
    assert!(
      query.iter().any(|(k, _)| k == "X-Amz-Signature"),
      "{query:?}"
    );
  }

  #[test]
  fn config_debug_hides_the_secret_and_bad_configs_fail_typed() {
    let s = format!("{:?}", minio());
    assert!(s.contains("minio") && !s.contains("minio-secret"), "{s}");
    let err = S3Store::new(&S3Config::new("")).unwrap_err();
    assert!(matches!(err, StorageError::Config { .. }), "{err:?}");
    let layer = S3Store::layer(S3Config::new(""));
    assert!(matches!(
      layer.build().block_on(),
      Exit::Failure(Cause::Fail(StorageError::Config { .. }))
    ));
  }

  #[test]
  fn part_size_has_the_s3_floor() {
    let store = S3Store::new(&minio().with_virtual_hosted_style(false)).unwrap();
    assert_eq!(store.part_size(), DEFAULT_PART_SIZE);
    assert_eq!(store.bucket(), "statements");
    assert_eq!(
      store.clone().with_part_size(1).part_size(),
      S3Store::MIN_PART_SIZE
    );
    assert_eq!(store.with_part_size(64 << 20).part_size(), 64 << 20);
    assert!(S3Store::new(&S3Config::from_env("b").with_region("us-east-1")).is_ok());
  }
}

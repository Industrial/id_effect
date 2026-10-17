//! [`S3`]: an S3-compatible object store ([`S3Container`]).
//!
//! The image is SeaweedFS (`chrislusf/seaweedfs`), pinned by digest (release decision D11): the
//! MinIO community images are no longer pullable (`minio/minio` on Docker Hub and
//! `quay.io/minio/minio` both refused anonymous pulls when this was pinned, 2026-10-17), and
//! SeaweedFS is Apache-2.0, publishes multi-arch release tags on Docker Hub and implements the
//! S3 API the tests need: SigV4 (headers and presigned query strings, with expiry), multipart
//! uploads and `ListObjectsV2`.

use std::time::Duration;

use super::{Container, ContainerImage, ContainerSpec, DEFAULT_READY_TIMEOUT, Wait};

/// The S3 API's container port.
pub const S3_PORT: u16 = 8333;

/// The image name.
pub const S3_IMAGE: &str = "chrislusf/seaweedfs";

/// The default tag: SeaweedFS 4.48, pinned to the digest of its multi-arch index (pulled
/// 2026-10-17).
pub const S3_TAG: &str =
  "4.48@sha256:4e61d15fd35994cb1e43e1e553dff106794841fd9a99ade2fc8c8bfce4d7872d";

/// Where the identities file is copied in the container.
const S3_CONFIG: &str = "/etc/seaweedfs/s3.json";

/// An S3-compatible server (SeaweedFS [`S3_TAG`]: master, volume, filer and S3 gateway in one
/// process, data on a tmpfs) with one identity allowed everything, access key `id-effect`,
/// secret `id-effect-secret`. Requests must be SigV4-signed (any region); there are no buckets
/// until a client creates one (`PUT /<bucket>`). Ready once the S3 gateway logs that it started.
#[derive(Clone, Debug)]
pub struct S3 {
  tag: String,
  access_key_id: String,
  secret_access_key: String,
  ready_timeout: Duration,
}

impl Default for S3 {
  fn default() -> Self {
    Self::new()
  }
}

impl S3 {
  /// `chrislusf/seaweedfs` at [`S3_TAG`], access key `id-effect`, secret `id-effect-secret`.
  pub fn new() -> Self {
    Self {
      tag: S3_TAG.into(),
      access_key_id: "id-effect".into(),
      secret_access_key: "id-effect-secret".into(),
      ready_timeout: DEFAULT_READY_TIMEOUT,
    }
  }

  /// The image tag (a SeaweedFS release, optionally `tag@sha256:digest`).
  pub fn tag(mut self, tag: impl Into<String>) -> Self {
    self.tag = tag.into();
    self
  }

  /// The identity's credentials.
  pub fn credentials(
    mut self,
    access_key_id: impl Into<String>,
    secret_access_key: impl Into<String>,
  ) -> Self {
    self.access_key_id = access_key_id.into();
    self.secret_access_key = secret_access_key.into();
    self
  }

  /// Bounds the readiness wait ([`ContainerSpec::ready_timeout`]).
  pub fn ready_timeout(mut self, timeout: Duration) -> Self {
    self.ready_timeout = timeout;
    self
  }

  /// The SeaweedFS identities file: one identity with every action.
  fn identities(&self) -> String {
    format!(
      r#"{{"identities":[{{"name":"id-effect","credentials":[{{"accessKey":{},"secretKey":{}}}],"actions":["Admin","Read","Write","List","Tagging"]}}]}}"#,
      json_string(&self.access_key_id),
      json_string(&self.secret_access_key)
    )
  }
}

/// `s` as a JSON string literal.
fn json_string(s: &str) -> String {
  let mut out = String::with_capacity(s.len() + 2);
  out.push('"');
  for c in s.chars() {
    match c {
      '"' => out.push_str("\\\""),
      '\\' => out.push_str("\\\\"),
      c if c.is_control() => out.push_str(&format!("\\u{:04x}", u32::from(c))),
      c => out.push(c),
    }
  }
  out.push('"');
  out
}

impl ContainerImage for S3 {
  type Running = S3Container;

  fn spec(&self) -> ContainerSpec {
    ContainerSpec::new(S3_IMAGE, &self.tag)
      .port(S3_PORT)
      .file(S3_CONFIG, self.identities())
      .cmd([
        "server".to_owned(),
        "-s3".to_owned(),
        format!("-s3.config={S3_CONFIG}"),
        // Small volumes on the tmpfs, and a quick leader election for the single master.
        "-master.volumeSizeLimitMB=64".to_owned(),
        "-master.electionTimeout=1s".to_owned(),
      ])
      .tmpfs("/data")
      .wait(Wait::stderr("Start Seaweed S3 API Server"))
      .ready_timeout(self.ready_timeout)
  }

  fn running(&self, container: Container) -> S3Container {
    S3Container {
      port: container.mapped_port(S3_PORT),
      container,
      access_key_id: self.access_key_id.clone(),
      secret_access_key: self.secret_access_key.clone(),
    }
  }

  fn container(running: &S3Container) -> &Container {
    &running.container
  }
}

/// A started, ready S3-compatible server: the capability [`S3::layer`](ContainerImage::layer)
/// provides.
#[derive(Clone)]
pub struct S3Container {
  container: Container,
  port: u16,
  access_key_id: String,
  secret_access_key: String,
}

impl std::fmt::Debug for S3Container {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("S3Container")
      .field("container", &self.container)
      .field("port", &self.port)
      .field("access_key_id", &self.access_key_id)
      .finish_non_exhaustive()
  }
}

impl S3Container {
  /// `http://host:port`, the endpoint for path-style requests.
  pub fn endpoint(&self) -> String {
    format!("http://{}:{}", self.container.host(), self.port)
  }

  /// The access key id.
  pub fn access_key_id(&self) -> &str {
    &self.access_key_id
  }

  /// The secret access key.
  pub fn secret_access_key(&self) -> &str {
    &self.secret_access_key
  }

  /// The host.
  pub fn host(&self) -> &str {
    self.container.host()
  }

  /// The host port of [`S3_PORT`].
  pub fn port(&self) -> u16 {
    self.port
  }

  /// The container.
  pub fn container(&self) -> &Container {
    &self.container
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn the_builder_shapes_the_spec() {
    assert_eq!(
      S3::default().spec().image(),
      format!("chrislusf/seaweedfs:{S3_TAG}")
    );
    assert!(S3_TAG.contains("@sha256:"), "pinned by digest (D11)");
    let spec = S3::new()
      .tag("4.47")
      .ready_timeout(Duration::from_secs(9))
      .spec();
    assert_eq!(spec.image(), "chrislusf/seaweedfs:4.47");
    assert_eq!(spec.ports(), [S3_PORT]);
  }

  #[test]
  fn the_identities_file_is_json_with_the_credentials() {
    let s3 = S3::new().credentials("key", r#"se"c\ret"#);
    assert_eq!(
      s3.identities(),
      r#"{"identities":[{"name":"id-effect","credentials":[{"accessKey":"key","secretKey":"se\"c\\ret"}],"actions":["Admin","Read","Write","List","Tagging"]}]}"#
    );
    assert_eq!(json_string("a\nb"), r#""a\u000ab""#);
  }
}

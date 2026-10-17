//! Signed, expiring URLs for the backends that have no URL signing of their own (local file
//! system, in-memory): [`UrlSigner`] issues HMAC-SHA256 URLs under a base URL your service serves,
//! and verifies them when a request comes back (web.storage#7).
//!
//! A URL is `<base>/<key segments>?expires=<unix seconds>&signature=<hex>`; the signature covers
//! the method, the key and the expiry, so a URL issued for `GET` does not authorize a `PUT`, and
//! editing the key or the expiry invalidates it.
//!
//! ```
//! use std::time::{Duration, SystemTime};
//!
//! use id_effect_storage::{ObjectKey, SignatureError, SignedMethod, UrlSigner};
//!
//! let base = "https://files.example.com/objects/".parse().unwrap();
//! let signer = UrlSigner::new(*b"0123456789abcdef0123456789abcdef", base).unwrap();
//! let key = ObjectKey::new("invoices/2026/10.pdf").unwrap();
//! let now = SystemTime::now();
//! let url = signer.sign(SignedMethod::Get, &key, now + Duration::from_secs(60));
//!
//! assert_eq!(signer.verify_at(SignedMethod::Get, &url, now), Ok(key));
//! assert_eq!(
//!   signer.verify_at(SignedMethod::Put, &url, now),
//!   Err(SignatureError::BadSignature)
//! );
//! assert_eq!(
//!   signer.verify_at(SignedMethod::Get, &url, now + Duration::from_secs(120)),
//!   Err(SignatureError::Expired)
//! );
//! ```

use std::fmt;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use hmac::{Hmac, Mac};
use percent_encoding::percent_decode_str;
use sha2::Sha256;
use url::Url;

use crate::ObjectKey;
use crate::store::StorageError;

type HmacSha256 = Hmac<Sha256>;

/// The HTTP method a signed URL authorizes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SignedMethod {
  /// Download the object.
  Get,
  /// Upload the object.
  Put,
}

impl SignedMethod {
  /// The method name, as sent on the wire.
  pub fn as_str(self) -> &'static str {
    match self {
      SignedMethod::Get => "GET",
      SignedMethod::Put => "PUT",
    }
  }
}

impl fmt::Display for SignedMethod {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.write_str(self.as_str())
  }
}

/// Why [`UrlSigner::verify`] rejected a URL.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum SignatureError {
  /// The URL is not under the signer's base URL.
  NotUnderBase,
  /// The URL lacks a valid key, `expires` or `signature`.
  Malformed,
  /// The signature does not match the method, key and expiry.
  BadSignature,
  /// The signature is valid but the URL has expired.
  Expired,
}

impl fmt::Display for SignatureError {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.write_str(match self {
      SignatureError::NotUnderBase => "signed URL is not under the signer's base URL",
      SignatureError::Malformed => "signed URL lacks a valid key, `expires` or `signature`",
      SignatureError::BadSignature => "signed URL signature does not match",
      SignatureError::Expired => "signed URL has expired",
    })
  }
}

impl std::error::Error for SignatureError {}

/// Issues and verifies HMAC-SHA256 signed, expiring URLs under a base URL.
///
/// Cloning is cheap (the secret is shared). The secret never appears in `Debug` output.
#[derive(Clone)]
pub struct UrlSigner {
  secret: Arc<[u8]>,
  base: Url,
}

impl fmt::Debug for UrlSigner {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("UrlSigner")
      .field("base", &self.base.as_str())
      .finish_non_exhaustive()
  }
}

impl UrlSigner {
  /// Shortest accepted secret, in bytes (the HMAC-SHA256 output size).
  pub const MIN_SECRET_LEN: usize = 32;

  /// A signer for URLs under `base` (for example `https://files.example.com/objects/`).
  ///
  /// # Errors
  ///
  /// [`StorageError::Config`] when `secret` is shorter than [`MIN_SECRET_LEN`](Self::MIN_SECRET_LEN)
  /// bytes or `base` cannot carry a path (`mailto:`, `data:`).
  pub fn new(secret: impl Into<Vec<u8>>, base: Url) -> Result<Self, StorageError> {
    let secret = secret.into();
    if secret.len() < Self::MIN_SECRET_LEN {
      return Err(StorageError::Config {
        message: format!(
          "URL signing secret is {} bytes, shorter than {}",
          secret.len(),
          Self::MIN_SECRET_LEN
        ),
      });
    }
    if base.cannot_be_a_base() {
      return Err(StorageError::Config {
        message: format!("URL signing base {base} cannot carry a path"),
      });
    }
    Ok(Self {
      secret: secret.into(),
      base,
    })
  }

  /// The base URL signed URLs live under.
  pub fn base(&self) -> &Url {
    &self.base
  }

  /// A URL authorizing `method` on `key` until `expires_at` (whole seconds, rounded down).
  pub fn sign(&self, method: SignedMethod, key: &ObjectKey, expires_at: SystemTime) -> Url {
    let expires = unix_secs(expires_at);
    let mut url = self.base.clone();
    url.set_query(None);
    url.set_fragment(None);
    if let Ok(mut path) = url.path_segments_mut() {
      path.pop_if_empty().extend(key.segments());
    }
    url
      .query_pairs_mut()
      .append_pair("expires", &expires.to_string())
      .append_pair("signature", &hex(&self.mac(method, key, expires)));
    url
  }

  /// [`verify_at`](Self::verify_at) against the system clock.
  ///
  /// # Errors
  ///
  /// See [`verify_at`](Self::verify_at).
  pub fn verify(&self, method: SignedMethod, url: &Url) -> Result<ObjectKey, SignatureError> {
    self.verify_at(method, url, SystemTime::now())
  }

  /// The key `url` authorizes `method` on, if its signature is valid and it has not expired
  /// at `now`.
  ///
  /// # Errors
  ///
  /// [`SignatureError::NotUnderBase`] for a URL of another origin or path,
  /// [`SignatureError::Malformed`] when the key, `expires` or `signature` is missing or invalid,
  /// [`SignatureError::BadSignature`] when the signature does not match (checked in constant
  /// time), and [`SignatureError::Expired`] after the expiry.
  pub fn verify_at(
    &self,
    method: SignedMethod,
    url: &Url,
    now: SystemTime,
  ) -> Result<ObjectKey, SignatureError> {
    let key = self.key_of(url)?;
    let (mut expires, mut signature) = (None, None);
    for (name, value) in url.query_pairs() {
      match &*name {
        "expires" => {
          expires = Some(
            value
              .parse::<u64>()
              .map_err(|_| SignatureError::Malformed)?,
          )
        }
        "signature" => signature = Some(unhex(&value).ok_or(SignatureError::Malformed)?),
        _ => {}
      }
    }
    let (Some(expires), Some(signature)) = (expires, signature) else {
      return Err(SignatureError::Malformed);
    };
    self
      .hmac(method, &key, expires)
      .verify_slice(&signature)
      .map_err(|_| SignatureError::BadSignature)?;
    if unix_secs(now) > expires {
      return Err(SignatureError::Expired);
    }
    Ok(key)
  }

  /// The object key in `url`'s path below the base path.
  fn key_of(&self, url: &Url) -> Result<ObjectKey, SignatureError> {
    if url.origin() != self.base.origin() {
      return Err(SignatureError::NotUnderBase);
    }
    let base: Vec<&str> = self
      .base
      .path_segments()
      .map(|s| s.filter(|seg| !seg.is_empty()).collect())
      .unwrap_or_default();
    let mut segments = url.path_segments().ok_or(SignatureError::NotUnderBase)?;
    for expected in &base {
      if segments.next() != Some(expected) {
        return Err(SignatureError::NotUnderBase);
      }
    }
    let decoded = segments
      .map(|seg| percent_decode_str(seg).decode_utf8())
      .collect::<Result<Vec<_>, _>>()
      .map_err(|_| SignatureError::Malformed)?;
    ObjectKey::new(decoded.join("/")).map_err(|_| SignatureError::Malformed)
  }

  fn hmac(&self, method: SignedMethod, key: &ObjectKey, expires: u64) -> HmacSha256 {
    // `new_from_slice` accepts any key length for HMAC.
    let mut mac = HmacSha256::new_from_slice(&self.secret).unwrap_or_else(|_| unreachable!());
    mac.update(format!("{method}\n{key}\n{expires}").as_bytes());
    mac
  }

  fn mac(&self, method: SignedMethod, key: &ObjectKey, expires: u64) -> Vec<u8> {
    self
      .hmac(method, key, expires)
      .finalize()
      .into_bytes()
      .to_vec()
  }
}

/// Seconds since the Unix epoch (0 before it).
fn unix_secs(t: SystemTime) -> u64 {
  t.duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

fn hex(bytes: &[u8]) -> String {
  use fmt::Write as _;
  let mut out = String::with_capacity(bytes.len() * 2);
  for b in bytes {
    let _ = write!(out, "{b:02x}");
  }
  out
}

fn unhex(s: &str) -> Option<Vec<u8>> {
  if !s.len().is_multiple_of(2) {
    return None;
  }
  (0..s.len())
    .step_by(2)
    .map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok())
    .collect()
}

#[cfg(test)]
mod tests {
  use super::*;

  use std::time::Duration;

  const SECRET: [u8; 32] = *b"0123456789abcdef0123456789abcdef";

  fn signer(base: &str) -> UrlSigner {
    UrlSigner::new(SECRET, base.parse().unwrap()).unwrap()
  }

  fn key(k: &str) -> ObjectKey {
    ObjectKey::new(k).unwrap()
  }

  #[test]
  fn signed_url_expires() {
    let signer = signer("https://files.example.com/objects/");
    let at = UNIX_EPOCH + Duration::from_secs(1_800_000_000);
    let url = signer.sign(SignedMethod::Get, &key("a/b.txt"), at);
    assert_eq!(
      signer.verify_at(SignedMethod::Get, &url, at - Duration::from_secs(1)),
      Ok(key("a/b.txt"))
    );
    assert_eq!(
      signer.verify_at(SignedMethod::Get, &url, at),
      Ok(key("a/b.txt"))
    );
    assert_eq!(
      signer.verify_at(SignedMethod::Get, &url, at + Duration::from_secs(1)),
      Err(SignatureError::Expired)
    );
  }

  #[test]
  fn url_is_under_the_base_and_round_trips_odd_keys() {
    let signer = signer("https://files.example.com/objects");
    let k = key("dir with space/ü#?%.txt");
    let url = signer.sign(
      SignedMethod::Put,
      &k,
      SystemTime::now() + Duration::from_secs(60),
    );
    assert!(
      url
        .as_str()
        .starts_with("https://files.example.com/objects/dir%20with%20space/"),
      "{url}"
    );
    assert_eq!(signer.verify(SignedMethod::Put, &url), Ok(k));
  }

  #[test]
  fn tampering_is_rejected() {
    let signer = signer("https://files.example.com/");
    let expires = SystemTime::now() + Duration::from_secs(60);
    let url = signer.sign(SignedMethod::Get, &key("a.txt"), expires);

    let mut other_key = url.clone();
    other_key.set_path("/b.txt");
    assert_eq!(
      signer.verify(SignedMethod::Get, &other_key),
      Err(SignatureError::BadSignature)
    );

    let mut later = url.clone();
    let query = url.query().unwrap().replace(
      &unix_secs(expires).to_string(),
      &(unix_secs(expires) + 3600).to_string(),
    );
    later.set_query(Some(&query));
    assert_eq!(
      signer.verify(SignedMethod::Get, &later),
      Err(SignatureError::BadSignature)
    );

    let other_secret = UrlSigner::new([7_u8; 32], signer.base().clone()).unwrap();
    assert_eq!(
      other_secret.verify(SignedMethod::Get, &url),
      Err(SignatureError::BadSignature)
    );
  }

  #[test]
  fn foreign_and_malformed_urls_are_rejected() {
    let signer = signer("https://files.example.com/objects/");
    let url = |s: &str| s.parse::<Url>().unwrap();
    let cases = [
      (
        "https://evil.example.com/objects/a?expires=1&signature=00",
        SignatureError::NotUnderBase,
      ),
      (
        "https://files.example.com/other/a?expires=1&signature=00",
        SignatureError::NotUnderBase,
      ),
      (
        "https://files.example.com/objects/a?signature=00",
        SignatureError::Malformed,
      ),
      (
        "https://files.example.com/objects/a?expires=x&signature=00",
        SignatureError::Malformed,
      ),
      (
        "https://files.example.com/objects/a?expires=1&signature=0",
        SignatureError::Malformed,
      ),
      (
        "https://files.example.com/objects/a?expires=1&signature=zz",
        SignatureError::Malformed,
      ),
      (
        "https://files.example.com/objects/a?expires=1",
        SignatureError::Malformed,
      ),
      (
        "https://files.example.com/objects/?expires=1&signature=00",
        SignatureError::Malformed,
      ),
      (
        "https://files.example.com/objects/%FF?expires=1&signature=00",
        SignatureError::Malformed,
      ),
    ];
    for (u, err) in cases {
      assert_eq!(signer.verify(SignedMethod::Get, &url(u)), Err(err), "{u}");
    }
  }

  #[test]
  fn new_rejects_short_secrets_and_pathless_bases() {
    let base: Url = "https://files.example.com/".parse().unwrap();
    assert!(matches!(
      UrlSigner::new(*b"short", base),
      Err(StorageError::Config { .. })
    ));
    assert!(matches!(
      UrlSigner::new(SECRET, "mailto:a@example.com".parse().unwrap()),
      Err(StorageError::Config { .. })
    ));
  }

  #[test]
  fn debug_hides_the_secret_and_errors_display() {
    let s = format!("{:?}", signer("https://files.example.com/"));
    assert!(
      s.contains("files.example.com") && !s.contains("0123"),
      "{s}"
    );
    assert_eq!(SignedMethod::Put.to_string(), "PUT");
    for e in [
      SignatureError::NotUnderBase,
      SignatureError::Malformed,
      SignatureError::BadSignature,
      SignatureError::Expired,
    ] {
      assert!(!e.to_string().is_empty());
    }
    assert_eq!(unix_secs(UNIX_EPOCH - Duration::from_secs(5)), 0);
  }
}

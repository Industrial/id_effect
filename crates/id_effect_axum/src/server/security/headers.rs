//! Default security response headers: HSTS, `X-Frame-Options`, `X-Content-Type-Options` and
//! `Referrer-Policy`. Each is set only when the handler has not set it.

use std::time::Duration;

use axum::http::{HeaderMap, HeaderName, HeaderValue, header};

use super::SecurityError;

/// Two years, the HSTS preload list minimum.
const HSTS_MAX_AGE: Duration = Duration::from_secs(63_072_000);

/// `X-Frame-Options` value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FrameOptions {
  /// `DENY`: never render in a frame.
  Deny,
  /// `SAMEORIGIN`: only in frames of the same origin.
  SameOrigin,
}

/// The default security headers added to every response.
#[derive(Clone, Debug)]
pub struct SecurityHeaders {
  hsts: Option<HeaderValue>,
  frame_options: Option<FrameOptions>,
  nosniff: bool,
  referrer_policy: Option<HeaderValue>,
}

impl Default for SecurityHeaders {
  fn default() -> Self {
    Self::recommended()
  }
}

impl SecurityHeaders {
  /// `Strict-Transport-Security: max-age=63072000; includeSubDomains`,
  /// `X-Frame-Options: DENY`, `X-Content-Type-Options: nosniff`,
  /// `Referrer-Policy: strict-origin-when-cross-origin`.
  pub fn recommended() -> Self {
    Self {
      hsts: None,
      frame_options: Some(FrameOptions::Deny),
      nosniff: true,
      referrer_policy: Some(HeaderValue::from_static("strict-origin-when-cross-origin")),
    }
    .hsts(HSTS_MAX_AGE, true, false)
  }

  /// No headers at all; add the ones you want.
  pub fn none() -> Self {
    Self {
      hsts: None,
      frame_options: None,
      nosniff: false,
      referrer_policy: None,
    }
  }

  /// `Strict-Transport-Security` with `max_age`, optionally `includeSubDomains` and `preload`.
  pub fn hsts(mut self, max_age: Duration, include_subdomains: bool, preload: bool) -> Self {
    let mut value = format!("max-age={}", max_age.as_secs());
    if include_subdomains {
      value.push_str("; includeSubDomains");
    }
    if preload {
      value.push_str("; preload");
    }
    self.hsts = HeaderValue::from_str(&value).ok();
    self
  }

  /// No `Strict-Transport-Security` (e.g. a TLS-terminating proxy already sets it).
  pub fn without_hsts(mut self) -> Self {
    self.hsts = None;
    self
  }

  /// `X-Frame-Options` value.
  pub fn frame_options(mut self, value: FrameOptions) -> Self {
    self.frame_options = Some(value);
    self
  }

  /// No `X-Frame-Options`.
  pub fn without_frame_options(mut self) -> Self {
    self.frame_options = None;
    self
  }

  /// `X-Content-Type-Options: nosniff` on or off.
  pub fn nosniff(mut self, on: bool) -> Self {
    self.nosniff = on;
    self
  }

  /// `Referrer-Policy` value (e.g. `no-referrer`).
  pub fn referrer_policy(mut self, value: &str) -> Result<Self, SecurityError> {
    let parsed = HeaderValue::from_str(value).map_err(|_| SecurityError::InvalidHeaderValue {
      header: "referrer-policy",
      value: value.to_owned(),
    })?;
    self.referrer_policy = Some(parsed);
    Ok(self)
  }

  /// No `Referrer-Policy`.
  pub fn without_referrer_policy(mut self) -> Self {
    self.referrer_policy = None;
    self
  }

  /// Insert each configured header that `headers` does not already contain.
  pub fn apply(&self, headers: &mut HeaderMap) {
    let frame = self.frame_options.map(|f| match f {
      FrameOptions::Deny => HeaderValue::from_static("DENY"),
      FrameOptions::SameOrigin => HeaderValue::from_static("SAMEORIGIN"),
    });
    let nosniff = self.nosniff.then(|| HeaderValue::from_static("nosniff"));
    let entries: [(HeaderName, Option<HeaderValue>); 4] = [
      (header::STRICT_TRANSPORT_SECURITY, self.hsts.clone()),
      (header::X_FRAME_OPTIONS, frame),
      (header::X_CONTENT_TYPE_OPTIONS, nosniff),
      (header::REFERRER_POLICY, self.referrer_policy.clone()),
    ];
    for (name, value) in entries {
      if let Some(value) = value {
        headers.entry(name).or_insert(value);
      }
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn recommended_headers_values() {
    let mut headers = HeaderMap::new();
    SecurityHeaders::recommended().apply(&mut headers);
    assert_eq!(
      headers[header::STRICT_TRANSPORT_SECURITY],
      "max-age=63072000; includeSubDomains"
    );
    assert_eq!(headers[header::X_FRAME_OPTIONS], "DENY");
    assert_eq!(headers[header::X_CONTENT_TYPE_OPTIONS], "nosniff");
    assert_eq!(
      headers[header::REFERRER_POLICY],
      "strict-origin-when-cross-origin"
    );
  }

  #[test]
  fn none_sets_nothing_and_builders_compose() {
    let mut headers = HeaderMap::new();
    SecurityHeaders::none().apply(&mut headers);
    assert!(headers.is_empty());
    SecurityHeaders::none()
      .frame_options(FrameOptions::SameOrigin)
      .hsts(Duration::from_secs(60), false, true)
      .apply(&mut headers);
    assert_eq!(headers[header::X_FRAME_OPTIONS], "SAMEORIGIN");
    assert_eq!(
      headers[header::STRICT_TRANSPORT_SECURITY],
      "max-age=60; preload"
    );
  }

  #[test]
  fn referrer_policy_rejects_invalid_value() {
    assert!(matches!(
      SecurityHeaders::recommended().referrer_policy("bad\nvalue"),
      Err(SecurityError::InvalidHeaderValue { .. })
    ));
    let mut headers = HeaderMap::new();
    SecurityHeaders::recommended()
      .referrer_policy("no-referrer")
      .unwrap()
      .without_hsts()
      .apply(&mut headers);
    assert_eq!(headers[header::REFERRER_POLICY], "no-referrer");
    assert!(headers.get(header::STRICT_TRANSPORT_SECURITY).is_none());
  }
}

//! Default security response headers: HSTS, `X-Frame-Options`, `X-Content-Type-Options`,
//! `Referrer-Policy`, `Cross-Origin-Opener-Policy`, `Cross-Origin-Resource-Policy` and
//! `Permissions-Policy`. Each is set only when the handler has not set it.

use std::time::Duration;

use axum::http::{HeaderMap, HeaderName, HeaderValue, header};

use super::SecurityError;

/// Two years, the HSTS preload list minimum.
const HSTS_MAX_AGE: Duration = Duration::from_secs(63_072_000);

/// `Cross-Origin-Opener-Policy` (not among `http`'s standard header names).
const COOP: &str = "cross-origin-opener-policy";

/// `Cross-Origin-Resource-Policy`.
const CORP: &str = "cross-origin-resource-policy";

/// `Permissions-Policy`.
const PERMISSIONS_POLICY: &str = "permissions-policy";

/// [`COOP`], [`CORP`] and [`PERMISSIONS_POLICY`] as header names, built at compile time.
const COOP_NAME: HeaderName = HeaderName::from_static(COOP);
const CORP_NAME: HeaderName = HeaderName::from_static(CORP);
const PERMISSIONS_POLICY_NAME: HeaderName = HeaderName::from_static(PERMISSIONS_POLICY);

/// Powerful features denied to every origin by the recommended `Permissions-Policy`.
const DENIED_FEATURES: &str = "camera=(), microphone=(), geolocation=(), payment=(), usb=()";

fn parse(header: &'static str, value: &str) -> Result<HeaderValue, SecurityError> {
  HeaderValue::from_str(value).map_err(|_| SecurityError::InvalidHeaderValue {
    header,
    value: value.to_owned(),
  })
}

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
  opener_policy: Option<HeaderValue>,
  resource_policy: Option<HeaderValue>,
  permissions_policy: Option<HeaderValue>,
}

impl Default for SecurityHeaders {
  fn default() -> Self {
    Self::recommended()
  }
}

impl SecurityHeaders {
  /// `Strict-Transport-Security: max-age=63072000; includeSubDomains`,
  /// `X-Frame-Options: DENY`, `X-Content-Type-Options: nosniff`,
  /// `Referrer-Policy: strict-origin-when-cross-origin`,
  /// `Cross-Origin-Opener-Policy: same-origin`, `Cross-Origin-Resource-Policy: same-origin`,
  /// `Permissions-Policy: camera=(), microphone=(), geolocation=(), payment=(), usb=()`.
  pub fn recommended() -> Self {
    Self {
      hsts: None,
      frame_options: Some(FrameOptions::Deny),
      nosniff: true,
      referrer_policy: Some(HeaderValue::from_static("strict-origin-when-cross-origin")),
      opener_policy: Some(HeaderValue::from_static("same-origin")),
      resource_policy: Some(HeaderValue::from_static("same-origin")),
      permissions_policy: Some(HeaderValue::from_static(DENIED_FEATURES)),
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
      opener_policy: None,
      resource_policy: None,
      permissions_policy: None,
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
    self.referrer_policy = Some(parse("referrer-policy", value)?);
    Ok(self)
  }

  /// No `Referrer-Policy`.
  pub fn without_referrer_policy(mut self) -> Self {
    self.referrer_policy = None;
    self
  }

  /// `Cross-Origin-Opener-Policy` value (e.g. `same-origin-allow-popups` for OAuth popups).
  pub fn cross_origin_opener_policy(mut self, value: &str) -> Result<Self, SecurityError> {
    self.opener_policy = Some(parse(COOP, value)?);
    Ok(self)
  }

  /// No `Cross-Origin-Opener-Policy`.
  pub fn without_cross_origin_opener_policy(mut self) -> Self {
    self.opener_policy = None;
    self
  }

  /// `Cross-Origin-Resource-Policy` value (e.g. `cross-origin` for assets embedded elsewhere).
  pub fn cross_origin_resource_policy(mut self, value: &str) -> Result<Self, SecurityError> {
    self.resource_policy = Some(parse(CORP, value)?);
    Ok(self)
  }

  /// No `Cross-Origin-Resource-Policy`.
  pub fn without_cross_origin_resource_policy(mut self) -> Self {
    self.resource_policy = None;
    self
  }

  /// `Permissions-Policy` value (e.g. `camera=(self), microphone=()`).
  pub fn permissions_policy(mut self, value: &str) -> Result<Self, SecurityError> {
    self.permissions_policy = Some(parse(PERMISSIONS_POLICY, value)?);
    Ok(self)
  }

  /// No `Permissions-Policy`.
  pub fn without_permissions_policy(mut self) -> Self {
    self.permissions_policy = None;
    self
  }

  /// Insert each configured header that `headers` does not already contain.
  pub fn apply(&self, headers: &mut HeaderMap) {
    let frame = self.frame_options.map(|f| match f {
      FrameOptions::Deny => HeaderValue::from_static("DENY"),
      FrameOptions::SameOrigin => HeaderValue::from_static("SAMEORIGIN"),
    });
    let nosniff = self.nosniff.then(|| HeaderValue::from_static("nosniff"));
    let entries: [(HeaderName, Option<HeaderValue>); 7] = [
      (header::STRICT_TRANSPORT_SECURITY, self.hsts.clone()),
      (header::X_FRAME_OPTIONS, frame),
      (header::X_CONTENT_TYPE_OPTIONS, nosniff),
      (header::REFERRER_POLICY, self.referrer_policy.clone()),
      (COOP_NAME, self.opener_policy.clone()),
      (CORP_NAME, self.resource_policy.clone()),
      (PERMISSIONS_POLICY_NAME, self.permissions_policy.clone()),
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

  #[test]
  fn isolation_headers_override_or_drop() {
    let mut headers = HeaderMap::new();
    SecurityHeaders::recommended()
      .cross_origin_opener_policy("same-origin-allow-popups")
      .unwrap()
      .cross_origin_resource_policy("cross-origin")
      .unwrap()
      .permissions_policy("camera=(self)")
      .unwrap()
      .apply(&mut headers);
    assert_eq!(headers[COOP], "same-origin-allow-popups");
    assert_eq!(headers[CORP], "cross-origin");
    assert_eq!(headers[PERMISSIONS_POLICY], "camera=(self)");

    let mut headers = HeaderMap::new();
    SecurityHeaders::recommended()
      .without_cross_origin_opener_policy()
      .without_cross_origin_resource_policy()
      .without_permissions_policy()
      .apply(&mut headers);
    for name in [COOP, CORP, PERMISSIONS_POLICY] {
      assert!(headers.get(name).is_none(), "{name}");
    }
    assert!(matches!(
      SecurityHeaders::none().permissions_policy("bad\nvalue"),
      Err(SecurityError::InvalidHeaderValue {
        header: PERMISSIONS_POLICY,
        ..
      })
    ));
  }
}

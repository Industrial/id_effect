//! Response compression: [`CompressionLayer`] negotiates `Accept-Encoding` and compresses the
//! response body with gzip, brotli or deflate (tower-http's compression service underneath).
//!
//! By default every encoding is on, at the encoders' default quality, for responses whose body
//! is known to be at least [`CompressionLayer::DEFAULT_MIN_SIZE`] bytes. Images (except SVG),
//! gRPC and server-sent events are never compressed, nor is a response that already carries a
//! `Content-Encoding`.
//!
//! ```rust
//! use axum::{Router, routing::get};
//! use id_effect_axum::middleware::{CompressionLayer, CompressionLevel};
//!
//! let app: Router = Router::new()
//!   .route("/report", get(|| async { "a large report" }))
//!   .layer(CompressionLayer::new().quality(CompressionLevel::Best).min_size(256));
//! # let _ = app;
//! ```

use axum::http::Response;
use http_body::Body;
use tower_http::compression::Predicate;
use tower_http::compression::predicate::{NotForContentType, SizeAbove};
use tower_layer::Layer;

pub use tower_http::compression::CompressionLevel;

/// The [`Service`](tower_service::Service) produced by [`CompressionLayer`].
pub type CompressionService<S> = tower_http::compression::Compression<S, CompressionPredicate>;

/// Which responses [`CompressionLayer`] compresses: a body of at least `min_size` bytes that is
/// not an image (SVG excepted), gRPC or an event stream.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CompressionPredicate {
  min_size: u16,
}

impl Predicate for CompressionPredicate {
  fn should_compress<B>(&self, response: &Response<B>) -> bool
  where
    B: Body,
  {
    SizeAbove::new(self.min_size)
      .and(NotForContentType::GRPC)
      .and(NotForContentType::IMAGES)
      .and(NotForContentType::SSE)
      .should_compress(response)
  }
}

/// Compresses response bodies the client accepts (see the [module docs](self)).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CompressionLayer {
  gzip: bool,
  br: bool,
  deflate: bool,
  quality: CompressionLevel,
  min_size: u16,
}

impl Default for CompressionLayer {
  fn default() -> Self {
    Self::new()
  }
}

impl CompressionLayer {
  /// Smallest body compressed by default, in bytes: below it the encoding overhead outweighs
  /// the saving.
  pub const DEFAULT_MIN_SIZE: u16 = 860;

  /// gzip, brotli and deflate at default quality for bodies of at least
  /// [`Self::DEFAULT_MIN_SIZE`] bytes.
  pub fn new() -> Self {
    Self {
      gzip: true,
      br: true,
      deflate: true,
      quality: CompressionLevel::Default,
      min_size: Self::DEFAULT_MIN_SIZE,
    }
  }

  /// Offer gzip (default on).
  #[must_use]
  pub fn gzip(mut self, enable: bool) -> Self {
    self.gzip = enable;
    self
  }

  /// Offer brotli (default on).
  #[must_use]
  pub fn br(mut self, enable: bool) -> Self {
    self.br = enable;
    self
  }

  /// Offer deflate (default on).
  #[must_use]
  pub fn deflate(mut self, enable: bool) -> Self {
    self.deflate = enable;
    self
  }

  /// Compression quality for every encoding.
  #[must_use]
  pub fn quality(mut self, quality: CompressionLevel) -> Self {
    self.quality = quality;
    self
  }

  /// Compress only bodies of at least `bytes` bytes (a body of unknown length, e.g. a stream,
  /// is always a candidate).
  #[must_use]
  pub fn min_size(mut self, bytes: u16) -> Self {
    self.min_size = bytes;
    self
  }

  fn tower(&self) -> tower_http::compression::CompressionLayer<CompressionPredicate> {
    tower_http::compression::CompressionLayer::new()
      .gzip(self.gzip)
      .br(self.br)
      .deflate(self.deflate)
      .quality(self.quality)
      .compress_when(CompressionPredicate {
        min_size: self.min_size,
      })
  }
}

impl<S> Layer<S> for CompressionLayer {
  type Service = CompressionService<S>;

  fn layer(&self, inner: S) -> Self::Service {
    self.tower().layer(inner)
  }
}

//! Effect.ts-style `Config<T>` descriptor — a lazy, composable configuration description.
//!
//! A [`Config<T>`] is a *description* of what to load, and the crate's only reading API (Effect.ts
//! `Config`). Compose it with [`Config::with_default`], [`Config::map`], [`Config::all`],
//! [`Config::zip_with`], etc., then evaluate it with [`Config::run`] (an effect reading the
//! [`ConfigProviderService`] capability), [`Config::layer`] (a layer providing the loaded value as
//! a capability) or [`Config::load`] (synchronously, against a provider reference).
//!
//! # Errors accumulate
//!
//! [`Config::all`] (and [`Config::zip`] / [`Config::zip_with`]) loads every component and reports
//! **every** failure, not only the first, as Effect.ts `Config.all` does: two or more failures
//! come back as one [`ConfigError::And`]. [`Config::with_default`] and [`Config::or_else`] fall
//! back only when every failure is a missing key ([`ConfigError::is_missing_only`]).
//!
//! # Path conventions
//!
//! Path strings use `.` as the segment separator: `"server.host"` splits into
//! `["server", "host"]` which each [`crate::ConfigProvider`] then joins using its own
//! key delimiter (`_` for env, `.` for figment).
//!
//! # Example
//!
//! ```rust
//! use id_effect::Exit;
//! use id_effect_config::{Config, MapConfigProvider, config_env, ConfigError};
//!
//! let p = MapConfigProvider::from_pairs([
//!   ("HOST", "localhost"),
//!   ("PORT", "8080"),
//! ]);
//! let env = config_env(p);
//!
//! let host = Config::string("HOST")
//!   .run::<String, ConfigError, _, _>()
//!   .provide_all(env.clone())
//!   .block_on();
//! let port = Config::integer("PORT")
//!   .with_default(3000)
//!   .run::<i64, ConfigError, _, _>()
//!   .provide_all(env)
//!   .block_on();
//! assert!(matches!(host, Exit::Success(h) if h == "localhost"));
//! assert!(matches!(port, Exit::Success(8080)));
//! ```

use std::fmt;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

use ::id_effect::{Effect, HCons, HNil, Layer, Selector, fail, hlist, need, succeed};
use id_effect::LogLevel;
use id_effect::scheduling::duration::duration;
use url::Url;

use crate::ConfigEnv;
use crate::error::ConfigError;
use crate::provider::{ConfigProvider, ConfigProviderService};
use crate::secret::Secret;

// Shared loading function: takes a provider reference, returns a Result.
type LoadFn<T> = Arc<dyn Fn(&dyn ConfigProvider) -> Result<T, ConfigError> + Send + Sync>;

/// Runs `f` against the [`ConfigProviderService`] in the environment (a fused synchronous step).
fn with_provider<A, E, R, I>(
  f: impl FnOnce(&dyn ConfigProvider) -> Result<A, E> + Send + 'static,
) -> Effect<A, E, R>
where
  A: Send + 'static,
  E: Send + 'static,
  R: Selector<ConfigProviderService, I> + Send + Sync + 'static,
{
  need::<ConfigProviderService>().flat_map(move |service| match f(service.0.as_ref()) {
    Ok(a) => succeed(a),
    Err(e) => fail(e),
  })
}

// ── Internal adapter: prepends fixed segments to every path lookup ────────────

struct PrefixProvider<'a> {
  inner: &'a dyn ConfigProvider,
  prefix: Arc<Vec<String>>,
}

impl ConfigProvider for PrefixProvider<'_> {
  fn load_raw(&self, path: &[&str]) -> Result<Option<String>, ConfigError> {
    let mut full: Vec<String> = self.prefix.iter().cloned().collect();
    full.extend(path.iter().map(|s| (*s).to_string()));
    let full_refs: Vec<&str> = full.iter().map(String::as_str).collect();
    self.inner.load_raw(&full_refs)
  }

  fn seq_delim(&self) -> &'static str {
    self.inner.seq_delim()
  }
}

// ── Config<T> ─────────────────────────────────────────────────────────────────

/// A lazy, composable configuration descriptor (Effect.ts `Config<T>`).
///
/// Create with type-specific constructors ([`Config::string`], [`Config::integer`], …),
/// compose with combinators ([`Config::with_default`], [`Config::map`], [`Config::nested`], …),
/// and evaluate with [`Config::run`] (service-injected) or [`Config::load`] (direct provider).
pub struct Config<T: 'static> {
  loader: LoadFn<T>,
}

impl<T: 'static> fmt::Debug for Config<T> {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    write!(f, "Config<{}>", std::any::type_name::<T>())
  }
}

impl<T: 'static> Clone for Config<T> {
  fn clone(&self) -> Self {
    Self {
      loader: self.loader.clone(),
    }
  }
}

// ── Core impl (no Clone bound on T) ──────────────────────────────────────────

impl<T: Send + Sync + 'static> Config<T> {
  fn new(
    f: impl Fn(&dyn ConfigProvider) -> Result<T, ConfigError> + Send + Sync + 'static,
  ) -> Self {
    Self {
      loader: Arc::new(f),
    }
  }

  /// Evaluate against `provider` directly (synchronous; useful for tests).
  #[inline]
  pub fn load(&self, provider: &dyn ConfigProvider) -> Result<T, ConfigError> {
    (self.loader)(provider)
  }

  /// Evaluate this descriptor as an [`Effect`], pulling the provider from the environment.
  ///
  /// `R` only needs to contain [`ConfigProviderService`] (`R: Selector<ConfigProviderService, I>`,
  /// `I` inferred); callers compose whatever provider stack they like. See
  /// [`config_env`](crate::config_env) for building a minimal environment and
  /// [`provide_config_provider`](crate::provide_config_provider) for the layer.
  pub fn run<A, E, R, I>(&self) -> Effect<A, E, R>
  where
    A: From<T> + Send + 'static,
    E: From<ConfigError> + Send + 'static,
    R: Selector<ConfigProviderService, I> + Send + Sync + 'static,
  {
    let loader = self.loader.clone();
    with_provider(move |provider| (loader)(provider).map(A::from).map_err(E::from))
  }

  /// A [`Layer`] that loads this descriptor and provides the value as a capability (Effect.ts
  /// `Layer.effect(Tag, config)`).
  ///
  /// The layer needs the [`ConfigProviderService`] capability (`In = `[`ConfigEnv`]); feed it with
  /// a provider layer ([`provide_config_provider`](crate::provide_config_provider),
  /// [`ConfigProvider::layer`], ...) through `provide_to` / `provide_merge`. A failed load fails
  /// the build with `E::from(error)` (every accumulated failure in one [`ConfigError`]). Give the
  /// value a newtype ([`Config::map`]) so it is a distinct capability.
  ///
  /// ```rust
  /// use id_effect::Exit;
  /// use id_effect::{Effect, HCons, HNil, need};
  /// use id_effect_config::{Config, ConfigError, ConfigProvider, MapConfigProvider};
  ///
  /// struct Port(i64);
  ///
  /// let port = Config::integer("PORT").map(Port).layer::<ConfigError>();
  /// let app = MapConfigProvider::from_pairs([("PORT", "8080")])
  ///   .layer()
  ///   .provide_to(port);
  /// let program: Effect<i64, ConfigError, HCons<Port, HNil>> = need::<Port>().map(|p| p.0);
  /// assert!(matches!(program.provide_layer(app).block_on(), Exit::Success(8080)));
  /// ```
  pub fn layer<E>(&self) -> Layer<HCons<T, HNil>, E, ConfigEnv>
  where
    E: From<ConfigError> + Send + 'static,
  {
    let config = self.clone();
    Layer::effect(move || {
      config
        .run::<T, E, ConfigEnv, _>()
        .map(|value| hlist![value])
    })
  }

  /// Transform the loaded value (Effect `Config.map`).
  ///
  /// ```rust
  /// use id_effect_config::{Config, MapConfigProvider};
  ///
  /// let p = MapConfigProvider::from_pairs([("PORT", "8080")]);
  /// let port_u16: u16 = Config::integer("PORT").map(|n| n as u16).load(&p).unwrap();
  /// assert_eq!(port_u16, 8080u16);
  /// ```
  pub fn map<U: Send + Sync + 'static>(
    self,
    f: impl Fn(T) -> U + Send + Sync + 'static,
  ) -> Config<U> {
    let loader = self.loader;
    let f = Arc::new(f);
    Config {
      loader: Arc::new(move |p| {
        let t = (loader)(p)?;
        Ok(f(t))
      }),
    }
  }

  /// Transform the loaded value, allowing failure (Effect `Config.mapAttempt`).
  pub fn map_attempt<U: Send + Sync + 'static>(
    self,
    f: impl Fn(T) -> Result<U, ConfigError> + Send + Sync + 'static,
  ) -> Config<U> {
    let loader = self.loader;
    let f = Arc::new(f);
    Config {
      loader: Arc::new(move |p| {
        let t = (loader)(p)?;
        f(t)
      }),
    }
  }

  /// Wrap the loaded value in [`Secret`] so it is never printed (Effect `Config.secret`).
  ///
  /// ```rust
  /// use id_effect_config::{Config, MapConfigProvider};
  ///
  /// let p = MapConfigProvider::from_pairs([("API_KEY", "s3cr3t")]);
  /// let key = Config::string("API_KEY").secret().load(&p).unwrap();
  /// assert_eq!(format!("{key:?}"), "<redacted>");
  /// assert_eq!(key.expose(), "s3cr3t");
  /// ```
  pub fn secret(self) -> Config<Secret<T>> {
    let loader = self.loader;
    Config {
      loader: Arc::new(move |p| {
        (loader)(p)
          .map(Secret::new)
          .map_err(ConfigError::redact_values)
      }),
    }
  }

  /// Scope all path lookups under `prefix` segments (Effect `Config.nested`).
  ///
  /// `Config::string("PORT").nested("SERVER")` looks up `["SERVER", "PORT"]`.
  ///
  /// ```rust
  /// use id_effect_config::{Config, MapConfigProvider};
  ///
  /// let p = MapConfigProvider::from_pairs([("SERVER_HOST", "127.0.0.1")]);
  /// let host = Config::string("HOST").nested("SERVER").load(&p).unwrap();
  /// assert_eq!(host, "127.0.0.1");
  /// ```
  pub fn nested(self, prefix: impl Into<String>) -> Self {
    let prefix_segs: Arc<Vec<String>> = Arc::new(
      prefix
        .into()
        .split('.')
        .filter(|s| !s.is_empty())
        .map(String::from)
        .collect(),
    );
    let inner = self.loader;
    Self {
      loader: Arc::new(move |p| {
        let scoped = PrefixProvider {
          inner: p,
          prefix: prefix_segs.clone(),
        };
        inner(&scoped)
      }),
    }
  }
}

// ── Combinators that need T: Clone ────────────────────────────────────────────

impl<T: Clone + Send + Sync + 'static> Config<T> {
  /// Fall back to `default` when the key is missing (Effect `Config.withDefault`).
  ///
  /// Only missing data triggers the fallback ([`ConfigError::is_missing_only`], so a composite
  /// [`Config::all`] falls back when all of its failures are missing keys); parse errors still
  /// propagate.
  ///
  /// ```rust
  /// use id_effect_config::{Config, MapConfigProvider};
  ///
  /// let p = MapConfigProvider::from_pairs::<[(&str, &str); 0], _, _>([]);
  /// let port = Config::integer("PORT").with_default(3000).load(&p).unwrap();
  /// assert_eq!(port, 3000);
  /// ```
  pub fn with_default(self, default: T) -> Self {
    let loader = self.loader;
    Self {
      loader: Arc::new(move |p| match (loader)(p) {
        Ok(v) => Ok(v),
        Err(e) if e.is_missing_only() => Ok(default.clone()),
        Err(e) => Err(e),
      }),
    }
  }

  /// Validate the loaded value; produces `ConfigError::Invalid` when `predicate` is false.
  ///
  /// ```rust
  /// use id_effect_config::{Config, MapConfigProvider};
  ///
  /// let p = MapConfigProvider::from_pairs([("PORT", "99999")]);
  /// let err = Config::integer("PORT")
  ///   .validate("port must be 1–65535", |n| (1..=65535).contains(n))
  ///   .load(&p)
  ///   .unwrap_err();
  /// assert!(err.to_string().contains("port must be 1–65535"));
  /// ```
  pub fn validate(
    self,
    reason: &'static str,
    predicate: impl Fn(&T) -> bool + Send + Sync + 'static,
  ) -> Self {
    let loader = self.loader;
    let predicate = Arc::new(predicate);
    Self {
      loader: Arc::new(move |p| {
        let v = (loader)(p)?;
        if predicate(&v) {
          Ok(v)
        } else {
          Err(ConfigError::Invalid {
            path: String::new(),
            value: String::new(),
            reason: reason.to_string(),
          })
        }
      }),
    }
  }
}

// ── Type-specific constructors ────────────────────────────────────────────────

fn split_dotted(path: &str) -> Arc<Vec<String>> {
  Arc::new(
    path
      .split('.')
      .filter(|s| !s.is_empty())
      .map(String::from)
      .collect(),
  )
}

impl Config<String> {
  /// Load a required string (Effect `Config.string`).
  ///
  /// `path` may be dotted to express nesting: `"server.host"` → `["server", "host"]`.
  pub fn string(path: impl Into<String>) -> Self {
    let segs = split_dotted(&path.into());
    Self::new(move |p| {
      let refs: Vec<&str> = segs.iter().map(String::as_str).collect();
      let path_str = refs.join(".");
      match p.load_raw(&refs)? {
        None => Err(ConfigError::Missing { path: path_str }),
        Some(s) => Ok(s),
      }
    })
  }
}

impl Config<Option<String>> {
  /// Load an optional string; a missing key yields `None` (Effect `Config.option`).
  pub fn optional_string(path: impl Into<String>) -> Self {
    let segs = split_dotted(&path.into());
    Self::new(move |p| {
      let refs: Vec<&str> = segs.iter().map(String::as_str).collect();
      p.load_raw(&refs)
    })
  }
}

impl Config<f64> {
  /// Load a floating-point number (Effect `Config.number`).
  pub fn number(path: impl Into<String>) -> Self {
    let segs = split_dotted(&path.into());
    Self::new(move |p| {
      let refs: Vec<&str> = segs.iter().map(String::as_str).collect();
      let path_str = refs.join(".");
      let s = match p.load_raw(&refs)? {
        None => {
          return Err(ConfigError::Missing {
            path: path_str.clone(),
          });
        }
        Some(s) => s,
      };
      s.parse::<f64>().map_err(|e| ConfigError::Invalid {
        path: path_str,
        value: s,
        reason: e.to_string(),
      })
    })
  }
}

impl Config<i64> {
  /// Load a signed integer (Effect `Config.integer`).
  pub fn integer(path: impl Into<String>) -> Self {
    let segs = split_dotted(&path.into());
    Self::new(move |p| {
      let refs: Vec<&str> = segs.iter().map(String::as_str).collect();
      let path_str = refs.join(".");
      let s = match p.load_raw(&refs)? {
        None => {
          return Err(ConfigError::Missing {
            path: path_str.clone(),
          });
        }
        Some(s) => s,
      };
      s.parse::<i64>().map_err(|e| ConfigError::Invalid {
        path: path_str,
        value: s,
        reason: e.to_string(),
      })
    })
  }
}

impl Config<bool> {
  /// Load a boolean (Effect `Config.boolean`).
  pub fn boolean(path: impl Into<String>) -> Self {
    let segs = split_dotted(&path.into());
    Self::new(move |p| {
      let refs: Vec<&str> = segs.iter().map(String::as_str).collect();
      let path_str = refs.join(".");
      let s = match p.load_raw(&refs)? {
        None => {
          return Err(ConfigError::Missing {
            path: path_str.clone(),
          });
        }
        Some(s) => s,
      };
      match s.to_ascii_lowercase().as_str() {
        "true" | "1" | "yes" => Ok(true),
        "false" | "0" | "no" => Ok(false),
        _ => Err(ConfigError::Invalid {
          path: path_str,
          value: s,
          reason: "expected boolean string".into(),
        }),
      }
    })
  }
}

impl Config<Vec<String>> {
  /// Load a delimiter-separated string list (Effect `Config.array(Config.string(…))`).
  pub fn string_list(path: impl Into<String>) -> Self {
    let segs = split_dotted(&path.into());
    Self::new(move |p| {
      let refs: Vec<&str> = segs.iter().map(String::as_str).collect();
      let path_str = refs.join(".");
      let s = match p.load_raw(&refs)? {
        None => return Err(ConfigError::Missing { path: path_str }),
        Some(s) => s,
      };
      let delim = p.seq_delim();
      Ok(
        s.split(delim)
          .map(str::trim)
          .filter(|x| !x.is_empty())
          .map(str::to_string)
          .collect(),
      )
    })
  }

  /// CSV list (comma-separated, trimmed); ignores [`ConfigProvider::seq_delim`] (Effect `Config.repeat`).
  pub fn repeat(path: impl Into<String>) -> Self {
    let segs = split_dotted(&path.into());
    Self::new(move |p| {
      let refs: Vec<&str> = segs.iter().map(String::as_str).collect();
      let path_str = refs.join(".");
      let s = match p.load_raw(&refs)? {
        None => return Err(ConfigError::Missing { path: path_str }),
        Some(s) => s,
      };
      Ok(split_csv_list(&s))
    })
  }
}

fn split_csv_list(s: &str) -> Vec<String> {
  s.split(',')
    .map(str::trim)
    .filter(|x| !x.is_empty())
    .map(str::to_string)
    .collect()
}

impl Config<Duration> {
  /// Load a [`Duration`] using [`id_effect::scheduling::duration::duration::decode`] (Effect.ts `Duration` decode).
  ///
  /// ```rust
  /// use std::time::Duration;
  /// use id_effect_config::{Config, MapConfigProvider};
  ///
  /// let p = MapConfigProvider::from_pairs([("TIMEOUT", "5s")]);
  /// let t = Config::duration("TIMEOUT").load(&p).unwrap();
  /// assert_eq!(t, Duration::from_secs(5));
  /// ```
  pub fn duration(path: impl Into<String>) -> Self {
    let segs = split_dotted(&path.into());
    Self::new(move |p| {
      let refs: Vec<&str> = segs.iter().map(String::as_str).collect();
      let path_str = refs.join(".");
      let s = match p.load_raw(&refs)? {
        None => {
          return Err(ConfigError::Missing {
            path: path_str.clone(),
          });
        }
        Some(s) => s,
      };
      duration::decode(s.trim()).map_err(|e| ConfigError::Invalid {
        path: path_str,
        value: e.input.clone(),
        reason: e.to_string(),
      })
    })
  }
}

impl Config<LogLevel> {
  /// Load a [`LogLevel`] (case-insensitive; see [`LogLevel`](id_effect::LogLevel)).
  pub fn log_level(path: impl Into<String>) -> Self {
    let segs = split_dotted(&path.into());
    Self::new(move |p| {
      let refs: Vec<&str> = segs.iter().map(String::as_str).collect();
      let path_str = refs.join(".");
      let s = match p.load_raw(&refs)? {
        None => {
          return Err(ConfigError::Missing {
            path: path_str.clone(),
          });
        }
        Some(s) => s,
      };
      LogLevel::from_str(s.trim()).map_err(|reason| ConfigError::Invalid {
        path: path_str,
        value: s,
        reason,
      })
    })
  }
}

impl Config<Url> {
  /// Load a [`Url`] string.
  pub fn url(path: impl Into<String>) -> Self {
    let segs = split_dotted(&path.into());
    Self::new(move |p| {
      let refs: Vec<&str> = segs.iter().map(String::as_str).collect();
      let path_str = refs.join(".");
      let s = match p.load_raw(&refs)? {
        None => {
          return Err(ConfigError::Missing {
            path: path_str.clone(),
          });
        }
        Some(s) => s,
      };
      s.parse::<Url>().map_err(|e| ConfigError::Invalid {
        path: path_str,
        value: s,
        reason: e.to_string(),
      })
    })
  }
}

// ── Combining descriptors (Effect `Config.all` / `zip` / `zipWith` / `orElse`) ──

/// Record `r`'s failure in `errors` and keep its value, if any (error accumulation for `all`).
fn keep<T>(r: Result<T, ConfigError>, errors: &mut Vec<ConfigError>) -> Option<T> {
  match r {
    Ok(v) => Some(v),
    Err(e) => {
      errors.push(e);
      None
    }
  }
}

/// A collection of descriptors that [`Config::all`] combines into one: tuples of [`Config`]s
/// (arity 1–12), `Vec<Config<T>>` and `[Config<T>; N]` (Effect.ts `Config.all` over a tuple or
/// an array).
///
/// Every component is loaded; when any fail, the result is [`ConfigError::and`] of all their
/// errors (one [`ConfigError::And`] for two or more).
pub trait ConfigAll {
  /// The combined value: the tuple of the component values, a `Vec<T>` or a `[T; N]`.
  type Output: Send + Sync + 'static;

  /// Combine into one descriptor.
  fn into_config(self) -> Config<Self::Output>;
}

macro_rules! impl_config_all_tuple {
  ($($T:ident $c:ident),+) => {
    impl<$($T: Send + Sync + 'static),+> ConfigAll for ($(Config<$T>,)+) {
      type Output = ($($T,)+);

      fn into_config(self) -> Config<Self::Output> {
        let ($($c,)+) = self;
        Config::new(move |p| {
          let mut errors = Vec::new();
          $(let $c = keep(($c.loader)(p), &mut errors);)+
          match ($($c,)+) {
            ($(Some($c),)+) => Ok(($($c,)+)),
            _ => Err(ConfigError::and(errors)),
          }
        })
      }
    }
  };
}

impl_config_all_tuple!(A a);
impl_config_all_tuple!(A a, B b);
impl_config_all_tuple!(A a, B b, C c);
impl_config_all_tuple!(A a, B b, C c, D d);
impl_config_all_tuple!(A a, B b, C c, D d, F f);
impl_config_all_tuple!(A a, B b, C c, D d, F f, G g);
impl_config_all_tuple!(A a, B b, C c, D d, F f, G g, H h);
impl_config_all_tuple!(A a, B b, C c, D d, F f, G g, H h, J j);
impl_config_all_tuple!(A a, B b, C c, D d, F f, G g, H h, J j, K k);
impl_config_all_tuple!(A a, B b, C c, D d, F f, G g, H h, J j, K k, L l);
impl_config_all_tuple!(A a, B b, C c, D d, F f, G g, H h, J j, K k, L l, M m);
impl_config_all_tuple!(A a, B b, C c, D d, F f, G g, H h, J j, K k, L l, M m, N n);

/// Load every descriptor of `configs`, accumulating the failures.
fn load_all<T: Send + Sync + 'static>(
  configs: &[Config<T>],
  p: &dyn ConfigProvider,
) -> Result<Vec<T>, ConfigError> {
  let mut errors = Vec::new();
  let values: Vec<T> = configs
    .iter()
    .filter_map(|c| keep((c.loader)(p), &mut errors))
    .collect();
  if errors.is_empty() {
    Ok(values)
  } else {
    Err(ConfigError::and(errors))
  }
}

impl<T: Send + Sync + 'static> ConfigAll for Vec<Config<T>> {
  type Output = Vec<T>;

  fn into_config(self) -> Config<Vec<T>> {
    Config::new(move |p| load_all(&self, p))
  }
}

impl<T: Send + Sync + 'static, const N: usize> ConfigAll for [Config<T>; N] {
  type Output = [T; N];

  fn into_config(self) -> Config<[T; N]> {
    Config::new(move |p| {
      load_all(&self, p).map(|values| match <[T; N]>::try_from(values) {
        Ok(array) => array,
        Err(_) => unreachable!("one value per descriptor"),
      })
    })
  }
}

impl<T: Send + Sync + 'static> Config<T> {
  /// Combine descriptors into one (Effect.ts `Config.all`): a tuple of configs yields a tuple,
  /// a `Vec` or array of configs a `Vec` or array.
  ///
  /// Every component is loaded and **every** failure is reported: with two or more, the error
  /// is one [`ConfigError::And`] listing them all (in component order).
  ///
  /// ```rust
  /// use id_effect_config::{Config, ConfigError, MapConfigProvider};
  ///
  /// let p = MapConfigProvider::from_pairs([("HOST", "0.0.0.0"), ("PORT", "9000")]);
  /// let (host, port) = Config::all((Config::string("HOST"), Config::integer("PORT")))
  ///   .load(&p)
  ///   .unwrap();
  /// assert_eq!((host.as_str(), port), ("0.0.0.0", 9000));
  ///
  /// // Both keys are reported, not only the first.
  /// let empty = MapConfigProvider::from_pairs::<[(&str, &str); 0], _, _>([]);
  /// let err = Config::all((Config::string("HOST"), Config::integer("PORT")))
  ///   .load(&empty)
  ///   .unwrap_err();
  /// assert_eq!(err.errors().len(), 2);
  /// assert!(matches!(err, ConfigError::And(_)));
  /// ```
  pub fn all<C: ConfigAll<Output = T>>(configs: C) -> Self {
    configs.into_config()
  }

  /// Load `self` and `that` into a pair, accumulating both failures (Effect `Config.zip`).
  pub fn zip<U: Send + Sync + 'static>(self, that: Config<U>) -> Config<(T, U)> {
    Config::all((self, that))
  }

  /// Load `self` and `that` and merge the values with `f`, accumulating both failures (Effect
  /// `Config.zipWith`).
  ///
  /// ```rust
  /// use id_effect_config::{Config, MapConfigProvider};
  ///
  /// let p = MapConfigProvider::from_pairs([("HOST", "localhost"), ("PORT", "8080")]);
  /// let addr = Config::string("HOST")
  ///   .zip_with(Config::integer("PORT"), |h, port| format!("{h}:{port}"))
  ///   .load(&p)
  ///   .unwrap();
  /// assert_eq!(addr, "localhost:8080");
  /// ```
  pub fn zip_with<U, V>(
    self,
    that: Config<U>,
    f: impl Fn(T, U) -> V + Send + Sync + 'static,
  ) -> Config<V>
  where
    U: Send + Sync + 'static,
    V: Send + Sync + 'static,
  {
    self.zip(that).map(move |(t, u)| f(t, u))
  }

  /// Try `self`; when it fails with missing data only ([`ConfigError::is_missing_only`]), load
  /// `that` instead (Effect `Config.orElse`). Other failures propagate.
  ///
  /// ```rust
  /// use id_effect_config::{Config, MapConfigProvider};
  ///
  /// let p = MapConfigProvider::from_pairs([("FALLBACK_PORT", "4000")]);
  /// let port = Config::integer("PORT")
  ///   .or_else(Config::integer("FALLBACK_PORT"))
  ///   .load(&p)
  ///   .unwrap();
  /// assert_eq!(port, 4000);
  /// ```
  pub fn or_else(self, that: Config<T>) -> Self {
    let primary = self.loader;
    Config {
      loader: Arc::new(move |p| match (primary)(p) {
        Ok(v) => Ok(v),
        Err(e) if e.is_missing_only() => (that.loader)(p),
        Err(e) => Err(e),
      }),
    }
  }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
  use std::time::Duration;

  use crate::MapConfigProvider;
  use crate::test_support::run_blocking;
  use id_effect::LogLevel;

  use super::*;

  fn map_provider(pairs: &[(&str, &str)]) -> MapConfigProvider {
    MapConfigProvider::from_pairs(pairs.iter().copied())
  }

  fn go<T: Send + Sync + 'static>(c: Config<T>, p: &dyn ConfigProvider) -> Result<T, ConfigError> {
    c.load(p)
  }

  #[test]
  fn string_required_found() {
    let p = map_provider(&[("HOST", "localhost")]);
    assert_eq!(go(Config::string("HOST"), &p).unwrap(), "localhost");
  }

  #[test]
  fn string_required_missing_is_error() {
    let p = map_provider(&[]);
    assert!(go(Config::string("HOST"), &p).is_err());
  }

  #[test]
  fn with_default_on_missing() {
    let p = map_provider(&[]);
    let port = go(Config::integer("PORT").with_default(3000), &p).unwrap();
    assert_eq!(port, 3000);
  }

  #[test]
  fn with_default_not_used_when_present() {
    let p = map_provider(&[("PORT", "8080")]);
    let port = go(Config::integer("PORT").with_default(3000), &p).unwrap();
    assert_eq!(port, 8080);
  }

  #[test]
  fn map_transforms_value() {
    let p = map_provider(&[("PORT", "8080")]);
    let port_u16: u16 = go(Config::integer("PORT").map(|n| n as u16), &p).unwrap();
    assert_eq!(port_u16, 8080);
  }

  #[test]
  fn map_attempt_success() {
    let p = map_provider(&[("VAL", "42")]);
    let v = go(
      Config::string("VAL").map_attempt(|s| {
        s.parse::<i32>().map_err(|e| ConfigError::Invalid {
          path: "VAL".into(),
          value: s,
          reason: e.to_string(),
        })
      }),
      &p,
    )
    .unwrap();
    assert_eq!(v, 42i32);
  }

  #[test]
  fn validate_passes() {
    let p = map_provider(&[("PORT", "8080")]);
    let port = go(
      Config::integer("PORT").validate("must be > 0", |n| *n > 0),
      &p,
    )
    .unwrap();
    assert_eq!(port, 8080);
  }

  #[test]
  fn validate_fails() {
    let p = map_provider(&[("PORT", "99999")]);
    let err = go(
      Config::integer("PORT").validate("port must be 1–65535", |n| (1..=65535).contains(n)),
      &p,
    )
    .unwrap_err();
    assert!(err.to_string().contains("port must be 1–65535"));
  }

  #[test]
  fn secret_wraps_and_redacts() {
    let p = map_provider(&[("KEY", "s3cr3t")]);
    let k = go(Config::string("KEY").secret(), &p).unwrap();
    assert_eq!(format!("{k:?}"), "<redacted>");
    assert_eq!(k.expose(), "s3cr3t");
  }

  #[test]
  fn secret_load_errors_never_contain_the_value() {
    let p = map_provider(&[("PIN", "s3cr3t"), ("A", "x9secret"), ("B", "y9secret")]);
    let err = go(Config::integer("PIN").secret(), &p).unwrap_err();
    let shown = format!("{err} {err:?}");
    assert!(!shown.contains("s3cr3t"), "{shown}");
    assert!(shown.contains("<redacted>"), "{shown}");
    assert!(shown.contains("PIN"), "{shown}");

    let err = go(Config::integer("A").zip(Config::integer("B")).secret(), &p).unwrap_err();
    let shown = format!("{err} {err:?}");
    assert!(
      matches!(err, ConfigError::And(ref v) if v.len() == 2),
      "{shown}"
    );
    assert!(
      !shown.contains("x9secret") && !shown.contains("y9secret"),
      "{shown}"
    );
  }

  #[test]
  fn nested_prepends_prefix() {
    let p = map_provider(&[("SERVER_HOST", "0.0.0.0")]);
    let host = go(Config::string("HOST").nested("SERVER"), &p).unwrap();
    assert_eq!(host, "0.0.0.0");
  }

  #[test]
  fn all_combines_two() {
    let p = map_provider(&[("HOST", "::1"), ("PORT", "9000")]);
    let (host, port) = go(
      Config::all((Config::string("HOST"), Config::integer("PORT"))),
      &p,
    )
    .unwrap();
    assert_eq!(host, "::1");
    assert_eq!(port, 9000);
  }

  #[test]
  fn all3_combines_three() {
    let p = map_provider(&[("A", "1"), ("B", "2"), ("C", "3")]);
    let (a, b, c) = go(
      Config::all((
        Config::integer("A"),
        Config::integer("B"),
        Config::integer("C"),
      )),
      &p,
    )
    .unwrap();
    assert_eq!((a, b, c), (1, 2, 3));
  }

  #[test]
  fn zip_with_merges() {
    let p = map_provider(&[("HOST", "localhost"), ("PORT", "8080")]);
    let addr = go(
      Config::string("HOST").zip_with(Config::integer("PORT"), |h, po| format!("{h}:{po}")),
      &p,
    )
    .unwrap();
    assert_eq!(addr, "localhost:8080");
  }

  #[test]
  fn or_else_falls_back_on_missing() {
    let p = map_provider(&[("FALLBACK_PORT", "4000")]);
    let port = go(
      Config::integer("PORT").or_else(Config::integer("FALLBACK_PORT")),
      &p,
    )
    .unwrap();
    assert_eq!(port, 4000);
  }

  #[test]
  fn or_else_uses_primary_when_present() {
    let p = map_provider(&[("PORT", "8080"), ("FALLBACK_PORT", "4000")]);
    let port = go(
      Config::integer("PORT").or_else(Config::integer("FALLBACK_PORT")),
      &p,
    )
    .unwrap();
    assert_eq!(port, 8080);
  }

  #[test]
  fn all_vec_collects() {
    let p = map_provider(&[("K0", "10"), ("K1", "20"), ("K2", "30")]);
    let vals = go(
      Config::all(vec![
        Config::integer("K0"),
        Config::integer("K1"),
        Config::integer("K2"),
      ]),
      &p,
    )
    .unwrap();
    assert_eq!(vals, vec![10, 20, 30]);
  }

  #[test]
  fn boolean_true_and_false() {
    let p = map_provider(&[("A", "true"), ("B", "false"), ("C", "1"), ("D", "0")]);
    assert!(go(Config::boolean("A"), &p).unwrap());
    assert!(!go(Config::boolean("B"), &p).unwrap());
    assert!(go(Config::boolean("C"), &p).unwrap());
    assert!(!go(Config::boolean("D"), &p).unwrap());
  }

  #[test]
  fn string_list_splits() {
    let p = map_provider(&[("TAGS", "a,b,c")]);
    let tags = go(Config::string_list("TAGS"), &p).unwrap();
    assert_eq!(tags, vec!["a", "b", "c"]);
  }

  mod duration {
    use super::*;

    fn dur(pairs: &[(&str, &str)], key: &'static str) -> Result<Duration, ConfigError> {
      let p = map_provider(pairs);
      go(Config::duration(key), &p)
    }

    #[test]
    fn bare_integer_is_millis() {
      assert_eq!(
        dur(&[("T", "500")], "T").unwrap(),
        Duration::from_millis(500)
      );
    }

    #[test]
    fn ms_suffix() {
      assert_eq!(
        dur(&[("T", "250ms")], "T").unwrap(),
        Duration::from_millis(250)
      );
    }

    #[test]
    fn seconds_suffix() {
      assert_eq!(dur(&[("T", "5s")], "T").unwrap(), Duration::from_secs(5));
    }

    #[test]
    fn minutes_suffix() {
      assert_eq!(
        dur(&[("T", "2min")], "T").unwrap(),
        Duration::from_secs(120)
      );
    }

    #[test]
    fn hours_suffix() {
      assert_eq!(dur(&[("T", "1h")], "T").unwrap(), Duration::from_secs(3600));
    }

    #[test]
    fn days_suffix() {
      assert_eq!(
        dur(&[("T", "1d")], "T").unwrap(),
        Duration::from_secs(86400)
      );
    }

    #[test]
    fn unknown_unit_is_error() {
      assert!(dur(&[("T", "5x")], "T").is_err());
    }
  }

  #[test]
  fn config_duration_parses_30_seconds() {
    let p = map_provider(&[("D", "30 seconds")]);
    assert_eq!(
      go(Config::duration("D"), &p).unwrap(),
      Duration::from_secs(30)
    );
  }

  #[test]
  fn config_log_level_parses_info() {
    let p = map_provider(&[("LVL", "info")]);
    assert_eq!(go(Config::log_level("LVL"), &p).unwrap(), LogLevel::Info);
  }

  #[test]
  fn config_url_parses_valid_url() {
    let p = map_provider(&[("U", "https://example.com/path?x=1")]);
    let u = go(Config::url("U"), &p).unwrap();
    assert_eq!(u.as_str(), "https://example.com/path?x=1");
  }

  #[test]
  fn config_nested_prefixes_keys() {
    let p = map_provider(&[("APP_SERVER_HOST", "h")]);
    let host = go(Config::string("HOST").nested("APP_SERVER"), &p).unwrap();
    assert_eq!(host, "h");
  }

  #[test]
  fn config_repeat_splits_csv() {
    let p = map_provider(&[("ITEMS", "a, b ,c")]);
    let v = go(Config::repeat("ITEMS"), &p).unwrap();
    assert_eq!(v, vec!["a", "b", "c"]);
  }

  // ── Debug / Clone ──────────────────────────────────────────────────────────

  #[test]
  fn config_debug_contains_type_name() {
    let c = Config::<String>::string("X");
    let s = format!("{c:?}");
    assert!(s.contains("Config"));
  }

  #[test]
  fn config_clone_independent_from_original() {
    let p = map_provider(&[("X", "hello")]);
    let c = Config::string("X");
    let c2 = c.clone();
    assert_eq!(go(c2, &p).unwrap(), "hello");
  }

  // ── Config::run ────────────────────────────────────────────────────────────

  #[test]
  fn config_run_via_effect() {
    use crate::{MapConfigProvider, config_env};

    let p = MapConfigProvider::from_pairs([("HOST", "localhost")]);
    let env = config_env(p);
    let host: String = run_blocking(
      Config::string("HOST").run::<String, ConfigError, _, _>(),
      env,
    )
    .unwrap();
    assert_eq!(host, "localhost");
  }

  #[test]
  fn config_run_error_propagated() {
    use crate::{MapConfigProvider, config_env};

    let p = MapConfigProvider::from_pairs::<[(&str, &str); 0], _, _>([]);
    let env = config_env(p);
    let result: Result<String, ConfigError> = run_blocking(
      Config::string("MISSING").run::<String, ConfigError, _, _>(),
      env,
    );
    assert!(result.is_err());
  }

  // ── Config::run with a scoped provider override ───────────────────────────

  #[test]
  fn config_run_with_scoped_env_override() {
    use crate::{ConfigEnv, MapConfigProvider, config_env, provide_config_provider};
    use id_effect::Effect;

    // The outer program reads from the base provider; the inner read is given its own layer.
    let inner: Effect<String, ConfigError> = Config::string("REGION")
      .run::<String, ConfigError, ConfigEnv, _>()
      .provide_layer(provide_config_provider(MapConfigProvider::from_pairs([(
        "REGION", "eu-west",
      )])));
    let program: Effect<(String, String), ConfigError, ConfigEnv> = Config::string("REGION")
      .run::<String, ConfigError, ConfigEnv, _>()
      .flat_map(move |outer| {
        inner
          .narrow::<ConfigEnv, _>()
          .map(move |inner| (outer, inner))
      });
    let base = config_env(MapConfigProvider::from_pairs([("REGION", "us-east")]));
    let (outer, inner) = run_blocking(program, base).unwrap();
    assert_eq!((outer.as_str(), inner.as_str()), ("us-east", "eu-west"));
  }

  // ── with_default non-missing error propagation ─────────────────────────────

  #[test]
  fn with_default_propagates_invalid_error() {
    let p = map_provider(&[("PORT", "not_a_number")]);
    let err = go(Config::integer("PORT").with_default(3000), &p).unwrap_err();
    assert!(matches!(err, ConfigError::Invalid { .. }));
  }

  // ── optional_string ────────────────────────────────────────────────────────

  #[test]
  fn optional_string_present() {
    let p = map_provider(&[("OPT", "value")]);
    assert_eq!(
      go(Config::optional_string("OPT"), &p).unwrap(),
      Some("value".to_string())
    );
  }

  #[test]
  fn optional_string_absent() {
    let p = map_provider(&[]);
    assert_eq!(go(Config::optional_string("OPT"), &p).unwrap(), None);
  }

  #[test]
  fn optional_string_dotted_path() {
    let p = map_provider(&[("SERVER_HOST", "localhost")]);
    assert_eq!(
      go(Config::optional_string("SERVER.HOST"), &p).unwrap(),
      Some("localhost".to_string())
    );
  }

  // ── boolean yes/no variants ────────────────────────────────────────────────

  #[test]
  fn boolean_yes_no_variants() {
    let p = map_provider(&[("A", "yes"), ("B", "no"), ("C", "YES"), ("D", "NO")]);
    assert!(go(Config::boolean("A"), &p).unwrap());
    assert!(!go(Config::boolean("B"), &p).unwrap());
    assert!(go(Config::boolean("C"), &p).unwrap());
    assert!(!go(Config::boolean("D"), &p).unwrap());
  }

  #[test]
  fn boolean_invalid_string_is_error() {
    let p = map_provider(&[("FLAG", "maybe")]);
    let err = go(Config::boolean("FLAG"), &p).unwrap_err();
    assert!(matches!(err, ConfigError::Invalid { .. }));
  }

  #[test]
  fn boolean_missing_is_error() {
    let p = map_provider(&[]);
    let err = go(Config::boolean("FLAG"), &p).unwrap_err();
    assert!(matches!(err, ConfigError::Missing { .. }));
  }

  // ── map_attempt failure ───────────────────────────────────────────────────

  #[test]
  fn map_attempt_failure_propagates_error() {
    let p = map_provider(&[("VAL", "not-a-number")]);
    let err = go(
      Config::string("VAL").map_attempt(|s| {
        s.parse::<i32>().map_err(|e| ConfigError::Invalid {
          path: "VAL".into(),
          value: s,
          reason: e.to_string(),
        })
      }),
      &p,
    )
    .unwrap_err();
    assert!(matches!(err, ConfigError::Invalid { .. }));
  }

  // ── number missing/invalid ────────────────────────────────────────────────

  #[test]
  fn number_missing_is_error() {
    let p = map_provider(&[]);
    let err = go(Config::number("N"), &p).unwrap_err();
    assert!(matches!(err, ConfigError::Missing { .. }));
  }

  #[test]
  fn number_invalid_is_error() {
    let p = map_provider(&[("N", "not_a_float")]);
    let err = go(Config::number("N"), &p).unwrap_err();
    assert!(matches!(err, ConfigError::Invalid { .. }));
  }

  // ── integer missing/invalid ───────────────────────────────────────────────

  #[test]
  fn integer_missing_is_error() {
    let p = map_provider(&[]);
    let err = go(Config::integer("N"), &p).unwrap_err();
    assert!(matches!(err, ConfigError::Missing { .. }));
  }

  #[test]
  fn integer_invalid_is_error() {
    let p = map_provider(&[("N", "not_an_int")]);
    let err = go(Config::integer("N"), &p).unwrap_err();
    assert!(matches!(err, ConfigError::Invalid { .. }));
  }

  // ── string_list missing ───────────────────────────────────────────────────

  #[test]
  fn string_list_missing_is_error() {
    let p = map_provider(&[]);
    let err = go(Config::string_list("TAGS"), &p).unwrap_err();
    assert!(matches!(err, ConfigError::Missing { .. }));
  }

  // ── repeat missing ────────────────────────────────────────────────────────

  #[test]
  fn repeat_missing_is_error() {
    let p = map_provider(&[]);
    let err = go(Config::repeat("ITEMS"), &p).unwrap_err();
    assert!(matches!(err, ConfigError::Missing { .. }));
  }

  // ── or_else propagates non-missing error ──────────────────────────────────

  #[test]
  fn or_else_propagates_invalid_error() {
    let p = map_provider(&[("PORT", "bad")]);
    let err = go(
      Config::integer("PORT").or_else(Config::integer("FALLBACK_PORT")),
      &p,
    )
    .unwrap_err();
    assert!(matches!(err, ConfigError::Invalid { .. }));
  }

  // ── prefix provider seq_delim ─────────────────────────────────────────────

  #[test]
  fn nested_prefix_provider_seq_delim_delegates() {
    use crate::MapConfigProvider;
    use crate::provider::ProviderOptions;

    let opts = ProviderOptions {
      path_delim: "_",
      seq_delim: "|",
    };
    let map: std::collections::HashMap<String, String> =
      [("NS_ITEMS".to_string(), "a|b|c".to_string())]
        .into_iter()
        .collect();
    let p = MapConfigProvider::with_options(map, opts);
    let items = go(Config::string_list("ITEMS").nested("NS"), &p).unwrap();
    assert_eq!(items, vec!["a", "b", "c"]);
  }
}

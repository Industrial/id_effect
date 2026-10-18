//! The login rate limits, per client and per organisation, hot-reloaded from the profile file.
//!
//! Two token buckets guard `POST /auth/login` and `POST /orgs`:
//!
//! - **per client** (`login_per_second`, `login_burst`): keyed by the address the proxy reports
//!   (see `app::client_address`);
//! - **per organisation** (`login_tenant_per_second`, `login_tenant_burst`): keyed by
//!   [`RateLimitKey::tenant`] on the [`TenantHint`] a login names in `X-Ledger-Org`, so one
//!   organisation's sign-ins share a budget across every client and instance. A login whose
//!   user is in another organisation than the one it names is refused like a wrong password, so
//!   naming a different organisation cannot dodge the bucket of the real one.
//!
//! The values are read through a [`ProfileWatch`] of `config/base.toml` and
//! `config/<profile>.toml` ([`watch`]): edit the file and, within `reload_limits_secs`, the next
//! login draws from buckets of the new size ([`ReloadingStore`]; the buckets restart full when
//! the values change). A file that no longer parses keeps the previous values.

use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use id_effect::{Effect, HNil, succeed};
use id_effect_kit::axum::middleware::{
  MemoryRateLimitStore, RateLimitError, RateLimitKey, RateLimitLayer, RateLimitStore,
};
use id_effect_kit::config::{ConfigError, Profile, ProfileWatch, Profiles};
use id_effect_kit::redis::{RedisClient, RedisRateLimitStore};
use serde::Deserialize;

use crate::config::LoginLimits;

/// The header naming the organisation a login is for.
pub const TENANT_HEADER: &str = "x-ledger-org";

/// The organisation a request names in [`TENANT_HEADER`] (recorded in the request capabilities
/// by `app::router`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TenantHint(pub String);

impl TenantHint {
  /// The hint of a header value: an organisation id (`^[a-z0-9][a-z0-9-]{1,31}$`), else none.
  pub fn parse(value: &str) -> Option<Self> {
    let valid = (2..=32).contains(&value.len())
      && value
        .bytes()
        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
      && !value.starts_with('-');
    valid.then(|| Self(value.to_owned()))
  }
}

/// The part of a profile the watch reads: `[auth]`'s login limits.
#[derive(Clone, Debug, Deserialize)]
pub struct LimitsFile {
  /// `[auth]`.
  pub auth: LoginLimits,
}

/// Watches `profile`'s files for new login limits, polling every `every`.
pub fn watch(
  profiles: &Profiles,
  profile: Profile,
  every: Duration,
) -> Result<ProfileWatch<LimitsFile>, ConfigError> {
  profiles.watch_for::<LimitsFile>(profile, every)
}

/// Where the limits in force come from.
#[derive(Clone)]
pub enum LimitSource {
  /// Fixed at start.
  Fixed(LoginLimits),
  /// The profile file, reloaded.
  Watched(ProfileWatch<LimitsFile>),
}

impl LimitSource {
  fn current(&self) -> Effect<LoginLimits, RateLimitError> {
    match self {
      Self::Fixed(limits) => succeed(*limits),
      Self::Watched(watch) => watch.get::<RateLimitError, HNil>().map(|file| file.auth),
    }
  }
}

/// Builds the store of a `(rate, burst)` pair: in process, or in Redis.
type Make = dyn Fn(f64, f64) -> Arc<dyn RateLimitStore> + Send + Sync;

/// A [`RateLimitStore`] whose rate and burst follow a [`LimitSource`]: each admission reads the
/// values in force and draws from the store built for them, rebuilt when they change. Clones
/// share the store.
#[derive(Clone)]
pub struct ReloadingStore(Arc<Reloading>);

struct Reloading {
  source: LimitSource,
  pick: fn(&LoginLimits) -> (f64, f64),
  make: Box<Make>,
  built: Mutex<Option<Built>>,
}

/// The store last built, with the `(rate, burst)` bits it was built for.
type Built = ((u64, u64), Arc<dyn RateLimitStore>);

impl ReloadingStore {
  /// The values `pick` takes from `source`, in stores `make` builds.
  pub fn new(
    source: LimitSource,
    pick: fn(&LoginLimits) -> (f64, f64),
    make: impl Fn(f64, f64) -> Arc<dyn RateLimitStore> + Send + Sync + 'static,
  ) -> Self {
    Self(Arc::new(Reloading {
      source,
      pick,
      make: Box::new(make),
      built: Mutex::new(None),
    }))
  }
}

impl Reloading {
  /// The store for `(rate, burst)`, built on first use and after a change.
  fn store_for(&self, rate: f64, burst: f64) -> Arc<dyn RateLimitStore> {
    let mut built = self.built.lock().unwrap_or_else(PoisonError::into_inner);
    let key = (rate.to_bits(), burst.to_bits());
    match &*built {
      Some((at, store)) if *at == key => Arc::clone(store),
      _ => {
        let store = (self.make)(rate, burst);
        *built = Some((key, Arc::clone(&store)));
        store
      }
    }
  }
}

impl RateLimitStore for ReloadingStore {
  fn acquire(&self, key: &str) -> Effect<(), RateLimitError, HNil> {
    let (this, key) = (Arc::clone(&self.0), key.to_owned());
    self.0.source.current().flat_map(move |limits| {
      let (rate, burst) = (this.pick)(&limits);
      this.store_for(rate, burst).acquire(&key)
    })
  }
}

/// The two login limits of the router.
#[derive(Clone)]
pub struct LoginLimit {
  /// Per client.
  pub client: RateLimitLayer,
  /// Per organisation.
  pub tenant: RateLimitLayer,
}

/// The login limits over `source`, in Redis when there is one (shared by every instance), else
/// in process; `client` keys the per-client bucket.
pub fn login_limit(
  source: LimitSource,
  redis: Option<RedisClient>,
  client: RateLimitKey,
) -> LoginLimit {
  let layer = |pick: fn(&LoginLimits) -> (f64, f64), key: RateLimitKey| {
    let redis = redis.clone();
    let store = ReloadingStore::new(source.clone(), pick, move |rate, burst| match &redis {
      Some(redis) => {
        Arc::new(RedisRateLimitStore::new(redis.clone(), rate, burst)) as Arc<dyn RateLimitStore>
      }
      None => Arc::new(MemoryRateLimitStore::new(rate, burst)),
    });
    RateLimitLayer::new(store).key(key)
  };
  LoginLimit {
    client: layer(|l| (l.login_per_second, l.login_burst), client),
    tenant: layer(
      |l| (l.login_tenant_per_second, l.login_tenant_burst),
      RateLimitKey::tenant::<TenantHint>(|hint| Some(format!("org:{}", hint.0))),
    ),
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn tenant_hints_are_organisation_ids() {
    assert_eq!(TenantHint::parse("acme"), Some(TenantHint("acme".into())));
    assert_eq!(
      TenantHint::parse("acme-2"),
      Some(TenantHint("acme-2".into()))
    );
    for bad in ["", "a", "-acme", "Acme", "acme corp", &"a".repeat(33)] {
      assert_eq!(TenantHint::parse(bad), None, "{bad}");
    }
  }
}

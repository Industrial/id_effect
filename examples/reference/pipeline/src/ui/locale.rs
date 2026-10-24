//! The per-user language override: each signed-in user's chosen locale, which beats
//! `Accept-Language` on every back-office page (the admin's included).
//!
//! Choices are keyed by `<tenant>/<principal id>`, so a user id reused by two tenants keeps two
//! settings, and an anonymous request has none. Only locales the catalog supports are stored
//! (the handler checks), and at most [`MAX_PREFERENCES`] choices are kept (the oldest goes
//! first), so the store cannot grow without bound. They live in the process: a deployment with
//! several instances keeps the setting in its user store and returns it from
//! [`LocaleLayer::preference`](id_effect_kit::i18n::axum::LocaleLayer::preference) the same way.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex, PoisonError};

use axum::http::request::Parts;
use id_effect_kit::authz::CurrentPrincipal;
use id_effect_kit::i18n::Locale;

/// The most user choices kept.
pub const MAX_PREFERENCES: usize = 10_000;

#[derive(Default)]
struct Store {
  by_user: HashMap<String, Locale>,
  order: VecDeque<String>,
}

/// Every signed-in user's chosen locale; clones share the choices.
#[derive(Clone, Default)]
pub struct LocalePreferences {
  store: Arc<Mutex<Store>>,
}

impl std::fmt::Debug for LocalePreferences {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("LocalePreferences")
      .field("users", &self.len())
      .finish()
  }
}

impl LocalePreferences {
  /// No choices yet.
  pub fn new() -> Self {
    Self::default()
  }

  /// The key of `principal`'s choice: `<tenant>/<id>`; `None` for an anonymous principal or one
  /// without a tenant.
  pub fn key(principal: &CurrentPrincipal) -> Option<String> {
    if !principal.is_authenticated() {
      return None;
    }
    let tenant = principal.tenant().filter(|t| !t.is_empty())?;
    Some(format!("{tenant}/{}", principal.id()))
  }

  fn lock(&self) -> std::sync::MutexGuard<'_, Store> {
    self.store.lock().unwrap_or_else(PoisonError::into_inner)
  }

  /// Records that `principal` chose `locale` (nothing for an anonymous principal). Returns
  /// whether it was recorded.
  pub fn set(&self, principal: &CurrentPrincipal, locale: Locale) -> bool {
    let Some(key) = Self::key(principal) else {
      return false;
    };
    let mut store = self.lock();
    if store.by_user.insert(key.clone(), locale).is_none() {
      store.order.push_back(key);
      while store.order.len() > MAX_PREFERENCES {
        if let Some(oldest) = store.order.pop_front() {
          store.by_user.remove(&oldest);
        }
      }
    }
    true
  }

  /// `principal`'s chosen locale, if any.
  pub fn get(&self, principal: &CurrentPrincipal) -> Option<Locale> {
    let key = Self::key(principal)?;
    self.lock().by_user.get(&key).cloned()
  }

  /// How many users chose a locale.
  pub fn len(&self) -> usize {
    self.lock().by_user.len()
  }

  /// Whether no user chose a locale.
  pub fn is_empty(&self) -> bool {
    self.len() == 0
  }

  /// The choice of the request's principal (the `CurrentPrincipal` the authentication layer
  /// put on it), as the locale layer's preference.
  pub fn for_request(&self, parts: &Parts) -> Option<String> {
    let principal = parts.extensions.get::<CurrentPrincipal>()?;
    self.get(principal).map(|l| l.as_str().to_owned())
  }
}

#[cfg(test)]
mod tests {
  use id_effect_kit::authz::BasicPrincipal;

  use super::*;

  fn user(id: &str, tenant: Option<&str>) -> CurrentPrincipal {
    let mut p = BasicPrincipal::new(id);
    if let Some(t) = tenant {
      p = p.with_tenant(t);
    }
    CurrentPrincipal::new(p)
  }

  #[test]
  fn choices_are_per_tenant_user_and_bounded() {
    let prefs = LocalePreferences::new();
    let de = Locale::parse("de").unwrap();
    assert!(prefs.is_empty());
    assert!(prefs.set(&user("rita", Some("acme")), de.clone()));
    assert_eq!(prefs.get(&user("rita", Some("acme"))), Some(de.clone()));
    assert_eq!(prefs.get(&user("rita", Some("globex"))), None);
    assert!(!prefs.set(&user("anon", None), de.clone()));
    assert!(!prefs.set(&CurrentPrincipal::anonymous(), de.clone()));
    assert!(format!("{prefs:?}").contains("users: 1"));
    for n in 0..=MAX_PREFERENCES {
      prefs.set(&user(&format!("u{n}"), Some("acme")), de.clone());
    }
    assert_eq!(prefs.len(), MAX_PREFERENCES);
    assert_eq!(
      prefs.get(&user("rita", Some("acme"))),
      None,
      "the oldest went first"
    );
  }
}

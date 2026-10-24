//! The back office (D1 stage 5): the review queue ([`review`]) next to the generated admin
//! ([`crate::admin`]), in English or German, as one guarded, CSRF-checked part of the app.
//!
//! - **Server-rendered.** maud views (escaped by default), one layout ([`view::page`]) and
//!   components ([`view::Panel`], [`view::StatusBadge`]); htmx swaps the parts that change (a
//!   document's panel, the queue after a publication) and the same handler answers the whole
//!   page to a browser that navigates. htmx is the vendored, sha384-pinned script served by the
//!   app itself ([`HTMX_PATH`](id_effect_kit::ui::htmx::HTMX_PATH)): the pages keep
//!   `script-src 'self'`.
//! - **English or German.** [`catalog`] holds the messages of `locales/<locale>/backoffice.ftl`
//!   and id_effect_i18n's `Schema` validation messages. Each request's locale is the signed-in
//!   user's own choice ([`LocalePreferences`], set on the queue page), else the best match of
//!   `Accept-Language`, else English; only `en` and `de` can be chosen. The admin's labels and
//!   page text come from the same catalog.
//! - **Secure by default.** Every route is guarded (`/admin/<tenant>` as [`crate::admin`] says;
//!   the queue needs a `reviewer` of the tenant); `id_effect_axum`'s `SecurityLayer` checks a
//!   masked CSRF token on every `POST` (the layout puts it on every htmx request) and sends the
//!   CSP and security headers ([`security_layer`]). [`posture()`] states the whole app to the
//!   kit's startup check.

pub mod locale;
pub mod posture;
pub mod review;
pub mod view;

use std::sync::Arc;

use id_effect::DynEnv;
use id_effect_kit::axum::route::Router;
use id_effect_kit::axum::server::security::{COOKIE_KEY_ENV, CookieKey, SecurityLayer};
use id_effect_kit::i18n::axum::LocaleLayer;
use id_effect_kit::i18n::{Catalog, Locale, SchemaMessages};
use id_effect_kit::ui::htmx::asset_route;

use crate::App;

pub use locale::LocalePreferences;
pub use posture::posture;

/// The English messages of the back office.
pub const MESSAGES_EN: &str = include_str!("../../locales/en/backoffice.ftl");

/// The German messages of the back office.
pub const MESSAGES_DE: &str = include_str!("../../locales/de/backoffice.ftl");

/// The locales the back office speaks (the allowlist a request may select), the default first.
pub const LOCALES: [&str; 2] = ["en", "de"];

/// The back office's messages: `locales/{en,de}/backoffice.ftl` (embedded) and the `Schema`
/// validation messages, English the default. Arguments are bidi-isolated (they go into HTML).
///
/// # Panics
///
/// Never: the embedded resources parse and every German message exists (a test checks it).
pub fn catalog() -> Catalog {
  let en = Locale::parse(LOCALES[0]).expect("en is a locale");
  let de = Locale::parse(LOCALES[1]).expect("de is a locale");
  Catalog::builder(en.clone())
    .embedded(en, "locales/en/backoffice.ftl", MESSAGES_EN)
    .embedded(de, "locales/de/backoffice.ftl", MESSAGES_DE)
    .schema_messages()
    .require_complete()
    .build()
    .expect("the embedded back-office catalog is valid and complete")
}

/// The security layer of the back office: CSRF on every state-changing request (a masked token
/// from a signed `__Host-` cookie), the nonce-based CSP and the default security headers. The
/// cookie key is `COOKIE_KEY` (base64, at least 64 bytes) when set, so every instance of the
/// service accepts the others' cookies; otherwise a key generated at startup.
pub fn security_layer() -> SecurityLayer {
  let key = std::env::var(COOKIE_KEY_ENV)
    .ok()
    .and_then(|text| CookieKey::from_base64(text.trim()).ok())
    .unwrap_or_else(CookieKey::generate);
  SecurityLayer::recommended(key)
}

/// The back office's typed routes over `app`: every tenant's admin, the review queue with the
/// language switch, and the htmx script (the one public route).
///
/// # Errors
///
/// When the admin does not build (a defect).
pub fn routes(
  app: &Arc<App>,
  catalog: &Catalog,
  preferences: &LocalePreferences,
) -> Result<Router<DynEnv>, String> {
  let admin = crate::admin::routes(app, catalog).map_err(|e| format!("admin: {e}"))?;
  Ok(
    Router::new()
      .merge(admin)
      .merge(review::routes(app, preferences))
      .merge(asset_route::<DynEnv>()),
  )
}

/// The back office as an Axum router: [`routes`] behind the locale negotiation (the user's
/// choice, then `Accept-Language`) and [`security_layer`]. Merge it into the application's
/// router inside its authentication and tenancy layers ([`crate::router`] does).
///
/// # Errors
///
/// As [`routes`].
pub fn backoffice(app: &Arc<App>) -> Result<axum::Router, String> {
  let catalog = catalog();
  let preferences = LocalePreferences::new();
  let typed = routes(app, &catalog, &preferences)?;
  let locale = LocaleLayer::new(catalog)
    .without_cookie()
    .preference(move |parts| preferences.for_request(parts));
  Ok(
    typed
      .with_state::<()>(DynEnv::new())
      .into_axum()
      .layer(locale)
      .layer(security_layer()),
  )
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn the_catalog_is_complete_in_both_locales() {
    let catalog = catalog();
    assert!(catalog.check().is_complete(), "{}", catalog.check());
    let de = Locale::parse("de").unwrap();
    let args = id_effect_kit::i18n::Args::new();
    assert_eq!(
      catalog.format(&de, "review-title", &args),
      "Prüfwarteschlange"
    );
    assert_eq!(catalog.format(&de, "pipeline.document", &args), "Dokumente");
    assert_eq!(catalog.format(&de, "document.title", &args), "Titel");
    assert!(catalog.has_key(&de, "schema-not-empty"));
  }

  #[test]
  fn the_security_layer_checks_csrf() {
    assert!(security_layer().csrf_enabled());
  }
}

//! Placeholder module graph for upcoming auth and security layers.

pub mod auth {
  //! Session / JWT trait surfaces (wave 1).
}

pub mod security {
  //! CSRF / CSP middleware surfaces (wave 2).
}

/// Ordered list of module names for diagnostics and future auto-wiring.
pub const MODULE_GRAPH: &[&str] = &["bootstrap", "lifecycle", "shutdown", "auth", "security"];

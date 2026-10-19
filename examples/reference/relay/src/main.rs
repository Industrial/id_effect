//! Relay — the id_effect 0.12 reference app: a 3-node chat with presence over two-way RPC,
//! channels and the mesh, built through `id_effect_kit`.
//!
//! This is the skeleton of leaf `r12-t0-skeletons`; leaf `r12-w7-relay` builds the app on it.

#![forbid(unsafe_code)]
#![deny(missing_docs)]

use id_effect_kit::prelude::*;

/// The line the binary prints on start.
fn banner() -> Effect<String, String> {
  succeed(format!("relay {}", env!("CARGO_PKG_VERSION")))
}

fn main() {
  if let Exit::Success(line) = banner().block_on() {
    println!("{line}");
  } else {
    std::process::exit(1);
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn banner_names_the_app_and_its_version() {
    let expected = format!("relay {}", env!("CARGO_PKG_VERSION"));
    assert!(matches!(banner().block_on(), Exit::Success(line) if line == expected));
  }
}

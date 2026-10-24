//! The Relay's manifest (F55): every id_effect battery comes through `id_effect_kit`, one
//! feature each. The only other id_effect crates are the two the macros expand to
//! (`::id_effect::` and `::id_effect_rpc::` paths), and they carry no feature of their own; the
//! local process layer comes from the kit's `process` re-export, not a dependency of its own.

use std::collections::BTreeMap;
use std::process::Command;

use serde_json::Value;

/// The id_effect crates the Relay may name directly: the kit, and the macro path crates.
const ALLOWED: [&str; 3] = ["id_effect", "id_effect_kit", "id_effect_rpc"];

/// The crates whose paths the macros expand to: present, with no feature of their own.
const MACRO_PATHS: [&str; 2] = ["id_effect", "id_effect_rpc"];

/// The Relay's package from `cargo metadata` (no dependency resolution, offline).
fn relay_package() -> Value {
  let manifest = concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml");
  let out = Command::new(env!("CARGO"))
    .args([
      "metadata",
      "--format-version",
      "1",
      "--no-deps",
      "--offline",
      "--manifest-path",
      manifest,
    ])
    .output()
    .expect("cargo metadata runs");
  assert!(
    out.status.success(),
    "cargo metadata failed: {}",
    String::from_utf8_lossy(&out.stderr)
  );
  let metadata: Value = serde_json::from_slice(&out.stdout).expect("cargo metadata is JSON");
  metadata["packages"]
    .as_array()
    .expect("packages")
    .iter()
    .find(|p| p["name"] == "relay")
    .cloned()
    .expect("the relay package")
}

/// The normal (not dev, not build) dependencies named `id_effect*`, by name.
fn id_effect_dependencies(package: &Value) -> BTreeMap<String, Value> {
  package["dependencies"]
    .as_array()
    .expect("dependencies")
    .iter()
    .filter(|d| d["kind"].is_null())
    .filter_map(|d| {
      let name = d["name"].as_str()?;
      name
        .starts_with("id_effect")
        .then(|| (name.to_owned(), d.clone()))
    })
    .collect()
}

fn features(dependency: &Value) -> Vec<&str> {
  dependency["features"]
    .as_array()
    .expect("features")
    .iter()
    .filter_map(Value::as_str)
    .collect()
}

#[test]
fn relay_depends_on_kit_only() {
  let relay = relay_package();
  let deps = id_effect_dependencies(&relay);
  let names: Vec<&str> = deps.keys().map(String::as_str).collect();
  assert_eq!(names, ALLOWED, "only the kit and the macro path crates");
  assert!(
    !deps.contains_key("id_effect_process"),
    "the process layer comes through the kit's `process` re-export"
  );
  for name in MACRO_PATHS {
    assert!(
      features(&deps[name]).is_empty(),
      "{name} takes no feature of its own: the kit picks them"
    );
  }
  // The batteries, one kit feature each: `cluster` brings the kit's `process` re-export,
  // `webtransport` the host's WebTransport (on `http3`).
  let kit = features(&deps["id_effect_kit"]);
  for feature in [
    "rpc",
    "rpc-ws",
    "http3",
    "webtransport",
    "cluster",
    "mesh-mtls",
    "events",
    "sql-pg",
    "auth",
    "authz",
  ] {
    assert!(kit.contains(&feature), "kit feature {feature}: {kit:?}");
  }
  assert_eq!(
    relay["metadata"]["id_effect"]["tier"], "example",
    "ADR 0010 tier"
  );
}

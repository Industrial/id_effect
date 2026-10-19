//! The service contract (ADR 0014 D11): a serde model of services, versions and procedures that
//! the `#[rpc::service]` macro generates (`Service::contract()`), `rpc.contract.json` stores, and
//! `id-effect rpc gen ts` / `id-effect rpc check` read without compiling anything.
//!
//! The JSON form is stable: services sorted by `(name, version)`, procedures by `name`, two-space
//! indentation and a trailing newline, so a checked-in snapshot only changes when the contract
//! does. [`assert_snapshot`] keeps that snapshot current.
//!
//! ```rust
//! use id_effect_rpc::contract::{
//!   Contract, ProcedureAccess, ProcedureContract, ProcedureKind, ServiceContract,
//! };
//! use serde_json::json;
//!
//! let greet = ProcedureContract::new(
//!   "greet",
//!   ProcedureKind::Query,
//!   ProcedureAccess::Public {},
//!   json!({"type": "string"}),
//!   json!({"type": "string"}),
//! );
//! let contract = Contract::new().with_service(ServiceContract::new("Greeter", 1).with_procedure(greet));
//! let text = contract.to_json_pretty();
//! assert_eq!(Contract::from_json(&text).unwrap(), contract);
//! assert!(text.contains("\"kind\": \"query\""));
//! ```

use std::fmt;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The contract format this crate reads and writes (`format` member).
pub const CONTRACT_FORMAT: &str = "id-effect.rpc.contract/1";

/// Environment variable that makes [`assert_snapshot`] rewrite the snapshot (`UPDATE_GOLDEN=1`);
/// `ID_EFFECT_BLESS=1` (the [`id_effect::testing`] golden switch) works too.
pub const UPDATE_GOLDEN_ENV: &str = "UPDATE_GOLDEN";

/// One of the six procedure kinds (ADR 0014 §2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProcedureKind {
  /// Read-only, retry-safe; `GET ?input=` with ETag/304, or POST.
  Query,
  /// State-changing; POST with `Idempotency-Key`.
  Mutation,
  /// One input, a stream of outputs (NDJSON, SSE, frames).
  ServerStream,
  /// A stream of inputs, one output (chunked NDJSON, frames).
  ClientStream,
  /// Two-way stream (WebSocket, WebTransport) with credits and resume.
  Bidi,
  /// A resumable stream of events from a cursor (SSE, WebSocket).
  Subscription,
}

impl ProcedureKind {
  /// Every kind, in contract order.
  pub const ALL: [Self; 6] = [
    Self::Query,
    Self::Mutation,
    Self::ServerStream,
    Self::ClientStream,
    Self::Bidi,
    Self::Subscription,
  ];

  /// The wire name (`query`, `server_stream`, …).
  pub fn as_str(self) -> &'static str {
    match self {
      Self::Query => "query",
      Self::Mutation => "mutation",
      Self::ServerStream => "server_stream",
      Self::ClientStream => "client_stream",
      Self::Bidi => "bidi",
      Self::Subscription => "subscription",
    }
  }

  /// Whether a call can be retried (or served by 0-RTT) without a second effect: queries only.
  pub fn is_retry_safe(self) -> bool {
    matches!(self, Self::Query)
  }

  /// Whether the call has one input and one output (the kinds a batch may carry).
  pub fn is_unary(self) -> bool {
    matches!(self, Self::Query | Self::Mutation)
  }
}

impl fmt::Display for ProcedureKind {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.write_str(self.as_str())
  }
}

/// A procedure's access decision (ADR 0014 C1): `{"public":{}}` or
/// `{"guarded":{"policies":[…]}}`. There is no undecided value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum ProcedureAccess {
  /// Anyone may call it, anonymous callers included.
  Public {},
  /// The named policies decide first (outermost last).
  Guarded {
    /// Policy names, outermost last.
    policies: Vec<String>,
  },
}

impl ProcedureAccess {
  /// Whether anyone may call the procedure.
  pub fn is_public(&self) -> bool {
    matches!(self, Self::Public {})
  }
}

/// One procedure of a service contract.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProcedureContract {
  /// Procedure name, unique within its service (the tag is `"<Service>.<name>"`).
  pub name: String,
  /// The call kind.
  pub kind: ProcedureKind,
  /// Who may call it.
  pub access: ProcedureAccess,
  /// JSON Schema (2020-12) of the input (of one input item for client streams and bidi).
  pub input: Value,
  /// JSON Schema of the output (of one item for streams and subscriptions).
  pub output: Value,
  /// JSON Schema of the typed error union, `null` when the procedure declares none.
  #[serde(default)]
  pub error: Option<Value>,
  /// Human-readable description.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub description: Option<String>,
  /// Deprecated procedures stay callable; generated clients warn.
  #[serde(default, skip_serializing_if = "is_false")]
  pub deprecated: bool,
  /// A mutation whose result `Idempotency-Key` replays (D14).
  #[serde(default, skip_serializing_if = "is_false")]
  pub idempotent: bool,
  /// Client streams: at most this many input items.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub max_items: Option<u64>,
  /// Client streams: at most this many input bytes.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub max_bytes: Option<u64>,
}

fn is_false(b: &bool) -> bool {
  !*b
}

impl ProcedureContract {
  /// A procedure with the required members; the optional ones are unset.
  pub fn new(
    name: impl Into<String>,
    kind: ProcedureKind,
    access: ProcedureAccess,
    input: Value,
    output: Value,
  ) -> Self {
    Self {
      name: name.into(),
      kind,
      access,
      input,
      output,
      error: None,
      description: None,
      deprecated: false,
      idempotent: false,
      max_items: None,
      max_bytes: None,
    }
  }

  /// Set the typed error union schema.
  #[must_use]
  pub fn with_error(mut self, error: Value) -> Self {
    self.error = Some(error);
    self
  }

  /// Set the description.
  #[must_use]
  pub fn with_description(mut self, description: impl Into<String>) -> Self {
    self.description = Some(description.into());
    self
  }
}

/// One version of one service.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ServiceContract {
  /// Service name (the trait name for `#[rpc::service]`).
  pub name: String,
  /// Contract version (≥ 1); versions are served side by side.
  pub version: u32,
  /// Human-readable description.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub description: Option<String>,
  /// Procedures, sorted by name.
  pub procedures: Vec<ProcedureContract>,
}

impl ServiceContract {
  /// An empty service.
  pub fn new(name: impl Into<String>, version: u32) -> Self {
    Self {
      name: name.into(),
      version,
      description: None,
      procedures: Vec::new(),
    }
  }

  /// Add `procedure`, keeping the procedures sorted by name.
  #[must_use]
  pub fn with_procedure(mut self, procedure: ProcedureContract) -> Self {
    self.procedures.push(procedure);
    self.procedures.sort_by(|a, b| a.name.cmp(&b.name));
    self
  }

  /// Set the description.
  #[must_use]
  pub fn with_description(mut self, description: impl Into<String>) -> Self {
    self.description = Some(description.into());
    self
  }

  /// The procedure called `name`.
  pub fn procedure(&self, name: &str) -> Option<&ProcedureContract> {
    self.procedures.iter().find(|p| p.name == name)
  }
}

/// A set of services: the content of `rpc.contract.json`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Contract {
  /// Always [`CONTRACT_FORMAT`].
  pub format: String,
  /// Services, sorted by `(name, version)`.
  pub services: Vec<ServiceContract>,
}

impl Default for Contract {
  fn default() -> Self {
    Self::new()
  }
}

/// A contract document that cannot be read.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ContractError {
  /// Not JSON, or not the contract shape.
  #[error("contract json: {0}")]
  Json(String),
  /// A `format` other than [`CONTRACT_FORMAT`].
  #[error("unsupported contract format `{0}` (expected `{CONTRACT_FORMAT}`)")]
  Format(String),
  /// Two services with the same name and version, or a version 0.
  #[error("invalid contract: {0}")]
  Invalid(String),
}

impl Contract {
  /// An empty contract.
  pub fn new() -> Self {
    Self {
      format: CONTRACT_FORMAT.to_owned(),
      services: Vec::new(),
    }
  }

  /// Add `service`, keeping the services sorted by `(name, version)`.
  #[must_use]
  pub fn with_service(mut self, service: ServiceContract) -> Self {
    self.services.push(service);
    self
      .services
      .sort_by(|a, b| (&a.name, a.version).cmp(&(&b.name, b.version)));
    self
  }

  /// The service `name` at `version`.
  pub fn service(&self, name: &str, version: u32) -> Option<&ServiceContract> {
    self
      .services
      .iter()
      .find(|s| s.name == name && s.version == version)
  }

  /// The stable JSON form (two-space indent, trailing newline).
  pub fn to_json_pretty(&self) -> String {
    // A tree of strings, numbers and `serde_json::Value`s always serializes.
    let mut text = serde_json::to_string_pretty(self).unwrap_or_default();
    text.push('\n');
    text
  }

  /// Read a contract document, refusing another `format`, version 0 and duplicate services.
  pub fn from_json(text: &str) -> Result<Self, ContractError> {
    let contract: Self =
      serde_json::from_str(text).map_err(|e| ContractError::Json(e.to_string()))?;
    if contract.format != CONTRACT_FORMAT {
      return Err(ContractError::Format(contract.format));
    }
    for (i, service) in contract.services.iter().enumerate() {
      if service.version == 0 {
        return Err(ContractError::Invalid(format!(
          "service `{}` has version 0",
          service.name
        )));
      }
      if contract.services[..i]
        .iter()
        .any(|s| s.name == service.name && s.version == service.version)
      {
        return Err(ContractError::Invalid(format!(
          "service `{}` version {} appears twice",
          service.name, service.version
        )));
      }
    }
    Ok(contract)
  }
}

/// Asserts that the snapshot at `path` equals `contract`'s JSON form, or rewrites it when
/// `UPDATE_GOLDEN=1` (or `ID_EFFECT_BLESS=1`) is set. A relative path resolves against the
/// crate under test (`CARGO_MANIFEST_DIR`), so `"rpc.contract.json"` is the crate root.
///
/// # Panics
///
/// When the snapshot is missing or differs (with a line diff and the rewrite hint), or cannot be
/// written.
#[track_caller]
pub fn assert_snapshot(contract: &Contract, path: impl AsRef<Path>) {
  let update = std::env::var_os(UPDATE_GOLDEN_ENV).is_some_and(|v| !v.is_empty() && v != "0");
  snapshot(contract, path.as_ref(), update);
}

#[track_caller]
fn snapshot(contract: &Contract, path: &Path, update: bool) {
  let text = contract.to_json_pretty();
  if update {
    let path = resolve(path);
    if let Some(dir) = path.parent() {
      std::fs::create_dir_all(dir)
        .unwrap_or_else(|e| panic!("contract snapshot: cannot create {}: {e}", dir.display()));
    }
    std::fs::write(&path, text.as_bytes())
      .unwrap_or_else(|e| panic!("contract snapshot: cannot write {}: {e}", path.display()));
    return;
  }
  id_effect::testing::assert_golden(path, &text);
}

fn resolve(path: &Path) -> PathBuf {
  if path.is_absolute() {
    return path.to_path_buf();
  }
  match std::env::var_os("CARGO_MANIFEST_DIR") {
    Some(dir) => PathBuf::from(dir).join(path),
    None => path.to_path_buf(),
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use serde_json::json;

  fn greeter() -> Contract {
    Contract::new()
      .with_service(
        ServiceContract::new("Greeter", 2).with_procedure(ProcedureContract::new(
          "greet",
          ProcedureKind::Query,
          ProcedureAccess::Public {},
          json!({"type": "string"}),
          json!({"type": "string"}),
        )),
      )
      .with_service(
        ServiceContract::new("Greeter", 1)
          .with_description("v1")
          .with_procedure(
            ProcedureContract::new(
              "shout",
              ProcedureKind::Mutation,
              ProcedureAccess::Guarded {
                policies: vec!["staff".to_owned()],
              },
              json!({"type": "string"}),
              json!({"type": "string"}),
            )
            .with_error(json!({"anyOf": []}))
            .with_description("loud"),
          )
          .with_procedure(ProcedureContract::new(
            "greet",
            ProcedureKind::Query,
            ProcedureAccess::Public {},
            json!({}),
            json!({}),
          )),
      )
  }

  #[test]
  fn kinds_have_wire_names_and_classes() {
    let names: Vec<_> = ProcedureKind::ALL.iter().map(|k| k.to_string()).collect();
    assert_eq!(
      names,
      [
        "query",
        "mutation",
        "server_stream",
        "client_stream",
        "bidi",
        "subscription"
      ]
    );
    for kind in ProcedureKind::ALL {
      let json = serde_json::to_value(kind).unwrap();
      assert_eq!(json, json!(kind.as_str()));
      assert_eq!(kind.is_retry_safe(), kind == ProcedureKind::Query);
    }
    assert!(ProcedureKind::Mutation.is_unary());
    assert!(!ProcedureKind::Bidi.is_unary());
  }

  #[test]
  fn access_serializes_as_documented() {
    assert_eq!(
      serde_json::to_value(ProcedureAccess::Public {}).unwrap(),
      json!({"public": {}})
    );
    let guarded = ProcedureAccess::Guarded {
      policies: vec!["a".into()],
    };
    assert_eq!(
      serde_json::to_value(&guarded).unwrap(),
      json!({"guarded": {"policies": ["a"]}})
    );
    assert!(ProcedureAccess::Public {}.is_public());
    assert!(!guarded.is_public());
    assert!(serde_json::from_value::<ProcedureAccess>(json!("undecided")).is_err());
  }

  #[test]
  fn services_and_procedures_are_sorted_and_found() {
    let c = greeter();
    assert_eq!(c.services[0].version, 1);
    assert_eq!(c.services[0].procedures[0].name, "greet");
    let v1 = c.service("Greeter", 1).unwrap();
    assert_eq!(v1.procedure("shout").unwrap().kind, ProcedureKind::Mutation);
    assert!(v1.procedure("nope").is_none());
    assert!(c.service("Greeter", 3).is_none());
    assert_eq!(Contract::default(), Contract::new());
  }

  #[test]
  fn json_form_is_stable_and_omits_unset_members() {
    let text = greeter().to_json_pretty();
    assert!(text.ends_with("}\n"));
    assert!(text.contains("\"format\": \"id-effect.rpc.contract/1\""));
    assert!(text.contains("\"error\": null"));
    assert!(!text.contains("deprecated"));
    assert!(!text.contains("max_items"));
    assert_eq!(text, Contract::from_json(&text).unwrap().to_json_pretty());
  }

  #[test]
  fn from_json_refuses_bad_documents() {
    assert!(matches!(
      Contract::from_json("nope"),
      Err(ContractError::Json(_))
    ));
    let other = json!({"format": "x/9", "services": []}).to_string();
    let err = Contract::from_json(&other).unwrap_err();
    assert_eq!(err, ContractError::Format("x/9".into()));
    assert!(
      err
        .to_string()
        .contains("expected `id-effect.rpc.contract/1`")
    );
    let zero = json!({"format": CONTRACT_FORMAT, "services": [
      {"name": "A", "version": 0, "procedures": []}
    ]})
    .to_string();
    assert!(matches!(
      Contract::from_json(&zero),
      Err(ContractError::Invalid(_))
    ));
    let twice = json!({"format": CONTRACT_FORMAT, "services": [
      {"name": "A", "version": 1, "procedures": []},
      {"name": "A", "version": 1, "procedures": []}
    ]})
    .to_string();
    let err = Contract::from_json(&twice).unwrap_err();
    assert!(err.to_string().contains("appears twice"));
  }

  #[test]
  fn snapshot_writes_then_compares() {
    let dir = std::env::temp_dir().join(format!("id_effect_rpc_snap_{}", std::process::id()));
    let path = dir.join("nested").join("rpc.contract.json");
    snapshot(&greeter(), &path, true);
    assert_eq!(
      std::fs::read_to_string(&path).unwrap(),
      greeter().to_json_pretty()
    );
    snapshot(&greeter(), &path, false);
    assert_snapshot(&greeter(), &path);
    let changed = std::panic::catch_unwind(|| snapshot(&Contract::new(), &path, false));
    assert!(changed.is_err());
    std::fs::remove_dir_all(&dir).unwrap();
  }

  #[test]
  fn relative_snapshot_paths_resolve_against_the_crate() {
    let resolved = resolve(Path::new("rpc.contract.json"));
    assert!(resolved.ends_with("id_effect_rpc/rpc.contract.json"));
    let abs = std::env::temp_dir().join("x.json");
    assert_eq!(resolve(&abs), abs);
  }
}

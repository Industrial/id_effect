//! RPC service metadata and Rust trait stub emission (Phase D3 spike).
//!
//! Callers describe HTTP-shaped RPC operations with [`RpcServiceDef`], then
//! [`emit_service_trait`] returns a Rust source string suitable for `build.rs`
//! or test fixtures.

use std::fmt::Write as _;

/// HTTP verb for an RPC operation at the wire edge.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RpcHttpMethod {
  /// GET
  Get,
  /// POST
  Post,
  /// PUT
  Put,
  /// DELETE
  Delete,
  /// PATCH
  Patch,
}

impl RpcHttpMethod {
  /// Uppercase method name (`GET`, `POST`, …).
  #[inline]
  pub fn as_str(self) -> &'static str {
    match self {
      Self::Get => "GET",
      Self::Post => "POST",
      Self::Put => "PUT",
      Self::Delete => "DELETE",
      Self::Patch => "PATCH",
    }
  }
}

/// Metadata for one RPC operation (maps to an Axum route + handler).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RpcMethodDef {
  /// Stable operation id (Rust fn name and OpenAPI `operationId`).
  pub operation: String,
  /// Route path (e.g. `/greet`).
  pub path: String,
  /// HTTP method.
  pub method: RpcHttpMethod,
  /// Optional short description for docs and OpenAPI.
  pub summary: Option<String>,
  /// Request payload Rust type name (when the route accepts a body).
  pub request_type: Option<String>,
  /// Success response Rust type name.
  pub response_type: Option<String>,
}

/// Collection of operations exposed as one logical service.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RpcServiceDef {
  /// Service / trait name (PascalCase recommended).
  pub service: String,
  /// Operations belonging to this service.
  pub methods: Vec<RpcMethodDef>,
}

/// Emit a Rust trait stub with one method per [`RpcMethodDef`].
///
/// Generated code depends on `id_effect::Effect` and [`crate::RpcError`].
pub fn emit_service_trait(def: &RpcServiceDef) -> String {
  let mut out = String::new();
  let _ = writeln!(
    out,
    "//! Generated RPC service trait for `{}`.\n\
     //!\n\
     //! Emit source via `id_effect_rpc::codegen::emit_service_trait`.",
    def.service
  );
  let _ = writeln!(out, "#![allow(dead_code, missing_docs)]");
  let _ = writeln!(out);
  let _ = writeln!(out, "use id_effect::Effect;");
  let _ = writeln!(out, "use id_effect_rpc::RpcError;");
  let _ = writeln!(out);
  let _ = writeln!(
    out,
    "/// RPC service trait — implement on your domain type or Axum state."
  );
  let _ = writeln!(out, "pub trait {} {{", def.service);

  for method in &def.methods {
    let req = method.request_type.as_deref().unwrap_or("()");
    let resp = method.response_type.as_deref().unwrap_or("()");
    let doc = match &method.summary {
      Some(s) => format!("{} {} {}", method.method.as_str(), method.path, s),
      None => format!("{} {}", method.method.as_str(), method.path),
    };
    let _ = writeln!(out, "  /// {doc}");
    let _ = writeln!(
      out,
      "  fn {}(&self, request: {req}) -> Effect<{resp}, RpcError, ()>;",
      method.operation
    );
    let _ = writeln!(out);
  }

  let _ = writeln!(out, "}}");
  out
}

#[cfg(test)]
mod tests {
  use super::*;

  fn greet_fixture() -> RpcServiceDef {
    RpcServiceDef {
      service: "GreetService".to_owned(),
      methods: vec![RpcMethodDef {
        operation: "greet".to_owned(),
        path: "/greet".to_owned(),
        method: RpcHttpMethod::Post,
        summary: Some("Say hello".to_owned()),
        request_type: Some("GreetRequest".to_owned()),
        response_type: Some("GreetResponse".to_owned()),
      }],
    }
  }

  #[test]
  fn emit_service_trait_includes_trait_name_and_methods() {
    let src = emit_service_trait(&greet_fixture());
    assert!(src.contains("pub trait GreetService"));
    assert!(src.contains("fn greet(&self, request: GreetRequest)"));
    assert!(src.contains("Effect<GreetResponse, RpcError, ()>"));
    assert!(src.contains("POST /greet"));
  }

  #[test]
  fn emit_service_trait_uses_unit_when_types_omitted() {
    let def = RpcServiceDef {
      service: "PingService".to_owned(),
      methods: vec![RpcMethodDef {
        operation: "ping".to_owned(),
        path: "/ping".to_owned(),
        method: RpcHttpMethod::Get,
        summary: None,
        request_type: None,
        response_type: None,
      }],
    };
    let src = emit_service_trait(&def);
    assert!(src.contains("fn ping(&self, request: ())"));
    assert!(src.contains("Effect<(), RpcError, ()>"));
  }
}

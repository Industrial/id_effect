//! Integration test: OpenAPI emission matches checked-in fixture.

use id_effect_rpc::codegen::{RpcHttpMethod, RpcMethodDef, RpcServiceDef};
use id_effect_rpc::openapi::{OpenApiInfo, OpenApiSpec, emit_openapi_json};
use serde_json::Value;

fn greet_spec() -> OpenApiSpec {
  OpenApiSpec {
    info: OpenApiInfo {
      title: "Greet API".to_owned(),
      version: "1.0.0".to_owned(),
    },
    service: RpcServiceDef {
      service: "GreetService".to_owned(),
      methods: vec![RpcMethodDef {
        operation: "greet".to_owned(),
        path: "/greet".to_owned(),
        method: RpcHttpMethod::Post,
        summary: Some("Say hello".to_owned()),
        request_type: Some("GreetRequest".to_owned()),
        response_type: Some("GreetResponse".to_owned()),
      }],
    },
  }
}

#[test]
fn emitted_openapi_json_matches_fixture() {
  let emitted = emit_openapi_json(&greet_spec()).expect("emit json");
  let emitted_v: Value = serde_json::from_str(&emitted).expect("parse emitted");
  let fixture = include_str!("fixtures/greet_openapi.json");
  let fixture_v: Value = serde_json::from_str(fixture).expect("parse fixture");
  assert_eq!(emitted_v, fixture_v);
}

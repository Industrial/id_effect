//! Integration test: generated service trait source is syntactically valid Rust.

use id_effect_rpc::codegen::{RpcHttpMethod, RpcMethodDef, RpcServiceDef, emit_service_trait};

#[test]
fn generated_greet_service_trait_parses_as_rust() {
  let def = RpcServiceDef {
    service: "GreetService".to_owned(),
    methods: vec![RpcMethodDef {
      operation: "greet".to_owned(),
      path: "/greet".to_owned(),
      method: RpcHttpMethod::Post,
      summary: Some("Say hello".to_owned()),
      request_type: Some("GreetRequest".to_owned()),
      response_type: Some("GreetResponse".to_owned()),
    }],
  };

  let src = emit_service_trait(&def);
  let syntax = syn::parse_file(&src).expect("generated stub should parse as Rust");
  assert_eq!(syntax.items.len(), 3, "use, use, trait");
  assert!(src.contains("pub trait GreetService"));
}

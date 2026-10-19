//! D11: the contract model round-trips through its JSON form, and an `RpcGroup` produces it.

use std::sync::Arc;

use id_effect::{schema, succeed};
use id_effect_rpc::contract::{CONTRACT_FORMAT, assert_snapshot};
use id_effect_rpc::{
  Contract, Procedure, ProcedureAccess, ProcedureContract, ProcedureKind, RpcGroup, ServiceContract,
};
use serde_json::json;

fn every_kind() -> ServiceContract {
  ProcedureKind::ALL.iter().fold(
    ServiceContract::new("Chat", 1).with_description("chat rooms"),
    |service, kind| {
      service.with_procedure(
        ProcedureContract::new(
          format!("p_{kind}"),
          *kind,
          ProcedureAccess::Guarded {
            policies: vec!["member".into()],
          },
          json!({"type": "string"}),
          json!({"type": "integer"}),
        )
        .with_error(json!({"anyOf": [{"properties": {"_tag": {"const": "Closed"}}}]})),
      )
    },
  )
}

#[test]
fn contract_json_round_trips() {
  let mut group = RpcGroup::new();
  group.register(
    Procedure::query("Greeter.greet")
      .public()
      .describe("Says hello"),
    Arc::new(schema::struct_(
      "name",
      schema::string::<()>(),
      "times",
      schema::i64::<()>(),
    )),
    Arc::new(schema::string::<()>()),
    &["message"],
    |(name, _), _| succeed(name),
  );
  let contract = Contract::new()
    .with_service(group.service_contract("Greeter", 1))
    .with_service(every_kind());

  let text = contract.to_json_pretty();
  let back = Contract::from_json(&text).expect("reads its own output");
  assert_eq!(back, contract);
  assert_eq!(back.to_json_pretty(), text, "stable form");

  let value: serde_json::Value = serde_json::from_str(&text).unwrap();
  assert_eq!(value["format"], CONTRACT_FORMAT);
  assert_eq!(value["services"][0]["name"], "Chat");
  let greet = &value["services"][1]["procedures"][0];
  assert_eq!(greet["name"], "greet");
  assert_eq!(greet["kind"], "query");
  assert_eq!(greet["access"], json!({"public": {}}));
  assert_eq!(greet["input"]["required"], json!(["name", "times"]));
  assert_eq!(greet["output"]["type"], "string");
  assert_eq!(greet["description"], "Says hello");
  let kinds: Vec<_> = back.services[0].procedures.iter().map(|p| p.kind).collect();
  assert_eq!(kinds.len(), 6);

  let dir = std::env::temp_dir().join(format!("id_effect_rpc_contract_{}", std::process::id()));
  std::fs::create_dir_all(&dir).unwrap();
  let path = dir.join("rpc.contract.json");
  std::fs::write(&path, &text).unwrap();
  assert_snapshot(&contract, &path);
  std::fs::remove_dir_all(&dir).unwrap();
}

//! `id-effect rpc check` (r12-e12-4-rpc-check): breaking-change detection against the previous
//! contract (design note `r12-rpc-design` §1, `web.rpc#6`) and the staleness check of a generated
//! TypeScript client. The contracts are variations of `id_effect_rpc`'s fixture
//! (`crates/id_effect_rpc/rpc.contract.json`).

use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use serde_json::{Value, json};

fn fixture() -> Value {
  let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../id_effect_rpc/rpc.contract.json");
  let text = std::fs::read_to_string(path).expect("fixture contract");
  serde_json::from_str(&text).expect("fixture json")
}

/// The service `name` at `version` of `contract`.
fn service<'a>(contract: &'a mut Value, name: &str, version: u64) -> &'a mut Value {
  contract["services"]
    .as_array_mut()
    .and_then(|s| {
      s.iter_mut()
        .find(|s| s["name"] == name && s["version"] == version)
    })
    .expect("service")
}

/// The procedure `name` of `Todos` v1.
fn todos<'a>(contract: &'a mut Value, name: &str) -> &'a mut Value {
  service(contract, "Todos", 1)["procedures"]
    .as_array_mut()
    .and_then(|p| p.iter_mut().find(|p| p["name"] == name))
    .expect("procedure")
}

fn write(dir: &Path, name: &str, contract: &Value) -> PathBuf {
  let path = dir.join(name);
  std::fs::write(&path, serde_json::to_string_pretty(contract).expect("json")).expect("write");
  path
}

struct Run {
  code: ExitCode,
  out: String,
  err: String,
}

fn tool(args: &[&str]) -> Run {
  let (mut out, mut err) = (Vec::new(), Vec::new());
  let mut argv = vec!["id-effect", "rpc"];
  argv.extend_from_slice(args);
  let code = id_effect_tool::cli::run(argv, &mut out, &mut err);
  Run {
    code,
    out: String::from_utf8_lossy(&out).into_owned(),
    err: String::from_utf8_lossy(&err).into_owned(),
  }
}

/// `rpc check --contract <new> --against <old>` in a fresh directory.
fn check(old: &Value, new: &Value) -> Run {
  let dir = tempfile::tempdir().expect("tempdir");
  let old = write(dir.path(), "old.json", old);
  let new = write(dir.path(), "rpc.contract.json", new);
  tool(&[
    "check",
    "--contract",
    new.to_str().expect("utf-8"),
    "--against",
    old.to_str().expect("utf-8"),
  ])
}

#[test]
fn an_unchanged_contract_passes() {
  let run = check(&fixture(), &fixture());
  assert_eq!(run.code, ExitCode::SUCCESS, "{}{}", run.out, run.err);
  assert!(
    run
      .out
      .contains("0 changes: 0 breaking, 0 versioned, 0 compatible")
  );
}

#[test]
fn removed_procedure_is_breaking() {
  let old = fixture();
  let mut new = fixture();
  service(&mut new, "Todos", 1)["procedures"]
    .as_array_mut()
    .expect("procedures")
    .retain(|p| p["name"] != "count");
  let run = check(&old, &new);
  assert_eq!(run.code, ExitCode::FAILURE, "{}", run.out);
  assert!(
    run
      .out
      .contains("breaking   Todos v1 count: procedure removed")
  );
  assert!(
    run
      .err
      .contains("1 breaking change(s) within a published version")
  );
}

#[test]
fn optional_field_added_is_compatible() {
  let old = fixture();
  let mut new = fixture();
  todos(&mut new, "add")["input"]["properties"]["note"] = json!({"type": "string"});
  // An output field is additive as well.
  todos(&mut new, "get")["output"]["properties"]["tags"] =
    json!({"type": "array", "items": {"type": "string"}});
  let run = check(&old, &new);
  assert_eq!(run.code, ExitCode::SUCCESS, "{}{}", run.out, run.err);
  assert!(
    run
      .out
      .contains("compatible Todos v1 add input.note: optional field added")
  );
  assert!(
    run
      .out
      .contains("compatible Todos v1 get output.tags: optional field added")
  );
}

#[test]
fn required_field_added_is_breaking() {
  let old = fixture();
  let mut new = fixture();
  let add = todos(&mut new, "add");
  add["input"]["properties"]["priority"] = json!({"type": "integer"});
  add["input"]["required"] = json!(["title", "priority"]);
  let run = check(&old, &new);
  assert_eq!(run.code, ExitCode::FAILURE, "{}", run.out);
  assert!(
    run
      .out
      .contains("breaking   Todos v1 add input.priority: required field added")
  );
}

#[test]
fn error_variant_added_is_breaking_for_clients() {
  let old = fixture();
  let mut new = fixture();
  todos(&mut new, "get")["error"]["oneOf"]
    .as_array_mut()
    .expect("union")
    .push(
      json!({"type": "object", "properties": {"_tag": {"const": "Gone"}}, "required": ["_tag"]}),
    );
  let run = check(&old, &new);
  assert_eq!(run.code, ExitCode::FAILURE, "{}", run.out);
  assert!(run.out.contains(
    "breaking   Todos v1 get error: error variant `Gone` added (old clients cannot decode it)"
  ));
  // Removing one is compatible: the server just never sends it.
  let run = check(&new, &old);
  assert_eq!(run.code, ExitCode::SUCCESS, "{}", run.out);
  assert!(
    run
      .out
      .contains("compatible Todos v1 get error: error variant `Gone` removed")
  );
}

#[test]
fn breaking_change_with_version_bump_passes() {
  // Previous: Greeter v1 only. Current: v1 unchanged and v2 with a required input field.
  let mut old = fixture();
  old["services"]
    .as_array_mut()
    .expect("services")
    .retain(|s| !(s["name"] == "Greeter" && s["version"] == 2));
  let new = fixture();
  let run = check(&old, &new);
  assert_eq!(run.code, ExitCode::SUCCESS, "{}{}", run.out, run.err);
  assert!(
    run
      .out
      .contains("compatible Greeter v2: version added (compared with v1)")
  );
  assert!(run.out.contains("versioned  Greeter v2 greet input"));
  assert!(run.out.contains("(new version; v1 is still served)"));
  assert!(
    run
      .out
      .contains("compatible Greeter v2 history: procedure added")
  );

  // The same change without the bump fails, and so does dropping the previous version.
  let mut in_place = fixture();
  let v2 = service(&mut in_place, "Greeter", 2).clone();
  in_place["services"]
    .as_array_mut()
    .expect("services")
    .retain(|s| s["name"] != "Greeter");
  let mut as_v1 = v2;
  as_v1["version"] = json!(1);
  in_place["services"]
    .as_array_mut()
    .expect("services")
    .push(as_v1.clone());
  let run = check(&old, &in_place);
  assert_eq!(run.code, ExitCode::FAILURE, "{}", run.out);
  assert!(run.out.contains("breaking   Greeter v1 greet input"));

  let mut dropped = old.clone();
  dropped["services"]
    .as_array_mut()
    .expect("services")
    .retain(|s| s["name"] != "Greeter");
  let mut only_v2 = as_v1;
  only_v2["version"] = json!(2);
  dropped["services"]
    .as_array_mut()
    .expect("services")
    .push(only_v2);
  let run = check(&old, &dropped);
  assert_eq!(run.code, ExitCode::FAILURE, "{}", run.out);
  assert!(
    run
      .out
      .contains("breaking   Greeter v1: version no longer served")
  );
  assert!(run.out.contains("(new version; v1 is no longer served)"));
}

#[test]
fn moved_module_is_not_a_change() {
  let with_def = |key: &str| {
    let mut c = fixture();
    todos(&mut c, "get")["output"] = json!({
      "$schema": "https://json-schema.org/draft/2020-12/schema",
      "$ref": format!("#/$defs/{key}"),
      "$defs": {key: {"type": "object", "properties": {"id": {"type": "integer"}}, "required": ["id"]}}
    });
    c
  };
  let run = check(&with_def("app::v1::Todo"), &with_def("app::store::Todo"));
  assert_eq!(run.code, ExitCode::SUCCESS, "{}{}", run.out, run.err);
  assert!(run.out.contains("0 changes"), "{}", run.out);
}

#[test]
fn access_tightened_and_kind_changed_are_breaking() {
  let old = fixture();
  let mut new = fixture();
  todos(&mut new, "count")["access"] = json!({"guarded": {"policies": ["staff"]}});
  todos(&mut new, "titles")["kind"] = json!("server_stream");
  todos(&mut new, "add")
    .as_object_mut()
    .expect("object")
    .remove("idempotent");
  todos(&mut new, "sum")["max_items"] = json!(10);
  let run = check(&old, &new);
  assert_eq!(run.code, ExitCode::FAILURE, "{}", run.out);
  for line in [
    "breaking   Todos v1 count: access tightened: public, now guarded by `staff`",
    "breaking   Todos v1 titles: kind changed from `subscription` to `server_stream`",
    "breaking   Todos v1 add: no longer idempotent",
    "breaking   Todos v1 sum: max_items lowered from 100 to 10",
  ] {
    assert!(run.out.contains(line), "{line}\n{}", run.out);
  }
}

fn git(dir: &Path, args: &[&str]) {
  let status = Command::new("git")
    .args([
      "-c",
      "user.name=rpc-check",
      "-c",
      "user.email=rpc-check@example.invalid",
      "-c",
      "commit.gpgsign=false",
    ])
    .args(args)
    .current_dir(dir)
    .env_remove("GIT_DIR")
    .env_remove("GIT_INDEX_FILE")
    .env_remove("GIT_WORK_TREE")
    .status()
    .expect("git runs");
  assert!(status.success(), "git {args:?}");
}

#[test]
fn against_git_ref_reads_old_contract() {
  let repo = tempfile::tempdir().expect("tempdir");
  git(repo.path(), &["init", "-q"]);
  let api = repo.path().join("api");
  std::fs::create_dir(&api).expect("mkdir");
  let contract = write(&api, "rpc.contract.json", &fixture());
  git(repo.path(), &["add", "api/rpc.contract.json"]);
  git(repo.path(), &["commit", "-q", "-m", "contract"]);
  git(repo.path(), &["tag", "v1"]);

  let mut new = fixture();
  service(&mut new, "Todos", 1)["procedures"]
    .as_array_mut()
    .expect("procedures")
    .retain(|p| p["name"] != "shout");
  write(&api, "rpc.contract.json", &new);
  let contract = contract.to_str().expect("utf-8");

  // A path from the repository root, and one relative to the contract's directory.
  for against in ["v1:api/rpc.contract.json", "HEAD:./rpc.contract.json"] {
    let run = tool(&["check", "--contract", contract, "--against", against]);
    assert_eq!(
      run.code,
      ExitCode::FAILURE,
      "{against}: {}{}",
      run.out,
      run.err
    );
    assert!(
      run
        .out
        .contains("breaking   Todos v1 shout: procedure removed")
    );
    assert!(run.out.contains(&format!("against {against}")));
  }

  // A missing ref or path is unreadable input.
  let run = tool(&[
    "check",
    "--contract",
    contract,
    "--against",
    "v9:api/rpc.contract.json",
  ]);
  assert_eq!(run.code, ExitCode::from(2), "{}", run.out);
  assert!(
    run
      .err
      .contains("cannot read `v9:api/rpc.contract.json` from git")
  );
  let run = tool(&["check", "--contract", contract, "--against", "--x:api"]);
  assert_eq!(run.code, ExitCode::from(2));

  // A blob that is not a contract.
  std::fs::write(repo.path().join("notes.txt"), "hello").expect("write");
  git(repo.path(), &["add", "notes.txt"]);
  git(repo.path(), &["commit", "-q", "-m", "notes"]);
  let run = tool(&[
    "check",
    "--contract",
    contract,
    "--against",
    "HEAD:notes.txt",
  ]);
  assert_eq!(run.code, ExitCode::from(2));
  assert!(
    run.err.contains("HEAD:notes.txt: contract json"),
    "{}",
    run.err
  );
}

#[test]
fn unreadable_input_exits_2() {
  let dir = tempfile::tempdir().expect("tempdir");
  let good = write(dir.path(), "good.json", &fixture());
  let good = good.to_str().expect("utf-8");
  let bad = dir.path().join("bad.json");
  std::fs::write(&bad, "{\"format\": \"other/1\", \"services\": []}").expect("write");
  let bad = bad.to_str().expect("utf-8");
  let missing = dir.path().join("missing.json");
  let missing = missing.to_str().expect("utf-8");
  for (contract, against) in [(missing, good), (bad, good), (good, missing), (good, bad)] {
    let run = tool(&["check", "--contract", contract, "--against", against]);
    assert_eq!(run.code, ExitCode::from(2), "{contract} {against}");
    assert!(run.err.starts_with("error: "), "{}", run.err);
  }
  // Neither --against nor --generated: a usage error.
  let run = tool(&["check", "--contract", good]);
  assert_eq!(run.code, ExitCode::from(2));
  assert!(run.err.contains("--against"), "{}", run.err);
}

#[test]
fn generated_client_must_be_current() {
  let dir = tempfile::tempdir().expect("tempdir");
  let contract = write(dir.path(), "rpc.contract.json", &fixture());
  let contract = contract.to_str().expect("utf-8");
  let out = dir.path().join("web");
  let out_s = out.to_str().expect("utf-8");
  let run = tool(&["gen", "ts", "--contract", contract, "--out", out_s]);
  assert_eq!(run.code, ExitCode::SUCCESS, "{}", run.err);

  let run = tool(&["check", "--contract", contract, "--generated", out_s]);
  assert_eq!(run.code, ExitCode::SUCCESS, "{}", run.err);
  assert!(run.out.contains("is current"));

  // Both checks at once.
  let run = tool(&[
    "check",
    "--contract",
    contract,
    "--against",
    contract,
    "--generated",
    out_s,
  ]);
  assert_eq!(run.code, ExitCode::SUCCESS, "{}", run.err);

  // A contract change the client was not regenerated for.
  let mut new = fixture();
  todos(&mut new, "add")["input"]["properties"]["note"] = json!({"type": "string"});
  write(dir.path(), "rpc.contract.json", &new);
  let run = tool(&["check", "--contract", contract, "--generated", out_s]);
  assert_eq!(run.code, ExitCode::FAILURE);
  assert!(run.out.contains("stale "), "{}", run.out);
  assert!(run.out.contains("types.ts"), "{}", run.out);
  assert!(
    run.err.contains("id-effect rpc gen ts --contract"),
    "{}",
    run.err
  );

  // A missing file is stale too.
  let run = tool(&["gen", "ts", "--contract", contract, "--out", out_s]);
  assert_eq!(run.code, ExitCode::SUCCESS);
  std::fs::remove_file(out.join("runtime.ts")).expect("remove");
  let run = tool(&["check", "--contract", contract, "--generated", out_s]);
  assert_eq!(run.code, ExitCode::FAILURE);
  assert!(run.out.contains("runtime.ts"));

  // A contract that cannot become TypeScript fails the generated check.
  let mut clash = fixture();
  service(&mut clash, "Todos", 1)["procedures"]
    .as_array_mut()
    .expect("procedures")
    .push(
      json!({"name": "bad name!", "kind": "query", "access": {"public": {}},
      "input": {"type": "null"}, "output": {"type": "null"}, "error": null}),
    );
  write(dir.path(), "rpc.contract.json", &clash);
  let run = tool(&["check", "--contract", contract, "--generated", out_s]);
  assert_eq!(run.code, ExitCode::FAILURE);
  assert!(run.err.starts_with("error: "), "{}", run.err);
}

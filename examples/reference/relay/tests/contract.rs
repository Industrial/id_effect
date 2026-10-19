//! The Relay's contract (D11): `rpc.contract.json` is the served contract (`UPDATE_GOLDEN=1
//! cargo test` rewrites it, and the TypeScript client in `web/src/rpc` with it), `id-effect rpc
//! check` passes against the last release tag's contract and against the checked-in TypeScript
//! client, and the generated TypeScript client chats with a running node under Deno.

mod common;

use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::time::Duration;

use common::{Options, cluster, database, run_typed};
use id_effect_kit::rpc::contract::{Contract, assert_snapshot};
use relay::api::Invite;

fn manifest() -> PathBuf {
  PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn bless() -> bool {
  ["UPDATE_GOLDEN", "ID_EFFECT_BLESS"]
    .iter()
    .any(|k| std::env::var_os(k).is_some_and(|v| !v.is_empty() && v != "0"))
}

/// `id-effect <args>` in process: its exit code and output.
fn id_effect(args: &[&str]) -> (ExitCode, String) {
  let (mut out, mut err) = (Vec::new(), Vec::new());
  let code = id_effect_tool::cli::run(
    std::iter::once("id-effect").chain(args.iter().copied()),
    &mut out,
    &mut err,
  );
  let text = format!(
    "{}{}",
    String::from_utf8_lossy(&out),
    String::from_utf8_lossy(&err)
  );
  (code, text)
}

/// The latest release tag, when the checkout has tags.
fn last_tag() -> Option<String> {
  let out = Command::new("git")
    .args(["describe", "--tags", "--abbrev=0"])
    .current_dir(manifest())
    .output()
    .ok()?;
  let tag = String::from_utf8(out.stdout).ok()?.trim().to_owned();
  (out.status.success() && !tag.is_empty()).then_some(tag)
}

/// The contract's path in the repository (what `<tag>:<path>` names).
const REPO_PATH: &str = "examples/reference/relay/rpc.contract.json";

#[test]
fn relay_contract_check_passes() {
  let contract = manifest().join("rpc.contract.json");
  let generated = manifest().join("web/src/rpc");
  if bless() {
    std::fs::create_dir_all(&generated).unwrap();
    id_effect_tool::rpc::gen_ts::generate_to(&write_contract(&contract), &generated).unwrap();
  }
  // The checked-in contract is the one the Relay serves.
  assert_snapshot(&relay::contract(), &contract);
  let contract_arg = contract.to_str().unwrap();
  let generated_arg = generated.to_str().unwrap();
  // Against the last release: its contract when it shipped one (the Relay's first is 0.12).
  let released = last_tag()
    .filter(|tag| {
      Command::new("git")
        .args(["cat-file", "-e", &format!("{tag}:{REPO_PATH}")])
        .current_dir(manifest())
        .status()
        .is_ok_and(|s| s.success())
    })
    .map(|tag| format!("{tag}:{REPO_PATH}"));
  let against = released.unwrap_or_else(|| contract_arg.to_owned());
  let (code, out) = id_effect(&[
    "rpc",
    "check",
    "--contract",
    contract_arg,
    "--against",
    &against,
    "--generated",
    generated_arg,
  ]);
  assert_eq!(code, ExitCode::SUCCESS, "{out}");

  // The check bites: a procedure removed from version 1 is a breaking change.
  let dir = std::env::temp_dir().join(format!("relay-contract-{}", std::process::id()));
  std::fs::create_dir_all(&dir).unwrap();
  let mut narrowed = relay::contract();
  narrowed.services[0]
    .procedures
    .retain(|p| p.name != "history");
  let narrowed_path = dir.join("rpc.contract.json");
  std::fs::write(&narrowed_path, narrowed.to_json_pretty()).unwrap();
  let (code, out) = id_effect(&[
    "rpc",
    "check",
    "--contract",
    narrowed_path.to_str().unwrap(),
    "--against",
    contract_arg,
  ]);
  assert_eq!(code, ExitCode::FAILURE, "{out}");
  assert!(out.contains("breaking"), "{out}");
  // And a stale TypeScript client is caught.
  let (code, out) = id_effect(&[
    "rpc",
    "check",
    "--contract",
    narrowed_path.to_str().unwrap(),
    "--against",
    narrowed_path.to_str().unwrap(),
    "--generated",
    generated_arg,
  ]);
  assert_eq!(code, ExitCode::FAILURE, "{out}");
  let _ = std::fs::remove_dir_all(dir);
}

/// Writes the served contract to `path` (bless mode) and returns the path.
fn write_contract(path: &Path) -> PathBuf {
  let contract: Contract = relay::contract();
  std::fs::write(path, contract.to_json_pretty()).unwrap();
  path.to_path_buf()
}

/// `deno` on `PATH`, or `None` (a failure under `ID_EFFECT_REQUIRE_DENO=1`).
fn deno() -> Option<PathBuf> {
  let found = std::env::var_os("PATH").and_then(|paths| {
    std::env::split_paths(&paths)
      .map(|dir| dir.join("deno"))
      .find(|p| p.is_file())
  });
  let required = std::env::var("ID_EFFECT_REQUIRE_DENO").is_ok_and(|v| v == "1");
  match found {
    Some(path) => Some(path),
    None if required => panic!("ID_EFFECT_REQUIRE_DENO=1 but `deno` is not on PATH"),
    None => {
      eprintln!("SKIP: `deno` is not on PATH (ID_EFFECT_REQUIRE_DENO=1 fails instead)");
      None
    }
  }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn relay_typescript_client_round_trip() {
  let Some(deno) = deno() else { return };
  let Some(db) = database("relay_typescript_client_round_trip").await else {
    return;
  };
  let c = cluster(
    db,
    Options {
      nodes: 2,
      ..Options::default()
    },
  )
  .await;
  // ada (Rust) makes the room; the TypeScript client is bob on node b.
  let ada = c.http(0, "ada").await;
  run_typed(ada.create_room("general".to_owned()))
    .await
    .unwrap();
  run_typed(ada.invite(Invite {
    room: "general".to_owned(),
    user: "bob".to_owned(),
  }))
  .await
  .unwrap();
  // The first message of the room, from Rust; the TypeScript client posts the second.
  run_typed(ada.send(relay::api::Outgoing {
    room: "general".to_owned(),
    text: "hello from rust".to_owned(),
  }))
  .await
  .unwrap();
  let token = c.token("bob").await;
  let url = format!("{}/rpc", c.url(1));
  let net = format!("--allow-net=127.0.0.1:{}", c.nodes[1].addr().port());
  let cache =
    PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("deno-{}", std::process::id()));
  let logs = std::env::temp_dir().join(format!("relay-deno-{}", std::process::id()));
  std::fs::create_dir_all(&logs).unwrap();
  let (out_path, err_path) = (logs.join("out"), logs.join("err"));
  let mut child = std::process::Command::new(deno)
    .args([
      "test",
      "--no-config",
      "--no-remote",
      "--no-prompt",
      &net,
      // The nodes' self-signed test certificate.
      "--unsafely-ignore-certificate-errors=127.0.0.1",
      "web/tests/relay.test.ts",
      "--",
      &url,
      &token,
    ])
    .current_dir(manifest())
    .env("DENO_DIR", cache)
    .env("NO_COLOR", "1")
    .env("DENO_NO_UPDATE_CHECK", "1")
    .stdin(std::process::Stdio::null())
    .stdout(std::fs::File::create(&out_path).unwrap())
    .stderr(std::fs::File::create(&err_path).unwrap())
    .spawn()
    .expect("deno runs");
  // Poll on the runtime's clock, so the nodes keep serving while Deno runs.
  let started = std::time::Instant::now();
  let status = loop {
    if let Some(status) = child.try_wait().unwrap() {
      break Some(status);
    }
    if started.elapsed() > Duration::from_secs(90) {
      let _ = child.kill();
      let _ = child.wait();
      break None;
    }
    tokio::time::sleep(Duration::from_millis(50)).await;
  };
  let stdout = std::fs::read_to_string(&out_path).unwrap_or_default();
  let stderr = std::fs::read_to_string(&err_path).unwrap_or_default();
  assert!(
    status.is_some_and(|s| s.success()),
    "deno test ({}):\n--- stdout\n{stdout}\n--- stderr\n{stderr}",
    status.map_or_else(|| "killed after 90 s".to_owned(), |s| s.to_string())
  );
  assert!(stdout.contains("1 passed"), "{stdout}");
  let _ = std::fs::remove_dir_all(logs);
  c.shutdown().await;
}

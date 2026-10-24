//! `id-effect generate admin` on a generated `service` project, through the kit alone: the entity
//! derives through `id_effect_kit::sql`, the generated admin imports `id_effect_kit::admin` and
//! the project gains the kit's `admin` feature (no direct id_effect_admin dependency). The
//! project builds and passes its library tests against this checkout (`--local-workspace`),
//! offline, on the workspace's `Cargo.lock`, and generating again changes nothing. The projects
//! share a target directory with `tests/generate.rs` under `CARGO_TARGET_TMPDIR`.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn id_effect(args: &[&str]) -> Output {
  Command::new(env!("CARGO_BIN_EXE_id-effect"))
    .args(args)
    .output()
    .expect("spawn id-effect")
}

fn text(bytes: &[u8]) -> String {
  String::from_utf8_lossy(bytes).into_owned()
}

fn read(path: PathBuf) -> String {
  fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

/// The id_effect checkout this test runs in.
fn workspace() -> PathBuf {
  Path::new(env!("CARGO_MANIFEST_DIR"))
    .join("../..")
    .canonicalize()
    .unwrap()
}

/// A tenant-scoped entity written against the kit's re-exports only.
const DOCUMENTS: &str = r#"//! The service's documents.

use id_effect_kit::prelude::{Entity, FromRow};

/// A document of an organization.
#[derive(Clone, Debug, FromRow, Entity)]
#[from_row(crate = id_effect_kit::sql)]
#[entity(crate = id_effect_kit::sql, table = "documents", tenant = "org_id")]
pub struct Document {
    #[entity(id, generated)]
    pub id: i64,
    pub org_id: i64,
    #[entity(editable)]
    pub title: String,
    #[entity(editable)]
    pub status: String,
    pub reviewed: bool,
    #[entity(version)]
    pub version: i64,
}
"#;

/// The `[dependencies]` table of `manifest` (up to the next table).
fn dependencies(manifest: &str) -> &str {
  let start = manifest
    .find("[dependencies]")
    .expect("a [dependencies] table");
  let rest = &manifest[start..];
  let end = rest[1..].find("\n[").map_or(rest.len(), |i| i + 1);
  &rest[..end]
}

#[test]
fn template_service_generate_admin_round_trip() {
  let tmp = tempfile::tempdir().unwrap();
  let target = Path::new(env!("CARGO_TARGET_TMPDIR")).join("generate-projects");
  let lock = workspace().join("Cargo.lock");
  let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
  let ws = workspace();
  let dir = tmp.path().join("service");
  let out = id_effect(&[
    "new",
    "desk",
    "--template",
    "service",
    "--dest",
    dir.to_str().unwrap(),
    "--local-workspace",
    ws.to_str().unwrap(),
  ]);
  assert!(out.status.success(), "{out:?}");
  fs::write(dir.join("src/documents.rs"), DOCUMENTS).unwrap();
  let lib =
    read(dir.join("src/lib.rs")).replacen("pub mod rpc;", "pub mod documents;\npub mod rpc;", 1);
  fs::write(dir.join("src/lib.rs"), lib).unwrap();

  // Generate: the admin imports the kit's re-export and the kit gains feature `admin`.
  let dir_arg = dir.to_str().unwrap();
  let out = id_effect(&["generate", "admin", "Document", "--path", dir_arg]);
  assert!(out.status.success(), "{out:?}");
  let stdout = text(&out.stdout);
  assert!(stdout.contains("id_effect_kit feature `admin`"), "{stdout}");
  let module = read(dir.join("src/admin/document.rs"));
  assert!(module.contains("use id_effect_kit::admin::{"), "{module}");
  assert!(!module.contains("id_effect_admin::"), "{module}");
  assert!(
    module.contains("use id_effect_kit::sql::{Dialect, TestSqlClient};"),
    "{module}"
  );
  let manifest = read(dir.join("Cargo.toml"));
  let deps = dependencies(&manifest);
  let kit = deps
    .lines()
    .find(|l| l.starts_with("id_effect_kit = "))
    .expect("the kit dependency");
  assert!(kit.contains("\"admin\""), "{kit}");
  assert!(
    !deps.contains("id_effect_admin"),
    "no direct id_effect_admin dependency:\n{deps}"
  );
  assert!(
    manifest.contains("id_effect_admin = { path = "),
    "the --local-workspace patch of the crate the feature adds:\n{manifest}"
  );

  // Build and test the project's library (the admin's own tests among them).
  if lock.is_file() {
    fs::copy(&lock, dir.join("Cargo.lock")).unwrap();
  }
  // Not a member of a workspace the temporary directory may sit in.
  let toml = read(dir.join("Cargo.toml")) + "\n[workspace]\n";
  fs::write(dir.join("Cargo.toml"), &toml).unwrap();
  let out = Command::new(&cargo)
    .args(["test", "--lib"])
    .current_dir(&dir)
    .env("CARGO_TARGET_DIR", &target)
    .env("CARGO_NET_OFFLINE", "true")
    .output()
    .expect("spawn cargo");
  let (stdout, stderr) = (text(&out.stdout), text(&out.stderr));
  assert!(out.status.success(), "cargo test\n{stdout}\n{stderr}");
  assert!(
    !stderr.contains("was not used in the crate graph"),
    "an unused [patch] entry\n{stderr}"
  );
  for test in [
    "the_configuration_matches_the_entity",
    "nothing_is_allowed_until_granted",
    "only_the_offered_actions_can_be_granted",
    "every_page_is_behind_the_guard",
  ] {
    let name = format!("admin::document::tests::{test}");
    assert!(
      stdout.contains(&format!("test {name} ... ok")),
      "{name}\n{stdout}"
    );
  }
  let resolved = read(dir.join("Cargo.lock"));
  assert!(
    resolved.contains("name = \"id_effect_admin\""),
    "the kit brought it"
  );

  // Round trip: generating again changes nothing.
  let files = ["src/admin/document.rs", "src/admin/mod.rs", "src/lib.rs"];
  let before: Vec<String> = files.iter().map(|f| read(dir.join(f))).collect();
  let out = id_effect(&["generate", "admin", "Document", "--path", dir_arg]);
  assert!(out.status.success(), "{out:?}");
  assert!(
    text(&out.stdout).contains("up to date: nothing was written"),
    "{out:?}"
  );
  let after: Vec<String> = files.iter().map(|f| read(dir.join(f))).collect();
  assert_eq!(before, after);
  assert_eq!(read(dir.join("Cargo.toml")), toml);
}

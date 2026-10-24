//! `id-effect generate admin` end to end: the generated files against golden copies
//! (`tests/golden/generate/admin/`; `UPDATE_GOLDEN=1` rewrites them), a second run that changes
//! nothing, refusals that write nothing, and (the slow test) a `service` project `id-effect new`
//! generated, with entities added, whose generated admin builds and passes its tests against
//! this checkout (`--local-workspace`), offline, on the workspace's `Cargo.lock` like
//! `scripts/template-smoke.sh`. The projects share a target directory with
//! `tests/generate.rs` under `CARGO_TARGET_TMPDIR`.

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

/// `id-effect generate admin ARGS --path DIR`.
fn generate(dir: &Path, args: &[&str]) -> Output {
  let mut argv = vec!["generate", "admin"];
  argv.extend_from_slice(args);
  argv.extend_from_slice(&["--path", dir.to_str().unwrap()]);
  id_effect(&argv)
}

/// The entities the tests generate admins for: a tenant-scoped, soft-deleted, versioned
/// `Document` with a secret, an `Invoice` with a text key, and a struct that is no entity.
const DOCUMENTS: &str = r#"//! The shop's documents.

use id_effect_sql::jiff::Timestamp;
use id_effect_sql::{Entity, FromRow};

/// A document of an organization.
#[derive(Clone, Debug, FromRow, Entity)]
#[entity(table = "documents", tenant = "org_id", soft_delete = "deleted_at")]
pub struct Document {
    #[entity(id, generated)]
    pub id: i64,
    pub org_id: i64,
    #[entity(editable)]
    pub title: String,
    #[entity(editable)]
    pub status: String,
    #[entity(editable)]
    pub pages: i64,
    pub reviewed: bool,
    pub summary: Option<String>,
    /// Never on an admin page.
    pub api_token: String,
    pub deleted_at: Option<Timestamp>,
    #[entity(version)]
    pub version: i64,
}

/// An invoice, by its number.
#[derive(Clone, Debug, FromRow, Entity)]
#[entity(table = "invoices")]
pub struct Invoice {
    #[entity(id)]
    pub number: String,
    pub customer: String,
    pub amount_cents: i64,
    pub paid: bool,
}

/// Not an entity.
pub struct Plain {
    pub id: i64,
}
"#;

/// A minimal project with [`DOCUMENTS`] (the golden tests never build it).
fn project(dir: &Path) -> PathBuf {
  let root = dir.join("shop");
  fs::create_dir_all(root.join("src")).unwrap();
  fs::write(
    root.join("Cargo.toml"),
    "[package]\nname = \"shop\"\nversion = \"0.1.0\"\nedition = \"2024\"\n\n[dependencies]\nid_effect_kit = { version = \"0.13.0\", features = [\"web\"] }\nid_effect = \"0.13.0\"\nid_effect_sql = { version = \"0.13.0\", features = [\"repository\"] }\n",
  )
  .unwrap();
  fs::write(
    root.join("src/lib.rs"),
    "//! The shop.\n\npub mod documents;\n",
  )
  .unwrap();
  fs::write(root.join("src/documents.rs"), DOCUMENTS).unwrap();
  root
}

/// Compares `actual` with the golden file `name`, or rewrites it with `UPDATE_GOLDEN=1`.
fn golden(name: &str, actual: &str) {
  let path = Path::new(env!("CARGO_MANIFEST_DIR"))
    .join("tests/golden/generate/admin")
    .join(name);
  if std::env::var_os("UPDATE_GOLDEN").is_some() {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, actual).unwrap();
    return;
  }
  let want = read(path);
  assert_eq!(
    actual, want,
    "{name} differs from its golden copy (UPDATE_GOLDEN=1 rewrites it)"
  );
}

#[test]
fn generate_admin_golden_output() {
  let tmp = tempfile::tempdir().unwrap();
  let dir = project(tmp.path());
  let out = generate(&dir, &["Document"]);
  assert!(out.status.success(), "{out:?}");
  golden("document.stdout.golden", &text(&out.stdout));
  let out = generate(
    &dir,
    &[
      "Invoice",
      "--read-only",
      "--permissions",
      "admin.billing_invoice",
    ],
  );
  assert!(out.status.success(), "{out:?}");
  golden("invoice.stdout.golden", &text(&out.stdout));
  for (file, name) in [
    ("src/admin/document.rs", "document.rs.golden"),
    ("src/admin/invoice.rs", "invoice.rs.golden"),
    ("src/admin/mod.rs", "mod.rs.golden"),
    ("src/lib.rs", "lib.rs.golden"),
    ("Cargo.toml", "Cargo.toml.golden"),
  ] {
    golden(name, &read(dir.join(file)));
  }
}

#[test]
fn generate_admin_is_idempotent() {
  let tmp = tempfile::tempdir().unwrap();
  let dir = project(tmp.path());
  let files = [
    "src/admin/document.rs",
    "src/admin/mod.rs",
    "src/lib.rs",
    "Cargo.toml",
  ];
  assert!(generate(&dir, &["Document"]).status.success());
  let first: Vec<String> = files.iter().map(|f| read(dir.join(f))).collect();
  let out = generate(&dir, &["Document"]);
  assert!(out.status.success(), "{out:?}");
  assert!(
    text(&out.stdout).contains("up to date: nothing was written"),
    "{out:?}"
  );
  let again: Vec<String> = files.iter().map(|f| read(dir.join(f))).collect();
  assert_eq!(first, again);

  // A second entity joins; generating the first again still changes nothing.
  assert!(generate(&dir, &["Invoice"]).status.success());
  let both: Vec<String> = files.iter().map(|f| read(dir.join(f))).collect();
  assert!(generate(&dir, &["Document"]).status.success());
  let after: Vec<String> = files.iter().map(|f| read(dir.join(f))).collect();
  assert_eq!(both, after);
  let admin = &after[1];
  assert_eq!(admin.matches("pub mod document;").count(), 1, "{admin}");
  assert_eq!(
    admin.matches("document::register(site)?").count(),
    1,
    "{admin}"
  );
  assert_eq!(after[3].matches("id_effect_admin =").count(), 1);

  // An admin module edited by hand is never overwritten.
  let edited = after[0].replace(".search([", ".search([\"summary\", ");
  fs::write(dir.join(files[0]), &edited).unwrap();
  let out = generate(&dir, &["Document"]);
  assert_eq!(out.status.code(), Some(1), "{out:?}");
  assert!(
    text(&out.stderr).contains("already exists; nothing was written"),
    "{out:?}"
  );
  assert_eq!(read(dir.join(files[0])), edited);
}

#[test]
fn generate_admin_unknown_entity_is_error() {
  let tmp = tempfile::tempdir().unwrap();
  let dir = project(tmp.path());
  for (args, want) in [
    (
      &["Nope"][..],
      "unknown entity `Nope`: no `#[derive(Entity)]` struct of that name in the library (src/)",
    ),
    (
      &["Plain"],
      "unknown entity `Plain`: `struct Plain` in src/documents.rs does not `#[derive(Entity)]`",
    ),
    (
      &["document"],
      "invalid entity `document`: give the struct's type name",
    ),
    (
      &["Document", "--permissions", "shop.document"],
      "invalid --permissions `shop.document`",
    ),
    (
      &["Document", "--permissions", "admin.*"],
      "invalid --permissions `admin.*`",
    ),
  ] {
    let out = generate(&dir, args);
    assert_eq!(out.status.code(), Some(1), "{args:?}: {out:?}");
    let stderr = text(&out.stderr);
    assert!(
      stderr.starts_with("error: ") && stderr.contains(want),
      "{args:?}: {stderr}"
    );
  }
  assert!(!dir.join("src/admin").exists(), "nothing was written");
  assert_eq!(
    read(dir.join("src/lib.rs")),
    "//! The shop.\n\npub mod documents;\n"
  );

  let out = generate(tmp.path(), &["Document"]);
  assert_eq!(out.status.code(), Some(1), "{out:?}");
  assert!(text(&out.stderr).contains("is not an id_effect project (no Cargo.toml)"));
  let out = id_effect(&["generate", "admin"]);
  assert_eq!(out.status.code(), Some(2), "{out:?}");
  let out = id_effect(&["generate", "admin", "--help"]);
  let help = text(&out.stdout);
  assert!(
    out.status.success() && help.contains("--read-only") && help.contains("--permissions"),
    "{out:?}"
  );
}

/// The generated admins compile and pass their tests in a `service` project: the full one for
/// `Document` and the read-only one for `Invoice`. Slow on a cold target directory, quick
/// afterwards.
#[test]
fn generated_admin_compiles_in_service_template() {
  let tmp = tempfile::tempdir().unwrap();
  let target = Path::new(env!("CARGO_TARGET_TMPDIR")).join("generate-projects");
  let lock = workspace().join("Cargo.lock");
  let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
  let ws = workspace();
  let dir = tmp.path().join("service");
  let out = id_effect(&[
    "new",
    "demo",
    "--template",
    "service",
    "--dest",
    dir.to_str().unwrap(),
    "--local-workspace",
    ws.to_str().unwrap(),
  ]);
  assert!(out.status.success(), "{out:?}");

  // The entities, and the crate their derives expand to.
  fs::write(dir.join("src/documents.rs"), DOCUMENTS).unwrap();
  let lib =
    read(dir.join("src/lib.rs")).replacen("pub mod rpc;", "pub mod documents;\npub mod rpc;", 1);
  fs::write(dir.join("src/lib.rs"), lib).unwrap();
  let version = env!("CARGO_PKG_VERSION");
  let root = ws.to_string_lossy().replace('\\', "/");
  let manifest = read(dir.join("Cargo.toml"))
    .replacen(
      "\naxum = ",
      &format!(
        "\nid_effect_sql = {{ version = \"{version}\", features = [\"repository\"] }}\naxum = "
      ),
      1,
    )
    .replacen(
      "[patch.crates-io]\n",
      &format!("[patch.crates-io]\nid_effect_sql = {{ path = \"{root}/crates/id_effect_sql\" }}\n"),
      1,
    );
  fs::write(dir.join("Cargo.toml"), manifest).unwrap();

  for args in [
    &["Document"][..],
    &[
      "Invoice",
      "--read-only",
      "--permissions",
      "admin.billing_invoice",
    ],
  ] {
    let out = generate(&dir, args);
    assert!(out.status.success(), "{args:?}: {out:?}");
  }
  if lock.is_file() {
    fs::copy(&lock, dir.join("Cargo.lock")).unwrap();
  }
  // Not a member of a workspace the temporary directory may sit in.
  let toml = read(dir.join("Cargo.toml")) + "\n[workspace]\n";
  fs::write(dir.join("Cargo.toml"), toml).unwrap();
  let out = Command::new(&cargo)
    .args(["test", "--lib", "admin::"])
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
  for module in ["document", "invoice"] {
    for test in [
      "the_configuration_matches_the_entity",
      "nothing_is_allowed_until_granted",
      "only_the_offered_actions_can_be_granted",
      "every_page_is_behind_the_guard",
    ] {
      let name = format!("admin::{module}::tests::{test}");
      assert!(
        stdout.contains(&format!("test {name} ... ok")),
        "{name}\n{stdout}"
      );
    }
  }
}

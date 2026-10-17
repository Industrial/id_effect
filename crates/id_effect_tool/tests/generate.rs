//! `id-effect generate job` end to end: into projects `id-effect new` generated, and (the slow
//! test) a `cargo test` of a service and a worker with their generated jobs, against this
//! checkout (`--local-workspace`), offline, on the workspace's `Cargo.lock` like
//! `scripts/template-smoke.sh`. The projects share a target directory under
//! `CARGO_TARGET_TMPDIR`, so only the first run compiles their dependencies.

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

/// The id_effect checkout this test runs in.
fn workspace() -> PathBuf {
  Path::new(env!("CARGO_MANIFEST_DIR"))
    .join("../..")
    .canonicalize()
    .unwrap()
}

/// `id-effect new demo --template TEMPLATE` into `dir/TEMPLATE`, built against this checkout.
fn project(dir: &Path, template: &str) -> PathBuf {
  let dest = dir.join(template);
  let ws = workspace();
  let out = id_effect(&[
    "new",
    "demo",
    "--template",
    template,
    "--dest",
    dest.to_str().unwrap(),
    "--local-workspace",
    ws.to_str().unwrap(),
  ]);
  assert!(out.status.success(), "{out:?}");
  dest
}

fn generate(dir: &Path, args: &[&str]) -> Output {
  let mut argv = vec!["generate", "job"];
  argv.extend_from_slice(args);
  argv.extend_from_slice(&["--path", dir.to_str().unwrap()]);
  id_effect(&argv)
}

fn read(path: PathBuf) -> String {
  fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

#[test]
fn generate_job_writes_the_job_its_registration_and_the_dependency() {
  let tmp = tempfile::tempdir().unwrap();
  let dir = project(tmp.path(), "service");
  let out = generate(&dir, &["send_report", "--queue", "reports"]);
  assert!(out.status.success(), "{out:?}");
  let stdout = text(&out.stdout);
  assert!(
    stdout.starts_with("Generated job `send_report` (demo.send_report, queue `reports`):"),
    "{stdout}"
  );
  assert!(stdout.contains("cargo test jobs::send_report"), "{stdout}");

  let job = read(dir.join("src/jobs/send_report.rs"));
  for want in [
    "#[derive(Clone, Debug, PartialEq, Schema)]\npub struct SendReport {",
    "impl Job for SendReport {",
    "const NAME: &'static str = \"demo.send_report\";",
    "const QUEUE: &'static str = \"reports\";",
    "fn max_attempts(&self) -> u16 {",
    "pub fn handle(job: SendReport, ctx: JobContext) -> Effect<(), JobFailure, JobEnv> {",
    "MemoryJobQueue::new(clock)",
    "fn a_queued_job_runs_once_and_succeeds()",
  ] {
    assert!(job.contains(want), "no {want:?} in:\n{job}");
  }
  assert!(!job.contains("CRON"), "{job}");
  let jobs = read(dir.join("src/jobs/mod.rs"));
  assert!(jobs.contains("\npub mod send_report;\n"), "{jobs}");
  assert!(
    jobs.contains("    let registry = registry.register(send_report::handle)?;\n    Ok(registry)"),
    "{jobs}"
  );
  let lib = read(dir.join("src/lib.rs"));
  assert_eq!(lib.matches("pub mod jobs;").count(), 1, "{lib}");
  let cargo = read(dir.join("Cargo.toml"));
  let version = env!("CARGO_PKG_VERSION");
  assert!(
    cargo.contains(&format!("\nid_effect_jobs = \"{version}\"\n")),
    "{cargo}"
  );
  for name in ["id_effect_jobs", "id_effect_sql"] {
    assert_eq!(
      cargo.matches(&format!("\n{name} = {{ path = ")).count(),
      1,
      "{name}: {cargo}"
    );
  }

  // A second job joins the first: the module in order, the registration after it, nothing else.
  let out = generate(&dir, &["archive_orders"]);
  assert!(out.status.success(), "{out:?}");
  let jobs = read(dir.join("src/jobs/mod.rs"));
  assert!(
    jobs.contains("pub mod archive_orders;\npub mod send_report;\n"),
    "{jobs}"
  );
  assert!(
    jobs.contains(
      "registry.register(send_report::handle)?;\n    let registry = registry.register(archive_orders::handle)?;"
    ),
    "{jobs}"
  );
  assert_eq!(
    read(dir.join("Cargo.toml")),
    cargo,
    "the dependency is there"
  );
  assert_eq!(read(dir.join("src/lib.rs")), lib);
}

#[test]
fn cron_adds_the_entry_the_schedule_and_the_feature() {
  let tmp = tempfile::tempdir().unwrap();
  let dir = project(tmp.path(), "worker");
  let out = generate(&dir, &["nightly_digest", "--cron", "0 6 * * *"]);
  assert!(out.status.success(), "{out:?}");
  let job = read(dir.join("src/jobs/nightly_digest.rs"));
  assert!(
    job.contains("pub const CRON: &str = \"0 6 * * *\";"),
    "{job}"
  );
  assert!(
    job.contains("Ok(enqueue_on_tick(ENTRY, Cron::parse(CRON)?, payload))"),
    "{job}"
  );
  let jobs = read(dir.join("src/jobs/mod.rs"));
  assert!(
    jobs.contains("    let schedule = schedule.with(nightly_digest::cron()?)?;\n    Ok(schedule)"),
    "{jobs}"
  );
  let cargo = read(dir.join("Cargo.toml"));
  assert!(cargo.contains("features = [\"cron\"]"), "{cargo}");
  for name in ["id_effect_node", "id_effect_process"] {
    assert!(cargo.contains(&format!("\n{name} = {{ path = ")), "{cargo}");
  }
}

#[test]
fn an_existing_job_or_a_bad_argument_exits_1_and_writes_nothing() {
  let tmp = tempfile::tempdir().unwrap();
  let dir = project(tmp.path(), "service");
  assert!(generate(&dir, &["send_report"]).status.success());
  let before: Vec<String> = ["src/jobs/send_report.rs", "src/jobs/mod.rs", "Cargo.toml"]
    .iter()
    .map(|p| read(dir.join(p)))
    .collect();
  for (args, want) in [
    (&["send_report"][..], "already exists; nothing was written"),
    (&["Send-Report"], "invalid job name `Send-Report`"),
    (&["worker"], "its type name is one the job module imports"),
    (&["ok", "--queue", "a b"], "invalid queue `a b`"),
    (&["ok", "--cron", "daily"], "invalid --cron `daily`"),
  ] {
    let out = generate(&dir, args);
    assert_eq!(out.status.code(), Some(1), "{args:?}: {out:?}");
    let stderr = text(&out.stderr);
    assert!(
      stderr.starts_with("error: ") && stderr.contains(want),
      "{args:?}: {stderr}"
    );
  }
  let after: Vec<String> = ["src/jobs/send_report.rs", "src/jobs/mod.rs", "Cargo.toml"]
    .iter()
    .map(|p| read(dir.join(p)))
    .collect();
  assert_eq!(before, after);
  assert!(!dir.join("src/jobs/ok.rs").exists());

  // A module of the job's name declared by hand.
  let jobs = read(dir.join("src/jobs/mod.rs"))
    .replace("pub mod send_report;", "pub mod send_report;\nmod mine;");
  fs::write(dir.join("src/jobs/mod.rs"), jobs).unwrap();
  let out = generate(&dir, &["mine"]);
  assert_eq!(out.status.code(), Some(1), "{out:?}");
  assert!(
    text(&out.stderr).contains("already declares `mod mine`"),
    "{out:?}"
  );
}

#[test]
fn outside_a_project_exits_1_and_without_a_name_2() {
  let tmp = tempfile::tempdir().unwrap();
  let out = generate(tmp.path(), &["send_report"]);
  assert_eq!(out.status.code(), Some(1), "{out:?}");
  assert!(text(&out.stderr).contains("is not an id_effect project (no Cargo.toml)"));
  let out = id_effect(&["generate", "job"]);
  assert_eq!(out.status.code(), Some(2), "{out:?}");
  let out = id_effect(&["generate", "--help"]);
  assert!(
    out.status.success() && text(&out.stdout).contains("job"),
    "{out:?}"
  );
}

#[test]
fn a_registry_it_cannot_edit_is_printed_under_do_by_hand() {
  let tmp = tempfile::tempdir().unwrap();
  let dir = project(tmp.path(), "worker");
  assert!(generate(&dir, &["first"]).status.success());
  let path = dir.join("src/jobs/mod.rs");
  let jobs = read(path.clone()).replace("    Ok(registry)\n", "    Ok(registry.clone())\n");
  fs::write(&path, &jobs).unwrap();
  let out = generate(&dir, &["second"]);
  assert!(out.status.success(), "{out:?}");
  let stdout = text(&out.stdout);
  assert!(
    stdout.contains(
      "Do by hand:\n  src/jobs/mod.rs: add `let registry = registry.register(second::handle)?;`"
    ),
    "{stdout}"
  );
  assert!(dir.join("src/jobs/second.rs").is_file());
  assert!(read(path).contains("pub mod second;"));
}

/// The generated jobs compile, pass clippy's default lints and their tests, in a service and in
/// a worker (with `--cron`). Slow on a cold target directory (it builds the projects'
/// dependencies), quick afterwards.
#[test]
fn generated_jobs_build_and_pass_their_tests() {
  let tmp = tempfile::tempdir().unwrap();
  let target = Path::new(env!("CARGO_TARGET_TMPDIR")).join("generate-projects");
  let lock = workspace().join("Cargo.lock");
  let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
  for (template, extra) in [
    ("service", &[][..]),
    ("worker", &["--cron", "0 6 * * *"][..]),
  ] {
    let dir = project(tmp.path(), template);
    let mut args = vec!["send_report"];
    args.extend_from_slice(extra);
    let out = generate(&dir, &args);
    assert!(out.status.success(), "{template}: {out:?}");
    if lock.is_file() {
      fs::copy(&lock, dir.join("Cargo.lock")).unwrap();
    }
    // Not a member of a workspace the temporary directory may sit in.
    let manifest = dir.join("Cargo.toml");
    let toml = read(manifest.clone()) + "\n[workspace]\n";
    fs::write(&manifest, toml).unwrap();
    let out = Command::new(&cargo)
      .args(["test", "--lib", "jobs::"])
      .current_dir(&dir)
      .env("CARGO_TARGET_DIR", &target)
      .env("CARGO_NET_OFFLINE", "true")
      .output()
      .expect("spawn cargo");
    let (stdout, stderr) = (text(&out.stdout), text(&out.stderr));
    assert!(
      out.status.success(),
      "{template}: cargo test\n{stdout}\n{stderr}"
    );
    assert!(
      !stderr.contains("was not used in the crate graph"),
      "{template}: an unused [patch] entry\n{stderr}"
    );
    let mut tests = vec!["jobs::send_report::tests::a_queued_job_runs_once_and_succeeds"];
    if template == "worker" {
      tests.push("jobs::send_report::tests::the_cron_entry_is_scheduled");
    }
    for test in tests {
      assert!(
        stdout.contains(&format!("test {test} ... ok")),
        "{template}: {test}\n{stdout}"
      );
    }
  }
}

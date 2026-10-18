//! `id-effect dev`: build the project, run its binary with `ID_EFFECT_PROFILE=dev`, and rebuild
//! and restart it whenever a watched file changes.
//!
//! ```text
//! id-effect dev [--path DIR] [--package NAME] [--bin NAME] [--watch PATH]... [--services]
//!               [--poll-ms MS] [--debounce-ms MS] [--grace-secs SECS] [-- APP ARGS...]
//! ```
//!
//! The watcher polls modification times (no `notify`, release 0.10 D4) of `src/`, `Cargo.toml`,
//! `config/` and every `--watch PATH` under DIR, every `--poll-ms`; it skips `target`, hidden
//! entries (`.git`, `.idea`, …) and editor backups (`*~`, `*.swp`, `*.swx`). Once the files have
//! been quiet for `--debounce-ms` after a change, it runs `cargo build` (its output streams to
//! the terminal). Only when the build succeeds is the running app stopped — SIGTERM, up to
//! `--grace-secs` to drain and exit, then killed (on Windows it is killed at once) — and the
//! new binary started. A failed build keeps the running app and waits for the next change.
//! `--services` first runs `docker compose up -d` when DIR has a compose file.
//!
//! The app's stdin, stdout and stderr are the terminal's. Ctrl-C stops `id-effect dev`, and the
//! app with it (both are in the terminal's process group). It exits `1` only when it cannot
//! start: no `Cargo.toml` in DIR, `cargo` not found, `docker compose up -d` failed.

use std::collections::{BTreeMap, BTreeSet};
use std::convert::Infallible;
use std::ffi::OsStr;
use std::fs;
use std::io::{self, BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitCode, ExitStatus, Stdio};
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use clap::Args;
use serde_json::Value;

/// The environment variable the app's profile is read from (`id_effect_config::profile`).
pub const PROFILE_ENV_VAR: &str = "ID_EFFECT_PROFILE";
/// The profile the app runs with.
pub const PROFILE: &str = "dev";
/// What is always watched, relative to the project directory.
pub const DEFAULT_WATCH: [&str; 3] = ["src", "Cargo.toml", "config"];
/// The compose files `--services` looks for, in this order (`docker compose`'s own order).
pub const COMPOSE_FILES: [&str; 4] = [
  "compose.yaml",
  "compose.yml",
  "docker-compose.yaml",
  "docker-compose.yml",
];

/// Arguments of `id-effect dev`.
#[derive(Debug, Args)]
pub struct DevArgs {
  /// The project directory.
  #[arg(long, value_name = "DIR", default_value = ".")]
  pub path: PathBuf,
  /// The package to build and run (`cargo build --package`).
  #[arg(short, long, value_name = "NAME")]
  pub package: Option<String>,
  /// The binary to run (`cargo build --bin`), needed when the build makes more than one.
  #[arg(long, value_name = "NAME")]
  pub bin: Option<String>,
  /// Also watch PATH, a file or directory relative to DIR (repeatable); src/, Cargo.toml and
  /// config/ are always watched.
  #[arg(long = "watch", value_name = "PATH")]
  pub watch: Vec<PathBuf>,
  /// Milliseconds between two scans of the watched files.
  #[arg(long, value_name = "MS", default_value_t = 500, value_parser = clap::value_parser!(u64).range(1..))]
  pub poll_ms: u64,
  /// Rebuild once the files have been quiet for this many milliseconds after a change.
  #[arg(long, value_name = "MS", default_value_t = 200)]
  pub debounce_ms: u64,
  /// Seconds the app may take to exit after SIGTERM before it is killed.
  #[arg(long, value_name = "SECS", default_value_t = 10)]
  pub grace_secs: u64,
  /// First run `docker compose up -d` in DIR (when it has a compose file).
  #[arg(long)]
  pub services: bool,
  /// Arguments passed to the app (after `--`).
  #[arg(last = true, value_name = "APP ARGS")]
  pub app_args: Vec<String>,
}

/// Runs `id-effect dev` until it is interrupted; returns (exit `1`, the reason on `err`) only
/// when it cannot start. Its own messages go to `out`, prefixed `id-effect dev: `.
pub fn run(args: &DevArgs, out: &mut dyn Write, err: &mut dyn Write) -> ExitCode {
  match dev(args, out) {
    Ok(never) => match never {},
    Err(reason) => {
      let _ = writeln!(err, "error: {reason}");
      ExitCode::FAILURE
    }
  }
}

fn dev(args: &DevArgs, out: &mut dyn Write) -> Result<Infallible, String> {
  let dir = &args.path;
  if !dir.join("Cargo.toml").is_file() {
    return Err(format!("no Cargo.toml in {}", dir.display()));
  }
  if args.services {
    services(dir, out)?;
  }
  let roots = roots(dir, &args.watch);
  let mut watcher = Watcher::new(roots.clone(), Duration::from_millis(args.debounce_ms));
  let watched: Vec<String> = roots.iter().map(|r| relative(dir, r)).collect();
  say(
    out,
    &format!("watching {} every {}ms", watched.join(", "), args.poll_ms),
  );
  let grace = Duration::from_secs(args.grace_secs);
  let mut app = None;
  rebuild(args, out, &mut app, grace)?;
  loop {
    thread::sleep(Duration::from_millis(args.poll_ms));
    if let Some(child) = &mut app
      && let Ok(Some(status)) = child.try_wait()
    {
      say(
        out,
        &format!(
          "the app (pid {}) exited ({status}); waiting for a change",
          child.id()
        ),
      );
      app = None;
    }
    if let Some(changed) = watcher.poll(Instant::now()) {
      let names: Vec<String> = changed.iter().map(|p| relative(dir, p)).collect();
      say(out, &format!("changed {}; rebuilding", names.join(", ")));
      rebuild(args, out, &mut app, grace)?;
    }
  }
}

fn say(out: &mut dyn Write, message: &str) {
  let _ = writeln!(out, "id-effect dev: {message}");
  let _ = out.flush();
}

/// `path` relative to `dir` for messages.
fn relative(dir: &Path, path: &Path) -> String {
  path.strip_prefix(dir).unwrap_or(path).display().to_string()
}

/// The watched paths: [`DEFAULT_WATCH`] then `extra`, under `dir`, without repeats.
fn roots(dir: &Path, extra: &[PathBuf]) -> Vec<PathBuf> {
  let mut roots: Vec<PathBuf> = Vec::new();
  let all = DEFAULT_WATCH
    .iter()
    .map(PathBuf::from)
    .chain(extra.iter().cloned());
  for path in all {
    let path = dir.join(path);
    if !roots.contains(&path) {
      roots.push(path);
    }
  }
  roots
}

/// `docker compose up -d` in `dir` when it has a compose file.
fn services(dir: &Path, out: &mut dyn Write) -> Result<(), String> {
  let Some(file) = compose_file(dir) else {
    say(
      out,
      &format!(
        "--services: no compose file ({}) in {}; skipped",
        COMPOSE_FILES.join(", "),
        dir.display()
      ),
    );
    return Ok(());
  };
  say(
    out,
    &format!("starting services ({file}): docker compose up -d"),
  );
  let status = Command::new("docker")
    .args(["compose", "up", "-d"])
    .current_dir(dir)
    .stdin(Stdio::null())
    .status()
    .map_err(|e| format!("cannot run `docker compose up -d`: {e}"))?;
  if status.success() {
    Ok(())
  } else {
    Err(format!("`docker compose up -d` failed ({status})"))
  }
}

/// The first of [`COMPOSE_FILES`] in `dir`.
fn compose_file(dir: &Path) -> Option<&'static str> {
  COMPOSE_FILES
    .into_iter()
    .find(|name| dir.join(name).is_file())
}

/// Builds; when that succeeds, stops the running app and starts the new binary. A failed build
/// leaves `app` alone. `Err` only when `cargo` cannot be run.
fn rebuild(
  args: &DevArgs,
  out: &mut dyn Write,
  app: &mut Option<Child>,
  grace: Duration,
) -> Result<(), String> {
  say(out, "cargo build");
  let exe = match build(args)? {
    Ok(exe) => exe,
    Err(reason) => {
      let keep = match app {
        Some(child) => format!("keeping the running app (pid {})", child.id()),
        None => "no app running".to_string(),
      };
      say(
        out,
        &format!("build failed ({reason}); {keep}; waiting for a change"),
      );
      return Ok(());
    }
  };
  if let Some(mut old) = app.take() {
    let pid = old.id();
    match stop(&mut old, grace) {
      Ok(Stop::Exited(status)) => say(out, &format!("stopped pid {pid} ({status})")),
      Ok(Stop::Killed) => say(
        out,
        &format!(
          "pid {pid} did not exit within {}s of SIGTERM; killed",
          grace.as_secs()
        ),
      ),
      Err(e) => say(out, &format!("stopping pid {pid}: {e}")),
    }
  }
  match start(&args.path, &exe, &args.app_args) {
    Ok(child) => {
      say(
        out,
        &format!(
          "started {} (pid {}, {PROFILE_ENV_VAR}={PROFILE})",
          relative(&args.path, &exe),
          child.id()
        ),
      );
      *app = Some(child);
    }
    Err(e) => say(
      out,
      &format!("cannot start {}: {e}; waiting for a change", exe.display()),
    ),
  }
  Ok(())
}

/// `cargo build` in the project, its diagnostics on the terminal: `Ok(Ok(binary))`, or
/// `Ok(Err(why))` when the build failed or made no single binary; `Err` when `cargo` does not run.
fn build(args: &DevArgs) -> Result<Result<PathBuf, String>, String> {
  let mut cargo = Command::new("cargo");
  cargo
    .args(["build", "--message-format=json-render-diagnostics"])
    .current_dir(&args.path)
    .stdin(Stdio::null())
    .stdout(Stdio::piped())
    .stderr(Stdio::inherit());
  if let Some(package) = &args.package {
    cargo.args(["--package", package]);
  }
  if let Some(bin) = &args.bin {
    cargo.args(["--bin", bin]);
  }
  let mut child = cargo.spawn().map_err(|e| match e.kind() {
    io::ErrorKind::NotFound => "`cargo` not found on PATH".to_string(),
    _ => format!("cannot run `cargo`: {e}"),
  })?;
  let mut bins = Vec::new();
  if let Some(stdout) = child.stdout.take() {
    for line in BufReader::new(stdout).lines() {
      let Ok(line) = line else { break };
      bins.extend(binary(&line));
    }
  }
  let status = child
    .wait()
    .map_err(|e| format!("waiting for `cargo build`: {e}"))?;
  if !status.success() {
    return Ok(Err(format!("`cargo build` {status}")));
  }
  Ok(pick(bins, args.bin.as_deref()))
}

/// The `(target name, executable)` of a `compiler-artifact` message for a `bin` target.
fn binary(line: &str) -> Option<(String, PathBuf)> {
  let message: Value = serde_json::from_str(line).ok()?;
  if message["reason"] != "compiler-artifact" {
    return None;
  }
  let target = &message["target"];
  let is_bin = target["kind"]
    .as_array()
    .is_some_and(|kinds| kinds.iter().any(|k| k == "bin"));
  if !is_bin {
    return None;
  }
  let exe = message["executable"].as_str()?;
  Some((target["name"].as_str()?.to_string(), PathBuf::from(exe)))
}

/// The binary to run: the only one, or `--bin`'s.
fn pick(bins: Vec<(String, PathBuf)>, bin: Option<&str>) -> Result<PathBuf, String> {
  if let Some(name) = bin {
    return bins
      .into_iter()
      .find(|(n, _)| n == name)
      .map(|(_, exe)| exe)
      .ok_or_else(|| format!("the build made no binary `{name}`"));
  }
  let names: Vec<&str> = bins.iter().map(|(n, _)| n.as_str()).collect();
  match bins.len() {
    0 => Err("the build made no binary; pick a package with --package".into()),
    1 => Ok(
      bins
        .into_iter()
        .next()
        .map(|(_, exe)| exe)
        .unwrap_or_default(),
    ),
    _ => Err(format!(
      "the build made {} binaries ({}); pick one with --bin",
      names.len(),
      names.join(", ")
    )),
  }
}

/// Starts `exe app_args` in `dir` with [`PROFILE_ENV_VAR`]`=`[`PROFILE`], on the terminal.
fn start(dir: &Path, exe: &Path, app_args: &[String]) -> io::Result<Child> {
  Command::new(exe)
    .args(app_args)
    .current_dir(dir)
    .env(PROFILE_ENV_VAR, PROFILE)
    .spawn()
}

/// How [`stop`] ended the app.
#[derive(Debug)]
enum Stop {
  /// It exited (after SIGTERM, or on its own before).
  Exited(ExitStatus),
  /// It was still running after the grace period, or could not be sent SIGTERM, and was killed.
  Killed,
}

/// Sends SIGTERM, waits up to `grace` for the app to drain and exit, then kills it.
fn stop(child: &mut Child, grace: Duration) -> io::Result<Stop> {
  if let Some(status) = child.try_wait()? {
    return Ok(Stop::Exited(status));
  }
  if terminate(child) {
    let deadline = Instant::now() + grace;
    loop {
      if let Some(status) = child.try_wait()? {
        return Ok(Stop::Exited(status));
      }
      if Instant::now() >= deadline {
        break;
      }
      thread::sleep(Duration::from_millis(20));
    }
  }
  child.kill()?;
  child.wait()?;
  Ok(Stop::Killed)
}

/// SIGTERM through `kill(1)`: the crate forbids `unsafe` and depends on no signal crate.
#[cfg(unix)]
fn terminate(child: &Child) -> bool {
  Command::new("kill")
    .args(["-TERM", &child.id().to_string()])
    .stdin(Stdio::null())
    .stdout(Stdio::null())
    .stderr(Stdio::null())
    .status()
    .is_ok_and(|s| s.success())
}

/// No SIGTERM on Windows: [`stop`] kills the app at once.
#[cfg(not(unix))]
fn terminate(_child: &Child) -> bool {
  false
}

/// A file's modification time and length: an edit within the filesystem's mtime resolution
/// usually still changes the length.
type Stamp = (Option<SystemTime>, u64);

/// Every watched file and its [`Stamp`].
type Snapshot = BTreeMap<PathBuf, Stamp>;

/// Entries the watcher skips: `target`, hidden names and editor backups.
fn ignored(name: &OsStr) -> bool {
  let Some(name) = name.to_str() else {
    return false;
  };
  name == "target"
    || name.starts_with('.')
    || name.ends_with('~')
    || name.ends_with(".swp")
    || name.ends_with(".swx")
}

/// The files under `roots` (a root may be a file, a directory or missing). Symlinked files are
/// followed, symlinked directories are not.
fn scan(roots: &[PathBuf]) -> Snapshot {
  let mut snapshot = Snapshot::new();
  for root in roots {
    visit(root, &mut snapshot);
  }
  snapshot
}

fn visit(path: &Path, snapshot: &mut Snapshot) {
  let Ok(meta) = fs::metadata(path) else {
    return;
  };
  if meta.is_file() {
    snapshot.insert(path.to_path_buf(), (meta.modified().ok(), meta.len()));
    return;
  }
  if !meta.is_dir() {
    return;
  }
  let Ok(entries) = fs::read_dir(path) else {
    return;
  };
  for entry in entries.flatten() {
    if ignored(&entry.file_name()) {
      continue;
    }
    let path = entry.path();
    match entry.file_type() {
      Ok(kind) if kind.is_symlink() => {
        if fs::metadata(&path).is_ok_and(|m| m.is_file()) {
          visit(&path, snapshot);
        }
      }
      Ok(_) => visit(&path, snapshot),
      Err(_) => {}
    }
  }
}

/// The files added, changed or removed from `old` to `new`.
fn diff(old: &Snapshot, new: &Snapshot) -> BTreeSet<PathBuf> {
  let changed = new
    .iter()
    .filter(|(path, stamp)| old.get(*path) != Some(*stamp))
    .map(|(path, _)| path.clone());
  let removed = old.keys().filter(|path| !new.contains_key(*path)).cloned();
  changed.chain(removed).collect()
}

/// Collects changes until a scan finds none `quiet` after the last one, so a save that touches
/// several files (or a formatter run) rebuilds once.
#[derive(Debug)]
struct Debounce {
  quiet: Duration,
  pending: BTreeSet<PathBuf>,
  last: Option<Instant>,
}

impl Debounce {
  fn new(quiet: Duration) -> Self {
    Self {
      quiet,
      pending: BTreeSet::new(),
      last: None,
    }
  }

  /// Records one scan's `changed` files at `now`; returns everything changed since the last
  /// rebuild once a scan with no change comes `quiet` after the last change.
  fn observe(&mut self, changed: BTreeSet<PathBuf>, now: Instant) -> Option<BTreeSet<PathBuf>> {
    if !changed.is_empty() {
      self.pending.extend(changed);
      self.last = Some(now);
      return None;
    }
    let last = self.last?;
    if now.duration_since(last) < self.quiet {
      return None;
    }
    self.last = None;
    Some(std::mem::take(&mut self.pending))
  }
}

/// Polls the watched files: [`scan`], [`diff`] against the previous scan, [`Debounce`].
#[derive(Debug)]
struct Watcher {
  roots: Vec<PathBuf>,
  last: Snapshot,
  debounce: Debounce,
}

impl Watcher {
  /// Scans `roots` once, as the state later scans compare to.
  fn new(roots: Vec<PathBuf>, quiet: Duration) -> Self {
    let last = scan(&roots);
    Self {
      roots,
      last,
      debounce: Debounce::new(quiet),
    }
  }

  /// Scans again; the changed files once they have settled.
  fn poll(&mut self, now: Instant) -> Option<BTreeSet<PathBuf>> {
    let next = scan(&self.roots);
    let changed = diff(&self.last, &next);
    self.last = next;
    self.debounce.observe(changed, now)
  }
}

#[cfg(test)]
mod tests {
  use std::fs::File;

  use clap::Parser;

  use super::*;
  use crate::cli::{Cli, Command as CliCommand};

  fn write(path: &Path, text: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
  }

  fn set_mtime(path: &Path, secs: u64) {
    let time = SystemTime::UNIX_EPOCH + Duration::from_secs(secs);
    File::options()
      .write(true)
      .open(path)
      .unwrap()
      .set_modified(time)
      .unwrap();
  }

  fn set(paths: &[&Path]) -> BTreeSet<PathBuf> {
    paths.iter().map(|p| p.to_path_buf()).collect()
  }

  mod watcher {
    use super::*;

    #[test]
    fn diff_reports_changed_added_and_removed_files() {
      let tmp = tempfile::tempdir().unwrap();
      let root = tmp.path().to_path_buf();
      let (main, lib, new) = (
        root.join("main.rs"),
        root.join("lib.rs"),
        root.join("new.rs"),
      );
      write(&main, "fn main() {}");
      write(&lib, "");
      set_mtime(&main, 1_000);
      let before = scan(std::slice::from_ref(&root));
      assert_eq!(before.len(), 2);
      assert!(diff(&before, &scan(std::slice::from_ref(&root))).is_empty());
      // Same length, newer mtime.
      set_mtime(&main, 2_000);
      fs::remove_file(&lib).unwrap();
      write(&new, "");
      let after = scan(std::slice::from_ref(&root));
      assert_eq!(diff(&before, &after), set(&[&main, &lib, &new]));
    }

    #[test]
    fn a_same_mtime_edit_is_seen_by_its_length() {
      let tmp = tempfile::tempdir().unwrap();
      let file = tmp.path().join("a.rs");
      write(&file, "v1");
      set_mtime(&file, 1_000);
      let before = scan(std::slice::from_ref(&file));
      write(&file, "v1 edited");
      set_mtime(&file, 1_000);
      assert_eq!(
        diff(&before, &scan(std::slice::from_ref(&file))),
        set(&[&file])
      );
    }

    #[test]
    fn target_hidden_and_backup_entries_are_ignored() {
      let tmp = tempfile::tempdir().unwrap();
      let root = tmp.path();
      let kept = root.join("src/main.rs");
      write(&kept, "");
      for skipped in [
        "target/debug/app",
        "src/target/x.rs",
        ".git/HEAD",
        "src/.main.rs.swp",
        "src/main.rs~",
        "src/x.swx",
      ] {
        write(&root.join(skipped), "");
      }
      let found: Vec<PathBuf> = scan(&[root.to_path_buf()]).into_keys().collect();
      assert_eq!(found, vec![kept]);
      assert!(ignored(OsStr::new("target")) && !ignored(OsStr::new("targets.rs")));
    }

    #[test]
    fn missing_roots_are_empty_until_created() {
      let tmp = tempfile::tempdir().unwrap();
      let config = tmp.path().join("config");
      let mut watcher = Watcher::new(vec![config.clone()], Duration::ZERO);
      let t0 = Instant::now();
      assert_eq!(watcher.poll(t0), None);
      write(&config.join("base.toml"), "a = 1");
      assert_eq!(watcher.poll(t0), None, "a change waits for a quiet scan");
      assert_eq!(watcher.poll(t0), Some(set(&[&config.join("base.toml")])));
      assert_eq!(watcher.poll(t0), None, "reported once");
    }

    #[test]
    fn debounce_waits_for_quiet_and_merges_changes() {
      let quiet = Duration::from_millis(200);
      let mut debounce = Debounce::new(quiet);
      let t0 = Instant::now();
      let ms = |n| t0 + Duration::from_millis(n);
      let (a, b) = (Path::new("a.rs"), Path::new("b.rs"));
      assert_eq!(
        debounce.observe(BTreeSet::new(), t0),
        None,
        "nothing changed"
      );
      assert_eq!(debounce.observe(set(&[a]), t0), None);
      assert_eq!(
        debounce.observe(BTreeSet::new(), ms(100)),
        None,
        "not quiet yet"
      );
      assert_eq!(
        debounce.observe(set(&[b]), ms(150)),
        None,
        "restarts the wait"
      );
      assert_eq!(debounce.observe(BTreeSet::new(), ms(300)), None);
      assert_eq!(
        debounce.observe(BTreeSet::new(), ms(350)),
        Some(set(&[a, b]))
      );
      assert_eq!(debounce.observe(BTreeSet::new(), ms(1_000)), None);
    }

    #[test]
    fn roots_are_the_defaults_then_extra_paths_without_repeats() {
      let dir = Path::new("/p");
      let extra = [PathBuf::from("templates"), PathBuf::from("src")];
      let want: Vec<PathBuf> = ["src", "Cargo.toml", "config", "templates"]
        .iter()
        .map(|p| dir.join(p))
        .collect();
      assert_eq!(roots(dir, &extra), want);
    }
  }

  mod build {
    use super::*;

    const BIN: &str = r#"{"reason":"compiler-artifact","target":{"kind":["bin"],"name":"app"},"executable":"/t/debug/app","fresh":true}"#;
    const LIB: &str =
      r#"{"reason":"compiler-artifact","target":{"kind":["lib"],"name":"app"},"executable":null}"#;

    #[test]
    fn binaries_come_from_bin_artifacts_only() {
      assert_eq!(
        binary(BIN),
        Some(("app".to_string(), PathBuf::from("/t/debug/app")))
      );
      assert_eq!(binary(LIB), None);
      assert_eq!(
        binary(r#"{"reason":"build-finished","success":true}"#),
        None
      );
      assert_eq!(binary("not json"), None);
    }

    #[test]
    fn pick_takes_the_only_binary_or_the_named_one() {
      let bin = |n: &str| (n.to_string(), PathBuf::from(format!("/t/{n}")));
      assert_eq!(pick(vec![bin("a")], None), Ok(PathBuf::from("/t/a")));
      assert_eq!(
        pick(vec![bin("a"), bin("b")], Some("b")),
        Ok(PathBuf::from("/t/b"))
      );
      let many = pick(vec![bin("a"), bin("b")], None).unwrap_err();
      assert_eq!(
        many,
        "the build made 2 binaries (a, b); pick one with --bin"
      );
      assert!(pick(vec![], None).unwrap_err().contains("no binary"));
      assert!(pick(vec![bin("a")], Some("c")).unwrap_err().contains("`c`"));
    }

    #[test]
    fn compose_files_are_found_in_docker_order() {
      let tmp = tempfile::tempdir().unwrap();
      assert_eq!(compose_file(tmp.path()), None);
      write(&tmp.path().join("docker-compose.yml"), "");
      assert_eq!(compose_file(tmp.path()), Some("docker-compose.yml"));
      write(&tmp.path().join("compose.yaml"), "");
      assert_eq!(compose_file(tmp.path()), Some("compose.yaml"));
    }
  }

  #[cfg(unix)]
  mod stop {
    use super::*;

    #[test]
    fn sigterm_stops_an_app_that_honours_it() {
      let mut child = Command::new("sleep").arg("30").spawn().unwrap();
      let started = Instant::now();
      match stop(&mut child, Duration::from_secs(10)).unwrap() {
        Stop::Exited(status) => assert!(!status.success(), "{status}"),
        Stop::Killed => panic!("sleep ignored SIGTERM"),
      }
      assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn an_app_ignoring_sigterm_is_killed_after_the_grace_period() {
      // `trap "" TERM` survives `exec`: sleep itself ignores SIGTERM.
      let mut child = Command::new("sh")
        .args(["-c", "trap '' TERM; exec sleep 30"])
        .spawn()
        .unwrap();
      thread::sleep(Duration::from_millis(200));
      let started = Instant::now();
      let stopped = stop(&mut child, Duration::from_millis(300)).unwrap();
      assert!(matches!(stopped, Stop::Killed), "{stopped:?}");
      assert!(started.elapsed() >= Duration::from_millis(300));
    }
  }

  #[test]
  fn dev_reads_every_option_and_app_args() {
    let argv = [
      "id-effect",
      "dev",
      "--path",
      "/p",
      "-p",
      "api",
      "--bin",
      "server",
      "--watch",
      "templates",
      "--watch",
      "migrations",
      "--poll-ms",
      "50",
      "--debounce-ms",
      "0",
      "--grace-secs",
      "3",
      "--services",
      "--",
      "--port",
      "9000",
    ];
    let CliCommand::Dev(args) = Cli::try_parse_from(argv).unwrap().command else {
      panic!("not `dev`");
    };
    assert_eq!(args.path, PathBuf::from("/p"));
    assert_eq!(args.package.as_deref(), Some("api"));
    assert_eq!(args.bin.as_deref(), Some("server"));
    assert_eq!(
      args.watch,
      vec![PathBuf::from("templates"), PathBuf::from("migrations")]
    );
    assert_eq!(
      (args.poll_ms, args.debounce_ms, args.grace_secs),
      (50, 0, 3)
    );
    assert!(args.services);
    assert_eq!(args.app_args, ["--port", "9000"]);
  }

  #[test]
  fn dev_defaults_and_rejects_a_zero_poll_interval() {
    let CliCommand::Dev(args) = Cli::try_parse_from(["id-effect", "dev"]).unwrap().command else {
      panic!("not `dev`");
    };
    assert_eq!(args.path, PathBuf::from("."));
    assert_eq!(
      (args.poll_ms, args.debounce_ms, args.grace_secs),
      (500, 200, 10)
    );
    assert!(!args.services && args.watch.is_empty() && args.app_args.is_empty());
    assert!(Cli::try_parse_from(["id-effect", "dev", "--poll-ms", "0"]).is_err());
  }

  #[test]
  fn dev_without_a_project_exits_1() {
    let tmp = tempfile::tempdir().unwrap();
    let argv = ["id-effect", "dev", "--path", tmp.path().to_str().unwrap()];
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let code = crate::cli::run(argv, &mut out, &mut err);
    assert_eq!(code, ExitCode::FAILURE);
    let err = String::from_utf8(err).unwrap();
    assert!(err.starts_with("error: no Cargo.toml in "), "{err}");
  }
}

//! `id-effect dev` end to end on a tiny std-only project written to a temp directory: an edit
//! rebuilds and restarts the app, a broken edit keeps the running one.
//!
//! Slow-ish: each test runs `cargo build` on the fixture three times (about a second each on
//! an idle machine, no network: `CARGO_NET_OFFLINE`). The waits are generous (`TIMEOUT`) so a
//! busy machine does not flake them. Unix only: the tests check processes with `kill(1)`.
#![cfg(unix)]

use std::io::{BufRead, BufReader, Read};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{Receiver, channel};
use std::thread;
use std::time::{Duration, Instant};

/// How long one expected line may take (it may sit behind a `cargo build`).
const TIMEOUT: Duration = Duration::from_secs(180);

const CARGO_TOML: &str = "[package]\nname = \"devapp\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n# Not a member of the workspace the temp directory may sit in.\n[workspace]\n";

/// The fixture app: prints its version, profile, arguments and pid, then stays up like a server
/// (it exits by itself after two minutes, so a leaked one cannot linger).
fn main_rs(version: &str) -> String {
  format!(
    r#"const VERSION: &str = {version};

fn main() {{
  let profile = std::env::var("ID_EFFECT_PROFILE").unwrap_or_default();
  let args: Vec<String> = std::env::args().skip(1).collect();
  println!("devapp {{VERSION}} profile={{profile}} args={{}} pid={{}}", args.join(","), std::process::id());
  std::thread::sleep(std::time::Duration::from_secs(120));
}}
"#
  )
}

fn project(dir: &Path) {
  std::fs::create_dir_all(dir.join("src")).unwrap();
  std::fs::write(dir.join("Cargo.toml"), CARGO_TOML).unwrap();
  std::fs::write(dir.join("src/main.rs"), main_rs("\"v1\"")).unwrap();
}

fn edit(dir: &Path, version: &str) {
  std::fs::write(dir.join("src/main.rs"), main_rs(version)).unwrap();
}

fn alive(pid: u32) -> bool {
  Command::new("kill")
    .args(["-0", &pid.to_string()])
    .stderr(Stdio::null())
    .status()
    .is_ok_and(|s| s.success())
}

/// The number after `pid=` or `pid ` in `line`.
fn pid(line: &str) -> u32 {
  let rest = line
    .split_once("pid=")
    .or_else(|| line.split_once("pid "))
    .map(|(_, rest)| rest)
    .unwrap_or_else(|| panic!("no pid in {line:?}"));
  let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
  digits.parse().unwrap()
}

/// A running `id-effect dev`, its stdout and stderr (with cargo's and the app's) merged into
/// lines. Dropping it kills it and every app it started.
struct Dev {
  child: Child,
  lines: Receiver<String>,
  seen: Vec<String>,
}

impl Dev {
  fn start(dir: &Path, extra: &[&str]) -> Self {
    let mut child = Command::new(env!("CARGO_BIN_EXE_id-effect"))
      .args([
        "dev",
        "--poll-ms",
        "100",
        "--debounce-ms",
        "100",
        "--grace-secs",
        "5",
        "--path",
      ])
      .arg(dir)
      .args(extra)
      .env("CARGO_NET_OFFLINE", "true")
      // The development certificate goes next to the fixture, not into the user's data dir.
      .env("ID_EFFECT_DEV_CERT_DIR", dir.join(".dev-cert"))
      // Build into the fixture's own target/, not a shared one.
      .env_remove("CARGO_TARGET_DIR")
      .env_remove("ID_EFFECT_PROFILE")
      .stdin(Stdio::null())
      .stdout(Stdio::piped())
      .stderr(Stdio::piped())
      .spawn()
      .expect("spawn id-effect dev");
    let (tx, lines) = channel();
    let pipes: [Box<dyn Read + Send>; 2] = [
      Box::new(child.stdout.take().unwrap()),
      Box::new(child.stderr.take().unwrap()),
    ];
    for pipe in pipes {
      let tx = tx.clone();
      thread::spawn(move || {
        for line in BufReader::new(pipe).lines().map_while(Result::ok) {
          if tx.send(line).is_err() {
            break;
          }
        }
      });
    }
    Self {
      child,
      lines,
      seen: Vec::new(),
    }
  }

  /// The next line containing `needle`; panics with the transcript after [`TIMEOUT`].
  fn wait_for(&mut self, needle: &str) -> String {
    let deadline = Instant::now() + TIMEOUT;
    loop {
      let left = deadline.saturating_duration_since(Instant::now());
      match self.lines.recv_timeout(left) {
        Ok(line) => {
          self.seen.push(line.clone());
          if line.contains(needle) {
            return line;
          }
        }
        Err(_) => panic!(
          "no line with {needle:?} within {TIMEOUT:?}; transcript:\n{}",
          self.seen.join("\n")
        ),
      }
    }
  }
}

impl Drop for Dev {
  fn drop(&mut self) {
    let _ = self.child.kill();
    let _ = self.child.wait();
    let apps = self
      .seen
      .iter()
      .filter(|l| l.starts_with("id-effect dev: started "))
      .map(|l| pid(l));
    for app in apps {
      let _ = Command::new("kill")
        .args(["-KILL", &app.to_string()])
        .stderr(Stdio::null())
        .status();
    }
  }
}

#[test]
fn edit_triggers_rebuild_and_restart() {
  let tmp = tempfile::tempdir().unwrap();
  let dir = tmp.path().join("devapp");
  project(&dir);
  let mut dev = Dev::start(&dir, &["--", "hello", "world"]);
  let first = dev.wait_for("devapp v1 ");
  assert!(first.contains("profile=dev args=hello,world "), "{first}");
  let old = pid(&first);
  // A different length too, in case the filesystem's mtime is coarse.
  edit(&dir, "\"v2-edited\"");
  dev.wait_for("changed src/main.rs; rebuilding");
  let stopped = dev.wait_for("stopped pid ");
  assert_eq!(pid(&stopped), old, "{stopped}");
  let second = dev.wait_for("devapp v2-edited ");
  assert!(second.contains("profile=dev args=hello,world "), "{second}");
  assert_ne!(pid(&second), old);
  assert!(!alive(old), "the old app {old} still runs");
}

#[test]
fn build_error_keeps_previous_child() {
  let tmp = tempfile::tempdir().unwrap();
  let dir = tmp.path().join("devapp");
  project(&dir);
  let mut dev = Dev::start(&dir, &[]);
  let old = pid(&dev.wait_for("devapp v1 "));
  edit(&dir, "v2_does_not_compile");
  let failed = dev.wait_for("build failed");
  assert!(
    failed.contains(&format!("keeping the running app (pid {old})")),
    "{failed}"
  );
  assert!(alive(old), "the running app {old} was stopped");
  // Fixed: now it restarts.
  edit(&dir, "\"v3-fixed\"");
  assert_eq!(pid(&dev.wait_for("stopped pid ")), old);
  dev.wait_for("devapp v3-fixed profile=dev");
  // Checked last: cargo's stderr arrives on its own pipe, in no fixed order with stdout.
  let rest: Vec<String> = dev.lines.try_iter().collect();
  dev.seen.extend(rest);
  assert!(
    dev.seen.iter().any(|l| l.contains("error[E0425]")),
    "cargo's error is shown:\n{}",
    dev.seen.join("\n")
  );
}

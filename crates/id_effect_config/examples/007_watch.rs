//! `ProfileWatch`: hot reload of `config/base.toml` and `config/<profile>.toml`.
//!
//! Loads the `dev` profile from a throwaway `config/` directory and subscribes to it, edits
//! `dev.toml` under the running poller (the subscriber receives the new value), then writes a
//! broken `dev.toml` (rejected: the previous value stays and `last_error` says why) and exits.
//!
//! Run: `cargo run -p id_effect_config --example 007_watch`

use std::path::Path;
use std::time::{Duration, SystemTime};

use id_effect::{Effect, Exit, Queue, QueueError, sleep, sync};
use id_effect_config::{ProfileWatch, Profiles};
use serde::Deserialize;

/// The watched settings.
#[derive(Clone, Debug, Deserialize, PartialEq)]
struct Server {
  host: String,
  port: u16,
}

/// How often the watcher polls the files' modification times.
const EVERY: Duration = Duration::from_millis(100);

/// Rewrite `path` and move its modification time forward, as an editor saving it later would
/// (so the change is seen even on a file system with a coarse clock).
fn rewrite(path: &Path, body: &str) {
  let before = std::fs::metadata(path)
    .and_then(|m| m.modified())
    .unwrap_or(SystemTime::UNIX_EPOCH);
  std::fs::write(path, body).expect("rewrite");
  std::fs::File::options()
    .write(true)
    .open(path)
    .and_then(|f| f.set_modified(before + Duration::from_secs(1)))
    .expect("set_modified");
}

fn main() {
  let root = tempfile::tempdir().expect("tempdir");
  let dir = root.path().join("config");
  std::fs::create_dir(&dir).expect("config dir");
  std::fs::write(dir.join("base.toml"), "host = \"127.0.0.1\"\nport = 3000\n").expect("base");
  let dev = dir.join("dev.toml");
  std::fs::write(&dev, "").expect("dev");

  let profiles = Profiles::new().dir(&dir).env_prefix("EXAMPLE_007_WATCH_");
  let watch: ProfileWatch<Server> = match profiles
    .profile("dev")
    .and_then(|p| profiles.watch_for(p, EVERY))
  {
    Ok(watch) => watch,
    Err(e) => {
      eprintln!("{e}");
      std::process::exit(2);
    }
  };

  // Subscribe, run the poller in the same scope (it stops when the scope closes), edit the file.
  let poller = watch.clone();
  let broken = dev.clone();
  let program: Effect<(Server, Server, Option<Server>), QueueError> =
    Effect::scoped(move |scope| {
      poller
        .subscribe(&scope)
        .flat_map(move |changes: Queue<Server>| {
          scope
            .fork_in(poller.run())
            .widen::<QueueError>()
            .then(changes.take())
            .flat_map(move |initial| {
              sync(move || rewrite(&dev, "port = 8080\n"))
                .then(changes.take())
                .flat_map(move |changed| {
                  sync(move || rewrite(&broken, "port = \n"))
                    .then(sleep(EVERY * 3))
                    .then(changes.poll())
                    .map(move |rejected| (initial, changed, rejected))
                })
            })
        })
    });

  match program.block_on() {
    Exit::Success((initial, changed, rejected)) => {
      println!("initial:  {}:{}", initial.host, initial.port);
      println!("reloaded: {}:{}", changed.host, changed.port);
      assert_eq!(changed.port, 8080);
      // The broken edit published nothing and the value is still the last good one.
      assert_eq!(rejected, None);
      let error = watch.last_error().expect("the broken edit is reported");
      println!("rejected: {}", error.lines().next().unwrap_or_default());
      println!("reloads:  {}", watch.generation());
    }
    Exit::Failure(cause) => panic!("the watch failed: {cause:?}"),
  }
}

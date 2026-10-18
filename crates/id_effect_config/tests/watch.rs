//! Hot reload of a profile directory through the public API (fault.reload #5): a subscriber of
//! [`ProfileWatch`] sees an edited file's new value, and an edit that does not parse keeps the
//! previous value. Virtual time (`TestExecutor`), real files.
#![cfg(all(feature = "toml", feature = "env"))]

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use id_effect::{
  Effect, Executor, Exit, Never, Queue, QueueError, TestExecutor, run_test_on, sleep, sync,
};
use id_effect_config::{ProfileWatch, Profiles};
use serde::Deserialize;

#[derive(Clone, Debug, Deserialize, PartialEq)]
struct App {
  name: String,
  port: u16,
}

const EVERY: Duration = Duration::from_secs(2);

/// `<root>/config/{base,prod}.toml`, as an application ships them.
fn app_root() -> (tempfile::TempDir, PathBuf) {
  let root = tempfile::tempdir().expect("tempdir");
  let config = root.path().join("config");
  std::fs::create_dir(&config).expect("config dir");
  std::fs::write(config.join("base.toml"), "name = \"ledger\"\nport = 3000\n").expect("base");
  std::fs::write(config.join("prod.toml"), "port = 8080\n").expect("prod");
  (root, config)
}

fn watch(config: &Path) -> ProfileWatch<App> {
  let profiles = Profiles::new()
    .dir(config)
    .env_prefix("IDCFG_IT_WATCH_UNSET_");
  profiles
    .watch_for(profiles.profile("prod").unwrap(), EVERY)
    .expect("initial load")
}

/// Rewrite `path` and move its modification time forward, so the poll sees the change even on
/// a file system with a coarse clock.
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

fn app(port: u16) -> App {
  App {
    name: "ledger".into(),
    port,
  }
}

fn books() -> App {
  App {
    name: "books".into(),
    port: 8080,
  }
}

fn current(exec: &TestExecutor, w: &ProfileWatch<App>) -> App {
  match run_test_on(exec, w.get::<Never, _>()) {
    Exit::Success(app) => app,
    other => panic!("get failed: {other:?}"),
  }
}

#[test]
fn subscriber_sees_the_edited_profile() {
  let (_root, config) = app_root();
  let w = watch(&config);
  let exec = TestExecutor::new();
  let start = exec.now();
  let prod = config.join("prod.toml");

  let poller = w.clone();
  let program: Effect<(App, App), QueueError> = Effect::scoped(move |scope| {
    poller
      .subscribe(&scope)
      .flat_map(move |changes: Queue<App>| {
        scope
          .fork_in(poller.run())
          .widen::<QueueError>()
          .then(changes.take())
          .flat_map(move |initial| {
            sync(move || rewrite(&prod, "port = 9090\n"))
              .then(changes.take())
              .map(move |changed| (initial, changed))
          })
      })
  });
  match run_test_on(&exec, program) {
    Exit::Success((initial, changed)) => {
      assert_eq!(initial, app(8080), "profile over base");
      assert_eq!(changed, app(9090), "the edit, merged over base again");
    }
    other => panic!("expected the change, got {other:?}"),
  }
  assert_eq!(exec.now() - start, EVERY, "picked up by the first poll");
  assert_eq!(w.generation(), 1);
  assert_eq!(w.last_error(), None);
  assert_eq!(current(&exec, &w), app(9090));
}

#[test]
fn invalid_edit_keeps_the_previous_value() {
  let (_root, config) = app_root();
  let w = watch(&config);
  let exec = TestExecutor::new();
  let base = config.join("base.toml");
  let fixed = base.clone();

  // A broken base file, then a good edit, under one poller.
  let poller = w.clone();
  let probe = w.clone();
  let program: Effect<(Option<App>, Option<String>, App), QueueError> =
    Effect::scoped(move |scope| {
      poller
        .subscribe(&scope)
        .flat_map(move |changes: Queue<App>| {
          changes.take().flat_map(move |_initial| {
            sync(move || rewrite(&base, "name = \"ledger\"\nport = \n"))
              .then(scope.fork_in(poller.run()).widen::<QueueError>())
              .then(sleep(EVERY * 3))
              .then(changes.poll())
              .flat_map(move |published| {
                let error = probe.last_error();
                sync(move || rewrite(&fixed, "name = \"books\"\nport = 4000\n"))
                  .then(changes.take())
                  .map(move |recovered| (published, error, recovered))
              })
          })
        })
    });
  match run_test_on(&exec, program) {
    Exit::Success((published, error, recovered)) => {
      assert_eq!(published, None, "a broken edit notifies nobody");
      let error = error.expect("the broken edit is reported");
      assert!(error.contains("TOML parse error"), "{error}");
      // The next good edit is picked up (its port stays under prod.toml's).
      assert_eq!(recovered, books());
    }
    other => panic!("expected the rejection then the recovery, got {other:?}"),
  }
  // Only the good edit replaced the value; the rejection was cleared by it.
  assert_eq!(w.generation(), 1);
  assert_eq!(w.last_error(), None);

  // A well-formed edit of the wrong type is rejected the same way.
  rewrite(&config.join("prod.toml"), "port = \"http\"\n");
  assert!(matches!(
    run_test_on(
      &exec,
      w.reload_if_changed::<id_effect_config::ConfigError, _>()
    ),
    Exit::Failure(_)
  ));
  assert_eq!(current(&exec, &w), books(), "the previous value stays");
  assert!(w.last_error().is_some());
  assert_eq!(w.generation(), 1);
}

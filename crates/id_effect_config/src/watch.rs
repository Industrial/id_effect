//! Hot reload of [`Profiles`] files: poll `base.toml` and `<profile>.toml` by modification time,
//! re-merge on a change and publish the new value through a [`SubscriptionRef`].
//!
//! [`Profiles::watch`] (or [`Profiles::watch_for`] / [`Profiles::watch_with`]) loads the
//! profile once, like [`Profiles::load_for`], and returns a [`ProfileWatch`]. Its
//! [`run`](ProfileWatch::run) effect sleeps the poll interval, compares each file's modification
//! time and length with the previous poll, and on a change merges every source again (files,
//! overlays, environment) and parses the result. A parsed value replaces the current one and is
//! published to every subscriber; a change that does not parse keeps the previous value and is
//! reported ([`ProfileWatch::reload_if_changed`] fails with the [`ConfigError`],
//! [`ProfileWatch::last_error`] keeps its message). The watcher never crashes the application.
//!
//! Only the two profile files are watched (no `notify` dependency: polling works the same on
//! every platform and under the virtual clock of `id_effect::TestExecutor`). Overlays and the
//! environment are re-read on each reload but a change to them alone does not trigger one; use
//! [`ProfileWatch::reload`] to force it.
//!
//! ```rust
//! use std::time::Duration;
//!
//! use id_effect::{Effect, Exit, Never, run_test};
//! use id_effect_config::{ConfigError, Profiles};
//! use serde::Deserialize;
//!
//! #[derive(Clone, Debug, Deserialize, PartialEq)]
//! struct App {
//!   port: u16,
//! }
//!
//! let dir = tempfile::tempdir().unwrap();
//! std::fs::write(dir.path().join("base.toml"), "port = 3000").unwrap();
//! let profiles = Profiles::new().dir(dir.path()).env_prefix("DOC_WATCH_APP_");
//! let watch = profiles.watch_for::<App>(profiles.profile("dev")?, Duration::from_secs(5))?;
//!
//! // Subscribe, run the poller in the same scope, and wait for the first change.
//! let base = dir.path().join("base.toml");
//! let program: Effect<App, Never> = Effect::scoped(move |scope| {
//!   watch.subscribe(&scope).flat_map(move |changes| {
//!     scope.fork_in(watch.run()).flat_map(move |_poller| {
//!       changes.take().map(|initial: App| assert_eq!(initial.port, 3000))
//!         .then(id_effect::sync(move || {
//!           std::fs::write(&base, "port = 8080").unwrap();
//!           let later = std::time::SystemTime::now() + Duration::from_secs(60);
//!           std::fs::File::options().write(true).open(&base).unwrap().set_modified(later).unwrap();
//!         }))
//!         .then(changes.take())
//!         .catch(|_| unreachable!("the queue lives as long as the scope"))
//!     })
//!   })
//! });
//! assert!(matches!(run_test(program), Exit::Success(App { port: 8080 })));
//! # Ok::<(), ConfigError>(())
//! ```

use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, SystemTime};

use ::id_effect::{Effect, Never, Queue, Scope, SubscriptionRef, fail, sleep, succeed, suspend};
use serde::de::DeserializeOwned;

use crate::error::ConfigError;
use crate::profile::{Profile, ProfiledConfig, Profiles};

/// Turns the merged profile into the watched value.
type Parse<C> = dyn Fn(&ProfiledConfig) -> Result<C, ConfigError> + Send + Sync;

/// What a poll saw of one file: modification time and length, `None` when it does not exist.
type Stamp = Option<(Option<SystemTime>, u64)>;

fn stamp(path: &Path) -> Stamp {
  std::fs::metadata(path)
    .ok()
    .map(|m| (m.modified().ok(), m.len()))
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
  m.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Profiles {
  /// [`Profiles::resolve`] then [`Profiles::watch_for`].
  pub fn watch<C>(&self, every: Duration) -> Result<ProfileWatch<C>, ConfigError>
  where
    C: DeserializeOwned + Clone + Send + Sync + 'static,
  {
    self.watch_for(self.resolve()?, every)
  }

  /// Load `profile` as `C` (see [`ProfiledConfig::extract`]) and watch its files, polling every
  /// `every` once [`ProfileWatch::run`] runs.
  ///
  /// The initial load must succeed: a file that does not parse or a value that does not
  /// deserialize is returned as the error, and a zero `every` is [`ConfigError::Invalid`].
  pub fn watch_for<C>(
    &self,
    profile: Profile,
    every: Duration,
  ) -> Result<ProfileWatch<C>, ConfigError>
  where
    C: DeserializeOwned + Clone + Send + Sync + 'static,
  {
    self.watch_with(profile, every, ProfiledConfig::extract::<C>)
  }

  /// Like [`Profiles::watch_for`], with `parse` turning the merged profile into the value (a
  /// typed config built from [`Config`](crate::Config) descriptors, or the [`ProfiledConfig`]
  /// itself).
  pub fn watch_with<C, F>(
    &self,
    profile: Profile,
    every: Duration,
    parse: F,
  ) -> Result<ProfileWatch<C>, ConfigError>
  where
    C: Clone + Send + Sync + 'static,
    F: Fn(&ProfiledConfig) -> Result<C, ConfigError> + Send + Sync + 'static,
  {
    if every.is_zero() {
      return Err(ConfigError::Invalid {
        path: "watch.every".into(),
        value: format!("{every:?}"),
        reason: "must be greater than zero".into(),
      });
    }
    let files = vec![self.base_path(), self.profile_path(&profile)];
    // Stamped before the load, so a write racing the load is picked up by the next poll.
    let stamps = files.iter().map(|p| stamp(p)).collect();
    let initial = parse(&self.load_for(profile.clone())?)?;
    Ok(ProfileWatch {
      inner: Arc::new(Inner {
        profiles: self.clone(),
        profile,
        files,
        every,
        parse: Box::new(parse),
        value: SubscriptionRef::new(initial),
        stamps: Mutex::new(stamps),
        generation: AtomicU64::new(0),
        last_error: Mutex::new(None),
      }),
    })
  }
}

struct Inner<C> {
  profiles: Profiles,
  profile: Profile,
  files: Vec<PathBuf>,
  every: Duration,
  parse: Box<Parse<C>>,
  value: SubscriptionRef<C>,
  stamps: Mutex<Vec<Stamp>>,
  generation: AtomicU64,
  last_error: Mutex<Option<String>>,
}

impl<C> Inner<C> {
  /// Re-merge and parse when a file changed (always with `force`); `None` when nothing changed.
  /// Records the outcome in `last_error`.
  fn check(&self, force: bool) -> Option<Result<C, ConfigError>> {
    let mut stamps = lock(&self.stamps);
    let now: Vec<Stamp> = self.files.iter().map(|p| stamp(p)).collect();
    if !force && now == *stamps {
      return None;
    }
    // Stamped even when the change is rejected: the same broken file is reported once, and the
    // next edit is picked up.
    *stamps = now;
    let result = self
      .profiles
      .load_for(self.profile.clone())
      .and_then(|cfg| (self.parse)(&cfg));
    *lock(&self.last_error) = result.as_ref().err().map(ToString::to_string);
    Some(result)
  }
}

/// A profile's configuration kept current from its files (from [`Profiles::watch`]).
///
/// The value lives in a [`SubscriptionRef`]: [`get`](Self::get) reads it,
/// [`subscribe`](Self::subscribe) receives it and every later value. Run
/// [`run`](Self::run) (typically `scope.fork_in(watch.run())`) to poll the files. Cloning yields
/// another handle to the same watch.
///
/// `Debug` names the profile, the files and the reload count but never prints the value or an
/// error message, either of which can quote a secret.
pub struct ProfileWatch<C> {
  inner: Arc<Inner<C>>,
}

impl<C> Clone for ProfileWatch<C> {
  fn clone(&self) -> Self {
    Self {
      inner: Arc::clone(&self.inner),
    }
  }
}

impl<C> fmt::Debug for ProfileWatch<C> {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("ProfileWatch")
      .field("profile", &self.inner.profile)
      .field("files", &self.inner.files)
      .field("every", &self.inner.every)
      .field("generation", &self.generation())
      .finish_non_exhaustive()
  }
}

impl<C> ProfileWatch<C> {
  /// The watched profile.
  #[inline]
  pub fn profile(&self) -> &Profile {
    &self.inner.profile
  }

  /// The polled files: `base.toml`, then `<profile>.toml`.
  #[inline]
  pub fn files(&self) -> &[PathBuf] {
    &self.inner.files
  }

  /// The poll interval.
  #[inline]
  pub fn every(&self) -> Duration {
    self.inner.every
  }

  /// How many reloads have replaced the value (0 for the initial load).
  pub fn generation(&self) -> u64 {
    self.inner.generation.load(Ordering::SeqCst)
  }

  /// The message of the last rejected reload, cleared by the next successful one.
  pub fn last_error(&self) -> Option<String> {
    lock(&self.inner.last_error).clone()
  }

  /// The cell holding the current value.
  #[inline]
  pub fn subscription(&self) -> &SubscriptionRef<C> {
    &self.inner.value
  }
}

impl<C: Clone + Send + Sync + 'static> ProfileWatch<C> {
  /// Reads the current value.
  pub fn get<E, R>(&self) -> Effect<C, E, R> {
    self.inner.value.get()
  }

  /// Subscribes to changes: the queue receives the current value first, then every reloaded
  /// value. The subscription ends when `scope` closes.
  pub fn subscribe<E, R>(&self, scope: &Scope) -> Effect<Queue<C>, E, R>
  where
    E: Send + 'static,
    R: Send + Sync + 'static,
  {
    self.inner.value.subscribe(scope)
  }

  /// Poll once: when a file changed since the last poll, re-merge and parse it, publish the new
  /// value and succeed with `true`; succeed with `false` when nothing changed.
  ///
  /// A change that does not parse keeps the previous value and fails with the error as
  /// `E::from(error)` (also kept in [`last_error`](Self::last_error)).
  pub fn reload_if_changed<E, R>(&self) -> Effect<bool, E, R>
  where
    E: From<ConfigError> + Send + 'static,
    R: Send + Sync + 'static,
  {
    self.step::<E, R>(false).flat_map(report)
  }

  /// Re-merge and parse now, whether or not a file changed (for example after an overlay or
  /// the environment changed); failures as in [`reload_if_changed`](Self::reload_if_changed).
  pub fn reload<E, R>(&self) -> Effect<(), E, R>
  where
    E: From<ConfigError> + Send + 'static,
    R: Send + Sync + 'static,
  {
    self.step::<E, R>(true).flat_map(report).map(drop)
  }

  /// Sleep the poll interval, [`reload_if_changed`](Self::reload_if_changed), repeat until
  /// interrupted. A rejected change keeps the previous value; the loop goes on.
  pub fn run<R: Send + Sync + 'static>(&self) -> Effect<(), Never, R> {
    let watch = self.clone();
    sleep::<Never, R>(self.inner.every)
      .flat_map(move |()| watch.step::<Never, R>(false).flat_map(move |_| watch.run()))
  }

  /// One poll: `Ok(true)` published, `Ok(false)` unchanged, `Err` rejected (value kept).
  fn step<E, R>(&self, force: bool) -> Effect<Result<bool, ConfigError>, E, R>
  where
    E: Send + 'static,
    R: Send + Sync + 'static,
  {
    let inner = Arc::clone(&self.inner);
    suspend(move || match inner.check(force) {
      None => succeed(Ok(false)),
      Some(Err(e)) => succeed(Err(e)),
      Some(Ok(value)) => inner.value.set::<E, R>(value).map(move |()| {
        inner.generation.fetch_add(1, Ordering::SeqCst);
        Ok(true)
      }),
    })
  }
}

fn report<E, R>(outcome: Result<bool, ConfigError>) -> Effect<bool, E, R>
where
  E: From<ConfigError> + Send + 'static,
  R: Send + Sync + 'static,
{
  match outcome {
    Ok(changed) => succeed(changed),
    Err(e) => fail(E::from(e)),
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use ::id_effect::{Executor, Exit, QueueError, TestExecutor, run_test_on, sync};
  use serde::Deserialize;

  #[derive(Clone, Debug, Deserialize, PartialEq)]
  struct Cfg {
    port: u16,
    #[serde(default)]
    password: Option<String>,
  }

  const EVERY: Duration = Duration::from_secs(5);

  fn write(dir: &tempfile::TempDir, name: &str, body: &str) -> PathBuf {
    let path = dir.path().join(name);
    std::fs::write(&path, body).expect("write");
    path
  }

  /// Rewrite `path` and move its modification time forward, so the change is seen even on a
  /// file system with a coarse clock and with a body of the same length.
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

  fn watch(dir: &tempfile::TempDir) -> ProfileWatch<Cfg> {
    let profiles = Profiles::new()
      .dir(dir.path())
      .env_prefix("IDCFG_WATCH_UNSET_");
    profiles
      .watch_for(profiles.profile("prod").unwrap(), EVERY)
      .expect("initial load")
  }

  fn port(p: u16) -> Cfg {
    Cfg {
      port: p,
      password: None,
    }
  }

  #[test]
  fn profile_file_change_notifies_subscribers() {
    let dir = tempfile::tempdir().expect("tempdir");
    write(&dir, "base.toml", "port = 1\n");
    let prod = write(&dir, "prod.toml", "");
    let w = watch(&dir);
    let exec = TestExecutor::new();
    let start = exec.now();

    let poller = w.clone();
    let program: Effect<Vec<Cfg>, QueueError> = Effect::scoped(move |scope| {
      poller.subscribe(&scope).flat_map(move |q: Queue<Cfg>| {
        q.take().flat_map(move |initial| {
          sync(move || rewrite(&prod, "port = 2\n"))
            .then(scope.fork_in(poller.run()).widen::<QueueError>())
            .then(q.take())
            .map(move |changed| vec![initial, changed])
        })
      })
    });
    match run_test_on(&exec, program) {
      Exit::Success(seen) => assert_eq!(seen, [port(1), port(2)]),
      other => panic!("expected the change, got {other:?}"),
    }
    // Picked up by the first poll, one interval of virtual time later.
    assert_eq!(exec.now() - start, EVERY);
    assert_eq!(w.generation(), 1);
    assert_eq!(w.last_error(), None);
    assert!(matches!(run_test_on(&exec, w.get::<Never, _>()), Exit::Success(c) if c == port(2)));
  }

  #[test]
  fn invalid_change_keeps_previous() {
    let dir = tempfile::tempdir().expect("tempdir");
    let base = write(&dir, "base.toml", "port = 1\n");
    let w = watch(&dir);
    let exec = TestExecutor::new();

    // A broken file under the running poller: no notification, the old value stays.
    rewrite(&base, "port = \n");
    let poller = w.clone();
    let program: Effect<Option<Cfg>, QueueError> = Effect::scoped(move |scope| {
      poller.subscribe(&scope).flat_map(move |q: Queue<Cfg>| {
        q.take().flat_map(move |_initial| {
          scope
            .fork_in(poller.run())
            .widen::<QueueError>()
            .then(sleep(EVERY * 3))
            .then(q.poll())
        })
      })
    });
    assert!(matches!(run_test_on(&exec, program), Exit::Success(None)));
    assert_eq!(w.generation(), 0);
    let reported = w.last_error().expect("the rejected change is reported");
    assert!(reported.contains("TOML parse error"), "{reported}");
    assert!(matches!(run_test_on(&exec, w.get::<Never, _>()), Exit::Success(c) if c == port(1)));

    // A well-formed file with a value of the wrong type is rejected the same way, as an error.
    rewrite(&base, "port = \"http\"\n");
    match run_test_on(&exec, w.reload_if_changed::<ConfigError, _>()) {
      Exit::Failure(cause) => assert!(matches!(
        cause.fail_option(),
        Some(ConfigError::Figment(e)) if e.path == ["port"]
      )),
      other => panic!("expected the parse error, got {other:?}"),
    }
    assert!(matches!(run_test_on(&exec, w.get::<Never, _>()), Exit::Success(c) if c == port(1)));

    // The next valid edit is applied and clears the error.
    rewrite(&base, "port = 3\n");
    assert!(matches!(
      run_test_on(&exec, w.reload_if_changed::<ConfigError, _>()),
      Exit::Success(true)
    ));
    assert_eq!(w.generation(), 1);
    assert_eq!(w.last_error(), None);
    assert!(matches!(run_test_on(&exec, w.get::<Never, _>()), Exit::Success(c) if c == port(3)));
  }

  #[test]
  fn unchanged_files_do_not_reload_and_reload_forces() {
    let dir = tempfile::tempdir().expect("tempdir");
    write(&dir, "base.toml", "port = 1\n");
    let w = watch(&dir);
    let exec = TestExecutor::new();
    assert!(matches!(
      run_test_on(&exec, w.reload_if_changed::<ConfigError, _>()),
      Exit::Success(false)
    ));
    assert_eq!(w.generation(), 0);
    assert!(matches!(
      run_test_on(&exec, w.reload::<ConfigError, _>()),
      Exit::Success(())
    ));
    assert_eq!(w.generation(), 1);

    // A profile file that appears later is a change too.
    let prod = write(&dir, "prod.toml", "port = 4\n");
    rewrite(&prod, "port = 4\n");
    assert!(matches!(
      run_test_on(&exec, w.reload_if_changed::<ConfigError, _>()),
      Exit::Success(true)
    ));
    assert!(matches!(run_test_on(&exec, w.get::<Never, _>()), Exit::Success(c) if c == port(4)));
  }

  #[test]
  fn watch_rejects_a_zero_interval_and_an_invalid_initial_load() {
    let dir = tempfile::tempdir().expect("tempdir");
    write(&dir, "base.toml", "port = 1\n");
    let profiles = Profiles::new().dir(dir.path());
    let prod = profiles.profile("prod").unwrap();
    assert!(matches!(
      profiles.watch_for::<Cfg>(prod.clone(), Duration::ZERO),
      Err(ConfigError::Invalid { path, .. }) if path == "watch.every"
    ));
    write(&dir, "prod.toml", "port = [");
    assert!(matches!(
      profiles.watch_for::<Cfg>(prod, EVERY),
      Err(ConfigError::Figment(_))
    ));
  }

  #[test]
  fn watch_with_parses_custom_values_and_resolves_the_profile() {
    let dir = tempfile::tempdir().expect("tempdir");
    write(&dir, "base.toml", "port = 1\n");
    write(&dir, "test.toml", "port = 7\n");
    let profiles = Profiles::new()
      .dir(dir.path())
      .env_var("IDCFG_WATCH_PROFILE");
    temp_env::with_var("IDCFG_WATCH_PROFILE", Some("test"), || {
      let w = profiles.watch::<Cfg>(EVERY).expect("watch");
      assert_eq!(w.profile().as_str(), "test");
      assert_eq!(w.every(), EVERY);
      assert_eq!(
        w.files(),
        [dir.path().join("base.toml"), dir.path().join("test.toml")]
      );
    });
    let w = profiles
      .watch_with(profiles.profile("test").unwrap(), EVERY, |cfg| {
        crate::Config::integer("port").load(&cfg.provider())
      })
      .expect("watch_with");
    let exec = TestExecutor::new();
    assert!(matches!(
      run_test_on(&exec, w.get::<Never, _>()),
      Exit::Success(7)
    ));
    assert!(matches!(
      run_test_on(&exec, w.subscription().get::<Never, _>()),
      Exit::Success(7)
    ));
  }

  #[test]
  fn errors_never_quote_a_malformed_secret_line() {
    let dir = tempfile::tempdir().expect("tempdir");
    let base = write(&dir, "base.toml", "port = 1\n");
    let w = watch(&dir);
    let exec = TestExecutor::new();
    let check = |body: &str, expect: &[&str]| {
      rewrite(&base, body);
      let err = match run_test_on(&exec, w.reload_if_changed::<ConfigError, _>()) {
        Exit::Failure(cause) => cause.fail_option().expect("a typed failure"),
        other => panic!("expected the error, got {other:?}"),
      };
      let reported = w.last_error().expect("reported");
      let shown = format!("{err}\n{err:?}\n{reported}");
      assert!(!shown.contains("hunter2"), "{shown}");
      for kept in expect {
        assert!(reported.contains(kept), "{kept:?} missing: {reported}");
      }
    };
    // An unquoted string: the TOML parser quotes the whole line.
    check(
      "port = 1\npassword = hunter2-s3cr3t\n",
      &["TOML parse error at line 2, column 12", "invalid string"],
    );
    // Well-formed, wrong type: serde reports the value it found.
    check(
      "port = \"hunter2-s3cr3t\"\n",
      &[
        "invalid type: found string, expected u16",
        "for key \"default.port\"",
        "base.toml",
      ],
    );
  }

  #[test]
  fn debug_never_prints_the_value_or_the_error() {
    let dir = tempfile::tempdir().expect("tempdir");
    let base = write(&dir, "base.toml", "port = 1\npassword = \"s3cr3t\"\n");
    let w = watch(&dir);
    rewrite(&base, "port = \"s3cr3t\"\n");
    let _ = run_test_on(
      &TestExecutor::new(),
      w.reload_if_changed::<ConfigError, _>(),
    );
    assert!(w.last_error().is_some());
    let shown = format!("{w:?}");
    assert!(shown.contains("prod"), "{shown}");
    assert!(!shown.contains("s3cr3t"), "{shown}");
  }
}

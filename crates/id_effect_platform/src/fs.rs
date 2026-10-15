//! Virtual filesystem ([`FileSystem`]): live Tokio ([`LiveFileSystem`]) and in-memory test double ([`TestFileSystem`]).
//!
//! The capability is [`FileSystemService`]; [`LiveFileSystemProvider::layer`] and
//! [`provide_file_system`] are the [`Layer`]s that provide it ([`FileSystemEnv`]).
//!
//! ```rust
//! use std::path::{Path, PathBuf};
//!
//! use id_effect::Exit;
//! use id_effect_platform::fs::{FileSystem, TestFileSystem, file_system_env, read};
//!
//! let fs = TestFileSystem::new();
//! let _ = fs.write(Path::new("a.txt"), b"hi").block_on();
//! let bytes = read(PathBuf::from("a.txt"))
//!   .provide_all(file_system_env(fs))
//!   .block_on();
//! assert!(matches!(bytes, Exit::Success(b) if b == b"hi"));
//! ```
//!
//! ## Path handling (Phase A / `iep-a-050`)
//!
//! The public API uses [`std::path::Path`] for maximum compatibility. Callers that need UTF-8 paths
//! should validate or convert at the boundary (e.g. with [`camino::Utf8PathBuf`](https://docs.rs/camino)
//! in application code) before calling these traits.
//!
//! ## Security (Phase A / `iep-a-033`)
//!
//! - **Path traversal:** [`TestFileSystem`] rejects path keys containing `..`. Live I/O follows the OS;
//!   sandboxed apps should canonicalize or jail paths at a higher layer.
//! - **Symlinks:** No special symlink policy here; treat remote paths as untrusted unless you control the tree.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use id_effect::next::{
  Effect, HCons, HList, HNil, Layer, Selector, fail, from_async, hlist, need, succeed, suspend,
};

use crate::error::FsError;

/// Capability: portable filesystem operations as [`Effect`] values.
pub trait FileSystem: Send + Sync + 'static {
  /// Read entire file into a byte vector.
  fn read(&self, path: &Path) -> Effect<Vec<u8>, FsError>;
  /// Write bytes (overwrite).
  fn write(&self, path: &Path, data: &[u8]) -> Effect<(), FsError>;
  /// Append bytes (create if missing).
  fn append(&self, path: &Path, data: &[u8]) -> Effect<(), FsError>;
  /// Create a directory (and parents), Unix semantics.
  fn create_dir_all(&self, path: &Path) -> Effect<(), FsError>;
  /// Remove a file.
  fn remove_file(&self, path: &Path) -> Effect<(), FsError>;
  /// Metadata: file length if a regular file.
  fn metadata_len(&self, path: &Path) -> Effect<u64, FsError>;
  /// Whether `path` exists (file or directory).
  fn exists(&self, path: &Path) -> Effect<bool, FsError>;
}

/// Tokio-backed live filesystem.
#[derive(Clone, Copy, Debug, Default)]
pub struct LiveFileSystem;

impl LiveFileSystem {
  /// New live filesystem handle.
  #[inline]
  pub fn new() -> Self {
    Self
  }
}

impl FileSystem for LiveFileSystem {
  fn read(&self, path: &Path) -> Effect<Vec<u8>, FsError> {
    let path = path.to_path_buf();
    from_async(move || async move { tokio::fs::read(&path).await.map_err(FsError::from) })
  }

  fn write(&self, path: &Path, data: &[u8]) -> Effect<(), FsError> {
    let path = path.to_path_buf();
    let data = data.to_vec();
    from_async(move || async move { tokio::fs::write(&path, &data).await.map_err(FsError::from) })
  }

  fn append(&self, path: &Path, data: &[u8]) -> Effect<(), FsError> {
    let path = path.to_path_buf();
    let data = data.to_vec();
    from_async(move || async move {
      use tokio::io::AsyncWriteExt;
      let mut f = tokio::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .await
        .map_err(FsError::from)?;
      f.write_all(&data).await.map_err(FsError::from)?;
      f.flush().await.map_err(FsError::from)?;
      Ok(())
    })
  }

  fn create_dir_all(&self, path: &Path) -> Effect<(), FsError> {
    let path = path.to_path_buf();
    from_async(move || async move {
      tokio::fs::create_dir_all(&path)
        .await
        .map_err(FsError::from)
    })
  }

  fn remove_file(&self, path: &Path) -> Effect<(), FsError> {
    let path = path.to_path_buf();
    from_async(move || async move { tokio::fs::remove_file(&path).await.map_err(FsError::from) })
  }

  fn metadata_len(&self, path: &Path) -> Effect<u64, FsError> {
    let path = path.to_path_buf();
    from_async(move || async move {
      let m = tokio::fs::metadata(&path).await.map_err(FsError::from)?;
      Ok(m.len())
    })
  }

  fn exists(&self, path: &Path) -> Effect<bool, FsError> {
    let path = path.to_path_buf();
    from_async(move || async move { tokio::fs::try_exists(&path).await.map_err(FsError::from) })
  }
}

type Tree = std::collections::BTreeMap<String, Vec<u8>>;

/// In-memory filesystem for tests (single mutex; not for production concurrency).
#[derive(Clone, Default)]
pub struct TestFileSystem {
  inner: Arc<Mutex<Tree>>,
}

impl std::fmt::Debug for TestFileSystem {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("TestFileSystem").finish_non_exhaustive()
  }
}

fn not_found() -> FsError {
  FsError::Io(std::io::Error::new(
    std::io::ErrorKind::NotFound,
    "test file not found",
  ))
}

impl TestFileSystem {
  /// Empty tree.
  #[inline]
  pub fn new() -> Self {
    Self::default()
  }

  #[cfg(test)]
  fn poison_inner_mutex(&self) {
    let inner = Arc::clone(&self.inner);
    let handle = std::thread::spawn(move || {
      let _guard = inner.lock().expect("lock");
      panic!("test mutex poison");
    });
    assert!(handle.join().is_err());
  }

  fn key(path: &Path) -> Result<String, FsError> {
    let s = path
      .to_str()
      .ok_or_else(|| FsError::PathNotAllowed("non-utf8 path".into()))?;
    if s.contains("..") {
      return Err(FsError::PathNotAllowed(
        "`..` not allowed in test paths".into(),
      ));
    }
    Ok(s.to_string())
  }

  /// A lazy step over the tree at `path`'s key (the key is validated eagerly).
  fn with_tree<A: Send + 'static>(
    &self,
    path: &Path,
    f: impl FnOnce(&mut Tree, String) -> Result<A, FsError> + Send + 'static,
  ) -> Effect<A, FsError> {
    let key = match Self::key(path) {
      Ok(k) => k,
      Err(e) => return fail(e),
    };
    let inner = Arc::clone(&self.inner);
    suspend(move || {
      let result = inner
        .lock()
        .map_err(|e| FsError::PathNotAllowed(e.to_string()))
        .and_then(|mut tree| f(&mut tree, key));
      match result {
        Ok(a) => succeed(a),
        Err(e) => fail(e),
      }
    })
  }
}

impl FileSystem for TestFileSystem {
  fn read(&self, path: &Path) -> Effect<Vec<u8>, FsError> {
    self.with_tree(path, |tree, key| {
      tree.get(&key).cloned().ok_or_else(not_found)
    })
  }

  fn write(&self, path: &Path, data: &[u8]) -> Effect<(), FsError> {
    let data = data.to_vec();
    self.with_tree(path, move |tree, key| {
      tree.insert(key, data);
      Ok(())
    })
  }

  fn append(&self, path: &Path, data: &[u8]) -> Effect<(), FsError> {
    let data = data.to_vec();
    self.with_tree(path, move |tree, key| {
      tree.entry(key).or_default().extend_from_slice(&data);
      Ok(())
    })
  }

  fn create_dir_all(&self, _path: &Path) -> Effect<(), FsError> {
    succeed(())
  }

  fn remove_file(&self, path: &Path) -> Effect<(), FsError> {
    self.with_tree(path, |tree, key| {
      tree.remove(&key).map(drop).ok_or_else(not_found)
    })
  }

  fn metadata_len(&self, path: &Path) -> Effect<u64, FsError> {
    self.with_tree(path, |tree, key| {
      tree.get(&key).map(|v| v.len() as u64).ok_or_else(not_found)
    })
  }

  fn exists(&self, path: &Path) -> Effect<bool, FsError> {
    self.with_tree(path, |tree, key| Ok(tree.contains_key(&key)))
  }
}

/// The [`FileSystem`] capability: a shared handle to the installed implementation.
#[derive(Clone)]
pub struct FileSystemService(pub Arc<dyn FileSystem>);

impl FileSystemService {
  /// Wrap `fs` as the capability.
  #[inline]
  pub fn new(fs: impl FileSystem) -> Self {
    Self(Arc::new(fs))
  }
}

impl std::ops::Deref for FileSystemService {
  type Target = dyn FileSystem;

  fn deref(&self) -> &Self::Target {
    &*self.0
  }
}

impl std::fmt::Debug for FileSystemService {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_tuple("FileSystemService")
      .field(&"<dyn FileSystem>")
      .finish()
  }
}

/// The minimal typed environment holding only [`FileSystemService`] (the output of the
/// filesystem layers).
pub type FileSystemEnv = HCons<FileSystemService, HNil>;

/// A [`FileSystemEnv`] wrapping `fs` (for `.provide_all(..)` in tests and entry points).
#[inline]
pub fn file_system_env(fs: impl FileSystem) -> FileSystemEnv {
  hlist![FileSystemService::new(fs)]
}

/// A [`Layer`] providing `fs` as the [`FileSystemService`] capability (e.g. a [`TestFileSystem`]).
#[inline]
pub fn provide_file_system<E: Send + 'static>(fs: impl FileSystem) -> Layer<FileSystemEnv, E> {
  Layer::succeed(file_system_env(fs))
}

/// The Tokio-backed live [`FileSystem`] provider.
#[derive(Clone, Copy, Debug, Default)]
pub struct LiveFileSystemProvider;

impl LiveFileSystemProvider {
  /// The layer: a [`LiveFileSystem`] as the [`FileSystemService`] capability.
  #[inline]
  pub fn layer<E: Send + 'static>() -> Layer<FileSystemEnv, E> {
    provide_file_system(LiveFileSystem::new())
  }
}

/// Read via the [`FileSystemService`] in `R`.
#[inline]
pub fn read<R, I>(path: PathBuf) -> Effect<Vec<u8>, FsError, R>
where
  R: Selector<FileSystemService, I> + HList,
{
  need::<FileSystemService>().flat_map(move |fs| fs.read(&path).narrow())
}

/// Exists check via the [`FileSystemService`] in `R`.
#[inline]
pub fn exists<R, I>(path: PathBuf) -> Effect<bool, FsError, R>
where
  R: Selector<FileSystemService, I> + HList,
{
  need::<FileSystemService>().flat_map(move |fs| fs.exists(&path).narrow())
}

#[cfg(test)]
mod tests {
  use super::*;

  use id_effect::{Cause, Exit};
  use std::path::Path;

  /// The v1 `run_blocking(effect, ())` shape over the `next` blocking edge.
  fn run_blocking<A: Send + 'static>(eff: Effect<A, FsError>) -> Result<A, FsError> {
    match eff.block_on() {
      Exit::Success(a) => Ok(a),
      Exit::Failure(Cause::Fail(e)) => Err(e),
      Exit::Failure(c) => panic!("unexpected cause: {c:?}"),
    }
  }

  #[test]
  fn filesystem_service_is_send_sync() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<FileSystemService>();
  }

  #[test]
  fn filesystem_service_debug_hides_the_implementation() {
    let s = format!("{:?}", FileSystemService::new(TestFileSystem::new()));
    assert!(s.contains("FileSystemService"), "{s}");
    assert!(format!("{:?}", TestFileSystem::new()).contains("TestFileSystem"));
  }

  mod capability {
    use super::*;

    #[test]
    fn read_and_exists_use_the_provided_service() {
      let fs = TestFileSystem::new();
      run_blocking(fs.write(Path::new("cap.txt"), b"via-cap")).unwrap();
      let env = file_system_env(fs);
      let got = run_blocking(read(PathBuf::from("cap.txt")).provide_all(env.clone())).unwrap();
      assert_eq!(got, b"via-cap");
      assert!(run_blocking(exists(PathBuf::from("cap.txt")).provide_all(env.clone())).unwrap());
      assert!(!run_blocking(exists(PathBuf::from("nope.txt")).provide_all(env)).unwrap());
    }

    #[test]
    fn provide_file_system_layer_supplies_the_capability() {
      let fs = TestFileSystem::new();
      run_blocking(fs.write(Path::new("layer.txt"), b"layered")).unwrap();
      let program =
        read::<FileSystemEnv, _>(PathBuf::from("layer.txt")).provide_layer(provide_file_system(fs));
      assert_eq!(run_blocking(program).unwrap(), b"layered");
    }

    #[test]
    fn live_provider_layer_builds_a_live_service() {
      let env = match LiveFileSystemProvider::layer::<FsError>()
        .build()
        .block_on()
      {
        Exit::Success(env) => env,
        Exit::Failure(c) => panic!("layer build failed: {c:?}"),
      };
      let _: &FileSystemService = &env.0;
    }
  }

  mod test_file_system {
    use super::*;

    mod path_policy {
      use super::*;

      #[test]
      fn write_rejects_when_path_contains_dotdot() {
        let fs = TestFileSystem::new();
        let err = run_blocking(fs.write(Path::new("a/../b.txt"), b"x")).unwrap_err();
        assert!(matches!(err, FsError::PathNotAllowed(_)));
      }

      #[cfg(unix)]
      #[test]
      fn write_rejects_when_path_not_utf8() {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt;
        let fs = TestFileSystem::new();
        let p = PathBuf::from(OsString::from_vec(vec![0xFF, 0xFE]));
        let err = run_blocking(fs.write(&p, b"x")).unwrap_err();
        assert!(matches!(err, FsError::PathNotAllowed(_)));
      }
    }

    mod exists {
      use super::*;

      #[test]
      fn false_before_write_true_after() {
        let fs = TestFileSystem::new();
        let p = Path::new("probe.txt");
        assert!(!run_blocking(fs.exists(p)).unwrap());
        run_blocking(fs.write(p, b"x")).unwrap();
        assert!(run_blocking(fs.exists(p)).unwrap());
      }

      #[test]
      fn false_after_remove() {
        let fs = TestFileSystem::new();
        let p = Path::new("gone.txt");
        run_blocking(fs.write(p, b"x")).unwrap();
        run_blocking(fs.remove_file(p)).unwrap();
        assert!(!run_blocking(fs.exists(p)).unwrap());
      }
    }

    mod read_write_round_trip {
      use super::*;

      #[test]
      fn read_returns_bytes_after_write() {
        let fs = TestFileSystem::new();
        let p = Path::new("dir/file.bin");
        run_blocking(fs.write(p, b"payload")).unwrap();
        let got = run_blocking(fs.read(p)).unwrap();
        assert_eq!(got, b"payload");
      }

      #[test]
      fn append_extends_existing_file() {
        let fs = TestFileSystem::new();
        let p = Path::new("log.txt");
        run_blocking(fs.write(p, b"a")).unwrap();
        run_blocking(fs.append(p, b"b")).unwrap();
        assert_eq!(run_blocking(fs.read(p)).unwrap(), b"ab");
      }

      #[test]
      fn append_creates_missing_file() {
        let fs = TestFileSystem::new();
        let p = Path::new("new.txt");
        run_blocking(fs.append(p, b"z")).unwrap();
        assert_eq!(run_blocking(fs.read(p)).unwrap(), b"z");
      }

      #[test]
      fn read_missing_is_not_found() {
        let fs = TestFileSystem::new();
        let err = run_blocking(fs.read(Path::new("missing.txt"))).unwrap_err();
        assert!(matches!(err, FsError::Io(ref e) if e.kind() == std::io::ErrorKind::NotFound));
      }

      #[test]
      fn metadata_len_matches_written_length() {
        let fs = TestFileSystem::new();
        let p = Path::new("sized.bin");
        let data = vec![0_u8; 42];
        run_blocking(fs.write(p, &data)).unwrap();
        let n = run_blocking(fs.metadata_len(p)).unwrap();
        assert_eq!(n, 42);
      }

      #[test]
      fn metadata_len_missing_is_not_found() {
        let fs = TestFileSystem::new();
        let err = run_blocking(fs.metadata_len(Path::new("missing.bin"))).unwrap_err();
        assert!(matches!(err, FsError::Io(_)));
      }

      #[test]
      fn remove_file_deletes_then_read_fails() {
        let fs = TestFileSystem::new();
        let p = Path::new("gone.txt");
        run_blocking(fs.write(p, b"x")).unwrap();
        run_blocking(fs.remove_file(p)).unwrap();
        let err = run_blocking(fs.read(p)).unwrap_err();
        assert!(matches!(err, FsError::Io(_)));
      }

      #[test]
      fn remove_missing_is_not_found() {
        let fs = TestFileSystem::new();
        let err = run_blocking(fs.remove_file(Path::new("never.txt"))).unwrap_err();
        assert!(matches!(err, FsError::Io(_)));
      }
    }

    mod create_dir_all {
      use super::*;

      #[test]
      fn succeeds_without_mutating_store() {
        let fs = TestFileSystem::new();
        run_blocking(fs.create_dir_all(Path::new("any/nested"))).unwrap();
        assert!(!run_blocking(fs.exists(Path::new("any/nested"))).unwrap());
      }
    }

    mod poisoned_mutex {
      use super::*;

      #[test]
      fn read_maps_lock_poison_to_path_not_allowed() {
        let fs = TestFileSystem::new();
        fs.poison_inner_mutex();
        let err = run_blocking(fs.read(Path::new("ok.txt"))).unwrap_err();
        assert!(matches!(err, FsError::PathNotAllowed(_)));
      }

      #[test]
      fn write_maps_lock_poison_to_path_not_allowed() {
        let fs = TestFileSystem::new();
        fs.poison_inner_mutex();
        let err = run_blocking(fs.write(Path::new("ok.txt"), b"x")).unwrap_err();
        assert!(matches!(err, FsError::PathNotAllowed(_)));
      }

      #[test]
      fn append_maps_lock_poison_to_path_not_allowed() {
        let fs = TestFileSystem::new();
        fs.poison_inner_mutex();
        let err = run_blocking(fs.append(Path::new("ok.txt"), b"x")).unwrap_err();
        assert!(matches!(err, FsError::PathNotAllowed(_)));
      }

      #[test]
      fn remove_file_maps_lock_poison_to_path_not_allowed() {
        let fs = TestFileSystem::new();
        fs.poison_inner_mutex();
        let err = run_blocking(fs.remove_file(Path::new("ok.txt"))).unwrap_err();
        assert!(matches!(err, FsError::PathNotAllowed(_)));
      }

      #[test]
      fn metadata_len_maps_lock_poison_to_path_not_allowed() {
        let fs = TestFileSystem::new();
        fs.poison_inner_mutex();
        let err = run_blocking(fs.metadata_len(Path::new("ok.txt"))).unwrap_err();
        assert!(matches!(err, FsError::PathNotAllowed(_)));
      }

      #[test]
      fn exists_maps_lock_poison_to_path_not_allowed() {
        let fs = TestFileSystem::new();
        fs.poison_inner_mutex();
        let err = run_blocking(fs.exists(Path::new("ok.txt"))).unwrap_err();
        assert!(matches!(err, FsError::PathNotAllowed(_)));
      }
    }
  }
}

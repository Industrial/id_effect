use std::path::Path;
use std::sync::Arc;

use id_effect::{Env, run_blocking};
use id_effect_platform::fs::{FileSystem, FileSystemKey, TestFileSystem, read};

#[test]
fn test_fs_roundtrip() {
  let fs = TestFileSystem::new();
  run_blocking(FileSystem::write(&fs, Path::new("a/b.txt"), b"hi"), ()).unwrap();
  let mut env = Env::new();
  env.insert::<FileSystemKey>(Arc::new(fs));
  let bytes = run_blocking(read("a/b.txt".into()), env).unwrap();
  assert_eq!(bytes, b"hi");
}

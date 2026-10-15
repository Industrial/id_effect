mod common;

use std::path::Path;

use common::run_blocking;
use id_effect_platform::fs::{FileSystem, TestFileSystem, file_system_env, read};

#[test]
fn test_fs_roundtrip() {
  let fs = TestFileSystem::new();
  run_blocking(FileSystem::write(&fs, Path::new("a/b.txt"), b"hi")).unwrap();
  let bytes = run_blocking(read("a/b.txt".into()).provide_all(file_system_env(fs))).unwrap();
  assert_eq!(bytes, b"hi");
}

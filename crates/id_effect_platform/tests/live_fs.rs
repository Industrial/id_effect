//! Integration tests for [`id_effect_platform::fs::LiveFileSystem`] (Tokio-backed I/O).

mod common;

use common::run;
use id_effect_platform::error::FsError;
use id_effect_platform::fs::{FileSystem, LiveFileSystem};
use tempfile::tempdir;

#[tokio::test]
async fn live_write_read_append_metadata_remove_round_trip() {
  let dir = tempdir().expect("tempdir");
  let path = dir.path().join("data.bin");
  let fs = LiveFileSystem::new();

  run(fs.write(&path, b"ab")).await.expect("write");
  let n = run(fs.metadata_len(&path)).await.expect("metadata");
  assert_eq!(n, 2);

  run(fs.append(&path, b"c")).await.expect("append");
  let body = run(fs.read(&path)).await.expect("read");
  assert_eq!(body, b"abc");

  run(fs.remove_file(&path)).await.expect("remove");
  let err = run(fs.read(&path)).await.expect_err("missing file");
  assert!(
    matches!(err, FsError::Io(ref e) if e.kind() == std::io::ErrorKind::NotFound),
    "unexpected err: {err:?}"
  );
}

#[tokio::test]
async fn live_create_dir_all_nested_then_write_under() {
  let dir = tempdir().expect("tempdir");
  let nested_dir = dir.path().join("p").join("q");
  let file = nested_dir.join("f.txt");
  let fs = LiveFileSystem::new();

  run(fs.create_dir_all(&nested_dir)).await.expect("mkdir");
  run(fs.write(&file, b"x"))
    .await
    .expect("write under nested");
  let got = run(fs.read(&file)).await.expect("read");
  assert_eq!(got, b"x");
}

#[tokio::test]
async fn live_exists_reflects_file_presence() {
  let dir = tempfile::tempdir().expect("tempdir");
  let path = dir.path().join("probe.txt");
  let fs = LiveFileSystem::new();
  assert!(!run(fs.exists(&path)).await.expect("exists"));
  run(fs.write(&path, b"x")).await.expect("write");
  assert!(run(fs.exists(&path)).await.expect("exists"));
}

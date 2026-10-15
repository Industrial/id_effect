//! Exercise [`id_effect_platform::fs::read`] with the [`LiveFileSystemProvider`] layer.

mod common;

use std::path::PathBuf;

use common::run;
use id_effect_platform::fs::{
  FileSystem, FileSystemEnv, LiveFileSystem, LiveFileSystemProvider, read,
};

#[tokio::test]
async fn read_via_provider_layer_and_live_fs() {
  let dir = tempfile::tempdir().expect("tempdir");
  let rel = PathBuf::from("layer-read.txt");
  let abs = dir.path().join(&rel);
  let fs = LiveFileSystem::new();
  run(fs.write(&abs, b"layered")).await.expect("seed");

  let program = read::<FileSystemEnv, _>(abs).provide_layer(LiveFileSystemProvider::layer());
  let bytes = run(program).await.expect("read via provider");
  assert_eq!(bytes, b"layered");
}

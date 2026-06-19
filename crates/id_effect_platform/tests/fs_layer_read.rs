//! Exercise [`id_effect_platform::fs::read`] with [`LiveFileSystem`] in `R`.

use std::path::PathBuf;

use id_effect::{build_env, provide, run_async};
use id_effect_platform::fs::{FileSystem, LiveFileSystem, LiveFileSystemProvider, read};

#[tokio::test]
async fn read_via_provider_and_live_fs() {
  let dir = tempfile::tempdir().expect("tempdir");
  let rel = PathBuf::from("layer-read.txt");
  let abs = dir.path().join(&rel);
  let fs = LiveFileSystem::new();
  run_async(fs.write(&abs, b"layered"), ())
    .await
    .expect("seed");

  let env = build_env([provide!(LiveFileSystemProvider)]).expect("providers");
  let bytes = run_async(read(abs), env).await.expect("read via provider");
  assert_eq!(bytes, b"layered");
}

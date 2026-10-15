//! m-platform AC: the fs, http and command providers are `next::Layer`s that compose with
//! `merge` and feed one program needing all three capabilities.

mod common;

use std::path::PathBuf;

use common::run;
use id_effect::next::{Effect, HCons, HNil, Layer};
use id_effect_platform::command::{
  CommandSpec, ProcessRuntimeService, TokioProcessRuntimeProvider, spawn_wait,
};
use id_effect_platform::error::{FsError, HttpError, ProcessError};
use id_effect_platform::fs::{
  FileSystem, FileSystemService, LiveFileSystemProvider, TestFileSystem,
};
use id_effect_platform::http::{
  HttpClientService, HttpRequest, ReqwestHttpClientProvider, execute,
};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// One error type for the whole graph.
#[derive(Debug)]
#[allow(dead_code)]
enum AppError {
  Fs(FsError),
  Http(HttpError),
  Process(ProcessError),
}

type PlatformEnv =
  HCons<FileSystemService, HCons<HttpClientService, HCons<ProcessRuntimeService, HNil>>>;

fn platform() -> Layer<PlatformEnv, AppError> {
  LiveFileSystemProvider::layer()
    .merge(ReqwestHttpClientProvider::layer())
    .merge(TokioProcessRuntimeProvider::layer())
}

#[cfg(unix)]
#[tokio::test]
async fn one_program_uses_the_merged_fs_http_and_command_layers() {
  let server = MockServer::start().await;
  Mock::given(method("GET"))
    .and(path("/payload"))
    .respond_with(ResponseTemplate::new(200).set_body_string("from-http"))
    .mount(&server)
    .await;
  let dir = tempfile::tempdir().expect("tempdir");
  let file = dir.path().join("payload.txt");
  let url = format!("{}/payload", server.uri());

  let f1 = file.clone();
  let program: Effect<(Vec<u8>, bool), AppError, PlatformEnv> =
    execute::<PlatformEnv, _>(HttpRequest::get(url))
      .map_err(AppError::Http)
      .flat_map(move |resp| {
        id_effect::next::need::<FileSystemService>()
          .flat_map(move |fs| fs.write(&f1, &resp.body).narrow())
          .map_err(AppError::Fs)
      })
      .flat_map(move |()| {
        id_effect_platform::fs::read::<PlatformEnv, _>(file).map_err(AppError::Fs)
      })
      .flat_map(|bytes| {
        spawn_wait::<PlatformEnv, _>(CommandSpec::new("true"))
          .map_err(AppError::Process)
          .map(move |st| (bytes, st.success()))
      });

  let (bytes, ok) = run(program.provide_layer(platform()))
    .await
    .expect("program");
  assert_eq!(bytes, b"from-http");
  assert!(ok);
}

#[tokio::test]
async fn a_test_double_layer_replaces_the_live_one() {
  let fs = TestFileSystem::new();
  common::run(fs.write(std::path::Path::new("k.txt"), b"double"))
    .await
    .expect("seed");
  let program =
    id_effect_platform::fs::read::<HCons<FileSystemService, HNil>, _>(PathBuf::from("k.txt"))
      .provide_layer(id_effect_platform::fs::provide_file_system(fs));
  assert_eq!(run(program).await.expect("read"), b"double");
}

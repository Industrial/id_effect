//! Stores statements in a local directory through the `ObjectStorage` capability: a streamed
//! upload, a listing, a streamed download and a signed download URL that the serving side
//! verifies. Provide an `S3Store` instead (feature `s3`) and the program is unchanged; its
//! signed URLs are then S3 presigned URLs.
//!
//! `cargo run -p id_effect_storage --example 020_storage`

use std::time::Duration;

use bytes::Bytes;
use id_effect::{Effect, Exit, Stream};
use id_effect_storage::{
  LocalStore, ObjectKey, ObjectStorageEnv, SignedMethod, StorageError, UrlSigner, get_stream, list,
  put_stream, signed_url,
};

/// Uploads a statement line by line, then lists the month and signs a download link.
fn program(month: ObjectKey, key: ObjectKey) -> Effect<String, StorageError, ObjectStorageEnv> {
  let lines = Stream::from_vec(
    [
      "date,amount\n",
      "2026-10-01,120.00\n",
      "2026-10-15,-42.50\n",
    ]
    .map(|line| Bytes::from_static(line.as_bytes()))
    .to_vec(),
  );
  put_stream::<ObjectStorageEnv, _>(key.clone(), lines)
    .flat_map(move |put| {
      println!("stored {} ({} bytes)", put.key, put.size);
      list(Some(month)).flat_map(|objects| objects.run_collect().narrow())
    })
    .flat_map({
      let key = key.clone();
      move |objects| {
        for meta in &objects {
          println!("listed {} ({} bytes)", meta.key, meta.size);
        }
        get_stream(key).flat_map(|body| body.run_collect().narrow())
      }
    })
    .flat_map(move |chunks| {
      println!(
        "read back {} bytes",
        chunks.iter().map(Bytes::len).sum::<usize>()
      );
      signed_url(SignedMethod::Get, key, Duration::from_secs(300)).map(|url| url.to_string())
    })
}

fn main() {
  let root = std::env::temp_dir().join(format!("id_effect_storage_{}", std::process::id()));
  let signer = UrlSigner::new(
    *b"example-secret-0123456789abcdef!",
    "https://files.example.com/statements/".parse().unwrap(),
  )
  .expect("a valid signer");

  let store = LocalStore::new(&root)
    .expect("a writable directory")
    .with_url_signer(signer.clone());
  let month = ObjectKey::new("acct-1/2026/10").unwrap();
  let key = ObjectKey::new("acct-1/2026/10/statement.csv").unwrap();
  let run = program(month, key.clone()).provide_all(id_effect_storage::object_storage_env(store));

  match run.block_on() {
    Exit::Success(url) => {
      println!("signed URL {url}");
      // The service behind the base URL checks the signature and expiry before serving.
      let url = url.parse().unwrap();
      assert_eq!(signer.verify(SignedMethod::Get, &url), Ok(key));
      println!("verified: the URL authorizes GET on the statement");
    }
    Exit::Failure(cause) => panic!("storage failed: {cause:?}"),
  }
  let _ = std::fs::remove_dir_all(root);
}

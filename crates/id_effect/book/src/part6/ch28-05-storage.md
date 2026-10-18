# Object storage (`id_effect_storage`)

`id_effect_storage` gives a service one interface for files and blobs — uploads, exports, avatars, statements — whatever holds them: a directory on the local disk, memory in tests, or an S3-compatible bucket (AWS S3, Cloudflare R2, Ceph, SeaweedFS, ...) in production. Uploads and downloads stream without holding the object in memory, and every backend issues signed, expiring URLs, so clients can fetch or upload an object directly.

```toml
id_effect_storage = { version = "0.10", features = ["s3"] }  # `s3` only for the S3 backend
```

It is built on the [`object_store`](https://crates.io/crates/object_store) crate (the Apache Arrow project's store abstraction). Every operation is a lazy `Effect<_, StorageError>` or a `Stream<_, StorageError>`; `StorageError` separates `NotFound`, `AlreadyExists`, `PermissionDenied` (credentials or policy), `InvalidKey`, `SigningUnavailable`, `Unsupported`, `Config` (a bad root directory, bucket or secret) and `Backend` (I/O, network, protocol).

## The capability and its backends

`ObjectStore` is the capability trait: `put`, `put_stream`, `get`, `get_stream`, `head`, `exists`, `delete`, `list` and `signed_url`. A program asks for it as the `ObjectStorage` service (`ObjectStorageEnv`) through free functions of the same names, and a layer decides which backend it gets:

| Backend | Layer | Use |
|---|---|---|
| `MemoryStore` | `MemoryStore::layer()` | tests and examples; clones share their objects |
| `LocalStore` | `LocalStore::layer(root)` | a directory: one file per object, written to a staging file and renamed into place |
| `S3Store` (feature `s3`) | `S3Store::layer(S3Config)` | an S3-compatible bucket, with SigV4 presigned URLs |

All three behave the same through the capability: `get` and `head` of a missing key fail with `StorageError::NotFound`, `delete` of a missing key succeeds, and `list(prefix)` matches whole segments (`docs` covers `docs/a.txt`, not `docsx/c`). Your own backend (a read-only view, a caching wrapper) is any type implementing `ObjectStore`, installed with `provide_object_storage(store)`.

Keys are `ObjectKey`s, validated once so they mean the same on every backend: 1 to 1024 bytes of `/`-separated segments, none empty, `.` or `..`, and no control characters. A key can therefore never climb out of a `LocalStore`'s root.

```rust
use bytes::Bytes;
use id_effect::{Exit, Stream};
use id_effect_storage::{
    KeyError, MemoryStore, ObjectKey, ObjectStorageEnv, StorageError, exists, get, head, put_stream,
};

assert_eq!(ObjectKey::new("../etc/passwd"), Err(KeyError::DotSegment));
let key = ObjectKey::new("invoices/2026/inv-0042.pdf").unwrap();
assert_eq!(key.file_name(), "inv-0042.pdf");

let body = Stream::<Bytes, StorageError>::from_vec(vec![
    Bytes::from_static(b"%PDF-1.7 "),
    Bytes::from_static(b"..."),
]);
let program = put_stream::<ObjectStorageEnv, _>(key.clone(), body)
    .flat_map({
        let key = key.clone();
        move |put| head(key).map(move |meta| (put.size, meta.size))
    })
    .flat_map({
        let key = key.clone();
        move |sizes| get(key).map(move |bytes| (sizes, bytes))
    })
    .flat_map(move |found| exists(ObjectKey::new("invoices/none.pdf").unwrap()).map(move |e| (found, e)));

let Exit::Success((((put, stored), bytes), other)) = program.provide_layer(MemoryStore::layer()).block_on() else {
    panic!("storage failed")
};
assert_eq!((put, stored), (12, 12));
assert_eq!(bytes, "%PDF-1.7 ...");
assert!(!other);
```

## Streaming without buffering

`put_stream(key, body)` takes the body as a `Stream<Bytes, StorageError>` (an HTTP request body, a generated export) and pulls it one item at a time. At most one **part** plus one body item is in memory: a body shorter than a part is sent with one `PUT`, a longer one as a multipart upload with one part in flight. If the body fails — the client went away — the upload is aborted and no object is left behind. `get_stream(key)` yields the object as the backend reads it (8 KiB file reads, network chunks for S3); the object is opened on the first pull.

The part size is 8 MiB unless set with `with_part_size`. S3 requires every part but the last to be at least 5 MiB, so `S3Store::with_part_size` never goes below `S3Store::MIN_PART_SIZE`; the local and in-memory stores accept any size (tests use 16 bytes to exercise multipart paths cheaply).

## S3 and S3-compatible services

`S3Config` names the bucket and, for services other than AWS, the endpoint. Nothing connects until the first operation, so a wrong endpoint shows up as a `Backend` error then, not at startup:

```rust
use id_effect_storage::{S3Config, S3Store};

let config = S3Config::new("statements")
    .with_region("eu-central-1")
    .with_endpoint("http://localhost:8333") // S3-compatible service; omit for AWS
    .with_allow_http(true)                   // plain HTTP for a local endpoint only
    .with_credentials("id-effect", "id-effect-secret");
let store = S3Store::new(&config).unwrap().with_part_size(16 * 1024 * 1024);
assert_eq!((store.bucket(), store.part_size()), ("statements", 16 * 1024 * 1024));
```

`S3Config::from_env(bucket)` starts from the `AWS_*` environment variables (`AWS_REGION`, `AWS_ENDPOINT`, `AWS_ACCESS_KEY_ID`, `AWS_SECRET_ACCESS_KEY`, ...) and the `with_*` settings override them. Requests are path-style (`host/bucket/key`) unless `with_virtual_hosted_style(true)`. The secret key never appears in `Debug` output. `S3Store` reads and writes objects; it does not create buckets, which belong to your infrastructure code.

## Signed URLs

`signed_url(method, key, expires_in)` returns a URL that lets its holder `GET` or `PUT` one object until it expires, without credentials — the way to let a browser upload an avatar or download an export without streaming it through your service:

- **S3** URLs are SigV4 **presigned** URLs, signed locally (no request), valid for at most 7 days. The S3 service checks the signature, the method and the expiry.
- **Local and in-memory** stores have no server of their own, so they sign with a `UrlSigner`: an HMAC-SHA256 over the method, key and expiry, under a base URL **your** service serves. The route behind that URL calls `UrlSigner::verify` and serves (or accepts) the object only if it passes. Without a signer, `signed_url` fails with `StorageError::SigningUnavailable`.

```rust
use std::time::{Duration, SystemTime};

use id_effect_storage::{LocalStore, ObjectKey, ObjectStore, SignatureError, SignedMethod, UrlSigner};

let signer = UrlSigner::new(
    *b"a-32-byte-secret-from-the-vault!",
    "https://files.example.com/o/".parse().unwrap(),
)
.unwrap();
let dir = tempfile::tempdir().unwrap();
let store = LocalStore::new(dir.path()).unwrap().with_url_signer(signer.clone());
let key = ObjectKey::new("exports/2026-10.csv").unwrap();

let id_effect::Exit::Success(url) =
    store.signed_url(SignedMethod::Get, &key, Duration::from_secs(300)).block_on()
else {
    panic!("signing failed")
};
// In the route serving https://files.example.com/o/...:
assert_eq!(signer.verify(SignedMethod::Get, &url), Ok(key));
assert_eq!(signer.verify(SignedMethod::Put, &url), Err(SignatureError::BadSignature));
let later = SystemTime::now() + Duration::from_secs(600);
assert_eq!(signer.verify_at(SignedMethod::Get, &url, later), Err(SignatureError::Expired));
```

Editing the key or the expiry in the URL invalidates the signature; a URL issued for `GET` does not authorize a `PUT`.

## Testing against S3

Unit tests use `MemoryStore` (or a `LocalStore` on a temporary directory). For the S3 backend itself, `id_effect_test` has an S3-compatible container, `S3`, used like its other images:

```rust,no_run
use id_effect::{Effect, need};
use id_effect_test::{ContainerError, ContainerImage, S3, S3Container};

let endpoint: Effect<String, ContainerError, id_effect::caps!(S3Container)> =
    need::<S3Container>().map(|s3| s3.endpoint());
let endpoint = endpoint.provide_layer(S3::new().layer());
# let _ = endpoint;
```

The image is **SeaweedFS 4.48** (`chrislusf/seaweedfs:4.48@sha256:4e61d15fd35994cb1e43e1e553dff106794841fd9a99ade2fc8c8bfce4d7872d`), pinned by the digest of its multi-arch index. MinIO's community images, the usual choice, could no longer be pulled anonymously from Docker Hub or Quay when the image was chosen (October 2026); SeaweedFS is Apache-2.0, publishes release tags on Docker Hub, and implements what the tests exercise — SigV4 with presigned-URL expiry, multipart uploads and `ListObjectsV2`. The container starts in a few seconds with one identity (`s3.access_key_id()`, `s3.secret_access_key()`) and no buckets: a test creates its bucket with a signed `PUT /<bucket>`.

`crates/id_effect_storage/tests/s3.rs` runs `S3Store` against it: the round trip and listing semantics above, a 12 MiB streamed upload sent as three parts and read back as a stream, an aborted upload leaving nothing behind, presigned `PUT` and `GET` URLs used with a plain HTTP client (refused without the signature, with a tampered one, for the other method, and once expired) and a wrong secret refused as `PermissionDenied`. Without Docker those tests print `SKIP` and pass; with `ID_EFFECT_REQUIRE_DOCKER=1` (the CI `containers-integration` job) a missing Docker daemon fails them.

The runnable example `cargo run -p id_effect_storage --example 020_storage` stores, lists, streams back and signs a statement in a local directory, no Docker needed.

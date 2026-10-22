//! The `Notes` RPC service: two procedures declared with `#[rpc::service]`, both behind the
//! [`ServiceToken`] guard, and the in-memory [`NoteStore`] that implements them.
//!
//! `#[rpc::service]` generates [`NotesService`] (the server side: `NotesService::mount` puts the
//! procedures on a typed router under `POST /rpc`) and [`NotesClient`] (the typed Rust client
//! the pages render with, over any `RpcTransport`).

use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use axum::http::request::Parts;
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::IntoResponse;
use id_effect::{DynEnv, Effect, Never, fail, succeed};
use id_effect_axum::route::GuardFuture;
use id_effect_config::SecretString;
use id_effect_rpc as rpc;
use id_effect_rpc::registry::RouteGuard;

use crate::app::Note;

/// The longest note title the service accepts, in characters.
pub const MAX_TITLE: usize = 120;

/// The input of `Notes.add`.
#[derive(Clone, Debug, PartialEq, Eq, id_effect::Schema, serde::Serialize, serde::Deserialize)]
pub struct NewNote {
  /// The note's text: 1 to [`MAX_TITLE`] characters.
  pub title: String,
}

/// Why `Notes.add` refused a note (a typed error union: the client gets the variant back).
#[derive(Debug, PartialEq, Eq, rpc::RpcErrorUnion)]
pub enum NoteError {
  /// The title is empty or longer than `max` characters.
  #[rpc(status = 422)]
  InvalidTitle {
    /// [`MAX_TITLE`].
    max: i64,
  },
}

/// Notes for the server that renders the pages. Service to service: every procedure needs the
/// [`ServiceToken`], which the browser never has.
#[rpc::service(version = 1)]
pub trait Notes {
  /// Every note, oldest first.
  #[query]
  #[guard(ServiceToken)]
  fn list(&self) -> Effect<Vec<Note>, Never, DynEnv>;

  /// Adds a note and returns it with its id.
  #[mutation]
  #[guard(ServiceToken)]
  fn add(&self, note: NewNote) -> Effect<Note, NoteError, DynEnv>;
}

/// The notes, in memory (a real service keeps them in a database through `id_effect_sql`).
#[derive(Clone, Debug, Default)]
pub struct NoteStore {
  notes: Arc<Mutex<Vec<Note>>>,
  next_id: Arc<AtomicI64>,
}

impl NoteStore {
  /// An empty store.
  pub fn new() -> Self {
    Self::default()
  }

  /// A store holding `titles`, in order.
  pub fn with_titles<I, S>(titles: I) -> Self
  where
    I: IntoIterator<Item = S>,
    S: Into<String>,
  {
    let store = Self::new();
    for title in titles {
      store.insert(title.into());
    }
    store
  }

  /// The notes the demo server starts with.
  pub fn seeded() -> Self {
    Self::with_titles([
      "Rendered by Leptos on the id_effect host",
      "Loaded through the generated NotesClient",
      "Hydrated in the browser by the wasm build",
    ])
  }

  /// Every note, oldest first.
  pub fn snapshot(&self) -> Vec<Note> {
    self
      .notes
      .lock()
      .unwrap_or_else(PoisonError::into_inner)
      .clone()
  }

  fn insert(&self, title: String) -> Note {
    let note = Note {
      id: self.next_id.fetch_add(1, Ordering::Relaxed) + 1,
      title,
    };
    self
      .notes
      .lock()
      .unwrap_or_else(PoisonError::into_inner)
      .push(note.clone());
    note
  }
}

impl Notes for NoteStore {
  fn list(&self) -> Effect<Vec<Note>, Never, DynEnv> {
    succeed(self.snapshot())
  }

  fn add(&self, note: NewNote) -> Effect<Note, NoteError, DynEnv> {
    let title = note.title.trim();
    let length = title.chars().count();
    if length == 0 || length > MAX_TITLE {
      return fail(NoteError::InvalidTitle {
        max: MAX_TITLE as i64,
      });
    }
    succeed(self.insert(title.to_owned()))
  }
}

/// The token the `Notes` procedures expect, as a request extension. [`crate::server::app`] adds
/// it to every request with `axum::Extension`; without it [`ServiceToken`] admits no one
/// (fail closed).
#[derive(Clone, Debug)]
pub struct ExpectedToken(Arc<SecretString>);

impl ExpectedToken {
  /// Expect `token`, sent as a bearer token in the `Authorization` header.
  pub fn new(token: SecretString) -> Self {
    Self(Arc::new(token))
  }

  fn admits(&self, presented: &str) -> bool {
    // `SecretString` compares in constant time.
    *self.0 == SecretString::new(presented.to_owned())
  }
}

/// The guard of every `Notes` procedure: admits a request whose `Authorization: Bearer` token
/// equals the [`ExpectedToken`] extension, and answers anything else with `401` and
/// `WWW-Authenticate: Bearer` before the procedure runs.
#[derive(Clone, Copy, Debug, Default)]
pub struct ServiceToken;

/// The policy name [`ServiceToken`] records in the contract and the access report.
pub const SERVICE_TOKEN_POLICY: &str = "notes.service-token";

impl RouteGuard for ServiceToken {
  fn policy(&self) -> &str {
    SERVICE_TOKEN_POLICY
  }

  fn check<'a>(&'a self, parts: &'a Parts) -> GuardFuture<'a> {
    let presented = parts
      .headers
      .get(header::AUTHORIZATION)
      .and_then(|value| value.to_str().ok())
      .and_then(|value| value.strip_prefix("Bearer "));
    let admitted = match (parts.extensions.get::<ExpectedToken>(), presented) {
      (Some(expected), Some(token)) => expected.admits(token),
      _ => false,
    };
    Box::pin(async move {
      if admitted {
        Ok(())
      } else {
        Err(
          (
            StatusCode::UNAUTHORIZED,
            [(header::WWW_AUTHENTICATE, HeaderValue::from_static("Bearer"))],
          )
            .into_response(),
        )
      }
    })
  }
}

//! The Leptos UI, compiled twice: rendered to HTML on the server (`ssr`) and hydrated in the
//! browser (`hydrate`). Both builds must produce the same view tree, so everything here is
//! shared; only [`load_notes`] differs.

use leptos::prelude::*;
use serde::{Deserialize, Serialize};

/// A note, as the `Notes` RPC service returns it and the page shows it.
///
/// Shared by both builds: the server decodes it from the RPC response and serializes it into the
/// page (serde); the browser deserializes it from there while hydrating. On the server it also
/// has an `id_effect` `Schema`, the wire schema of the RPC procedures.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(id_effect::Schema))]
pub struct Note {
  /// Assigned by the service, increasing.
  pub id: i64,
  /// What the note says (untrusted text: Leptos escapes it when rendering).
  pub title: String,
}

/// What the page shows: the notes, or a message saying why they are missing.
pub type NotesView = Result<Vec<Note>, String>;

/// Shown when the RPC call fails; the reason itself stays in the server log.
pub const NOTES_UNAVAILABLE: &str = "The notes service is unavailable right now.";

/// Load the notes for this render (server): through the generated RPC client that
/// [`crate::server`] provides as context.
#[cfg(feature = "ssr")]
fn load_notes() -> impl std::future::Future<Output = NotesView> + Send + 'static {
  // Read the context synchronously, while the resource's owner is current.
  let api = use_context::<crate::server::NotesApi>();
  async move {
    match api {
      Some(api) => api.list().await,
      None => Err(NOTES_UNAVAILABLE.to_owned()),
    }
  }
}

/// Load the notes (browser): never called while hydrating — the server's result travels in the
/// page — and nothing in the page asks for a reload.
#[cfg(not(feature = "ssr"))]
async fn load_notes() -> NotesView {
  Err(NOTES_UNAVAILABLE.to_owned())
}

/// The HTML document around [`App`]: `HydrationScripts` loads `/pkg/leptos_ssr.js` and its wasm
/// and calls [`crate::hydrate`]. Its inline `<script>` carries this request's CSP nonce, which
/// [`crate::server`] hands to Leptos.
#[cfg(feature = "ssr")]
pub fn shell(options: leptos::config::LeptosOptions) -> impl IntoView {
  view! {
    <!DOCTYPE html>
    <html lang="en">
      <head>
        <meta charset="utf-8" />
        <meta name="viewport" content="width=device-width, initial-scale=1" />
        <title>"Notes — id_effect + Leptos"</title>
        <link rel="stylesheet" href=crate::server::STYLESHEET_PATH />
        <HydrationScripts options />
      </head>
      <body>
        <App />
      </body>
    </html>
  }
}

/// The page: the notes from the RPC service (resolved before the HTML is sent) and a counter
/// that only works once the page is hydrated.
#[component]
pub fn App() -> impl IntoView {
  let notes = Resource::new(|| (), |()| load_notes());
  view! {
    <main>
      <h1>"Notes"</h1>
      <p class="lead">
        "Rendered on the server by Leptos with data from an id_effect RPC service, then hydrated in the browser."
      </p>
      <Suspense fallback=|| view! { <p>"Loading notes…"</p> }>
        {move || Suspend::new(async move {
          match notes.await {
            Ok(notes) => view! { <NoteList notes /> }.into_any(),
            Err(reason) => view! { <p class="error" role="alert">{reason}</p> }.into_any(),
          }
        })}
      </Suspense>
      <Counter />
    </main>
  }
}

/// The notes as a list (or a line saying there are none).
#[component]
pub fn NoteList(
  /// The notes to show, in order.
  notes: Vec<Note>,
) -> impl IntoView {
  if notes.is_empty() {
    return view! { <p id="notes-empty">"No notes yet."</p> }.into_any();
  }
  view! {
    <ul id="notes">
      {notes
        .into_iter()
        .map(|note| view! { <li data-id=note.id>{note.title}</li> })
        .collect_view()}
    </ul>
  }
  .into_any()
}

/// A button counting its clicks: server-rendered as "Clicked 0 times", interactive only after
/// hydration — the visible proof that the wasm half attached.
#[component]
pub fn Counter() -> impl IntoView {
  let (count, set_count) = signal(0u32);
  view! {
    <button id="counter" type="button" on:click=move |_| set_count.update(|n| *n += 1)>
      "Clicked " {count} " times"
    </button>
  }
}

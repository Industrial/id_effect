//! 095 — an htmx todo list (feature `ui`): maud views, components, one layout for the page and
//! fragments for htmx, a form bound to `Schema` errors, the vendored htmx served by the app, and
//! the secure stack (CSRF token on every htmx request, `script-src 'self'`).
//!
//! ```text
//! cargo run -p id_effect_axum --features ui --example 095_htmx_todo               # http://127.0.0.1:3002
//! cargo run -p id_effect_axum --features ui --example 095_htmx_todo -- --self-test
//! ```
//!
//! The CSRF cookie is `__Host-` and `Secure`; browsers accept it on `http://localhost` (a secure
//! context) — open <http://localhost:3002>, not the IP address.

use std::convert::Infallible;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use axum::body::Body;
use axum::extract::{Path, Request, State};
use axum::http::{HeaderMap, Method, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{MethodRouter, delete, post};
use base64::Engine;
use http_body_util::BodyExt;
use id_effect::schema::{self, HasSchema, Schema};
use id_effect::{Effect, succeed, sync};
use id_effect_axum::route::{Endpoint, Input, Public, Route, RouteInfo, Router};
use id_effect_axum::server::security::{CookieKey, SecurityLayer};
use id_effect_axum::ui::htmx::{self, HxRedirect, HxReswap, HxRetarget, HxTrigger};
use id_effect_axum::ui::{
  BasicLayout, Component, FormState, HtmlForm, LayoutLayer, Markup, Page, Slots, html,
};
use id_effect_axum::{ConnectionLimits, execute, serve_listener};
use sha2::{Digest, Sha384};
use tower::ServiceExt;

#[derive(Clone, Debug)]
struct Todo {
  id: u64,
  title: String,
  tag: Option<String>,
}

#[derive(Clone, Default)]
struct App {
  todos: Arc<Mutex<Vec<Todo>>>,
  next_id: Arc<AtomicU64>,
}

impl App {
  fn snapshot(&self) -> Vec<Todo> {
    self
      .todos
      .lock()
      .unwrap_or_else(PoisonError::into_inner)
      .clone()
  }
}

/// The new-todo form: a title (1–80 characters) and an optional tag (≤ 20).
struct NewTodo;

impl HasSchema for NewTodo {
  type A = (String, Option<String>);
  type I = (String, Option<String>);
  type E = ();

  fn schema() -> Schema<Self::A, Self::I> {
    schema::struct_(
      "title",
      schema::max_length(schema::non_empty(schema::string()), 80),
      "tag",
      schema::optional(schema::max_length(schema::string(), 20)),
    )
  }
}

/// One todo; its `done` button deletes it and swaps the list.
struct TodoItem<'a>(&'a Todo);

impl Component for TodoItem<'_> {
  fn render(&self, _slots: &Slots) -> Markup {
    let todo = self.0;
    html! {
      li id=(format!("todo-{}", todo.id)) {
        span.title { (todo.title) }
        @if let Some(tag) = &todo.tag { " " span.tag { (tag) } }
        " "
        button hx-delete=(format!("/todos/{}", todo.id)) hx-target="#todos" hx-swap="outerHTML" {
          "done"
        }
      }
    }
  }
}

/// The list; its slot is what to show when it is empty.
struct TodoList<'a>(&'a [Todo]);

impl Component for TodoList<'_> {
  fn render(&self, slots: &Slots) -> Markup {
    html! {
      ul #todos {
        @for todo in self.0 { (TodoItem(todo).alone()) }
        @if self.0.is_empty() { li.empty { (slots.children()) } }
      }
    }
  }
}

fn list(todos: &[Todo]) -> Markup {
  html! { (TodoList(todos).with(Slots::new().child("Nothing to do."))) }
}

fn todo_form(state: &FormState) -> Markup {
  html! {
    form #new-todo hx-post="/todos" hx-target="#todos" hx-swap="outerHTML" {
      (state.error_summary())
      (state.field("title", "Title").required().placeholder("Buy milk"))
      (state.field("tag", "Tag"))
      button { "Add" }
    }
  }
}

/// `GET /` (a typed route): the page, or only its content for htmx.
fn index(app: &mut App, _: Input) -> Effect<Page, Infallible, App> {
  let todos = app.snapshot();
  succeed(Page::new(
    "Todos",
    html! {
      h1 { "Todos" }
      (todo_form(&FormState::new()))
      (list(&todos))
      button hx-post="/todos/clear" { "Clear all" }
    },
  ))
}

fn add(app: &mut App, title: String, tag: Option<String>) -> Effect<Vec<Todo>, Infallible, App> {
  let app = app.clone();
  sync(move || {
    let id = app.next_id.fetch_add(1, Ordering::Relaxed) + 1;
    let mut todos = app.todos.lock().unwrap_or_else(PoisonError::into_inner);
    todos.push(Todo { id, title, tag });
    todos.clone()
  })
}

/// `POST /todos`: add and answer the list, or re-render the form with its errors (`422`,
/// retargeted at the form).
async fn create(State(app): State<App>, form: HtmlForm<NewTodo>) -> Response {
  match form.into_result() {
    Ok(((title, tag), _)) => {
      let tag = tag.filter(|t| !t.is_empty());
      execute(State(app), move |app| {
        add(app, title, tag).map(|todos| (HxTrigger::event("todo-added"), list(&todos)))
      })
      .await
    }
    Err(state) => (
      StatusCode::UNPROCESSABLE_ENTITY,
      HxRetarget("#new-todo".into()),
      HxReswap::OuterHtml,
      todo_form(&state),
    )
      .into_response(),
  }
}

/// `DELETE /todos/{id}`: remove and answer the list.
async fn remove(State(app): State<App>, Path(id): Path<u64>) -> Response {
  execute(State(app), move |app| {
    let app = app.clone();
    sync::<_, Infallible, App>(move || {
      let mut todos = app.todos.lock().unwrap_or_else(PoisonError::into_inner);
      todos.retain(|t| t.id != id);
      list(&todos)
    })
  })
  .await
}

/// `POST /todos/clear`: clear and send the browser back to `/` (same-origin `HX-Redirect`).
async fn clear(State(app): State<App>) -> Response {
  app
    .todos
    .lock()
    .unwrap_or_else(PoisonError::into_inner)
    .clear();
  match HxRedirect::to("/") {
    Ok(redirect) => (redirect, ()).into_response(),
    Err(refused) => (StatusCode::INTERNAL_SERVER_ERROR, refused.to_string()).into_response(),
  }
}

/// A plain Axum handler as a typed endpoint anyone may call (the typed router serves only routes
/// that declare their access).
fn public(method: Method, path: &str, handler: MethodRouter<App>) -> Endpoint<App, Public> {
  Endpoint::from_parts(RouteInfo::new(method, path), handler).public()
}

fn app(state: App) -> axum::Router {
  Router::new()
    .route(Route::get("/").public().handler(index))
    .route(public(Method::POST, "/todos", post(create)))
    .route(public(Method::DELETE, "/todos/{id}", delete(remove)))
    .route(public(Method::POST, "/todos/clear", post(clear)))
    .merge(htmx::asset_route())
    .with_state(state)
    .into_axum()
    .layer(LayoutLayer::new(BasicLayout::new().site_name("095")))
    // Outside the layout: the layout reads the CSRF token this layer issues.
    .layer(SecurityLayer::recommended(CookieKey::generate()))
}

async fn send(app: &axum::Router, req: Request) -> (StatusCode, HeaderMap, String) {
  let res = app.clone().oneshot(req).await.unwrap();
  let (parts, body) = res.into_parts();
  let body = body.collect().await.unwrap().to_bytes();
  (
    parts.status,
    parts.headers,
    String::from_utf8_lossy(&body).into_owned(),
  )
}

/// The masked token inside `hx-headers="{&quot;x-csrf-token&quot;:&quot;…&quot;}"`.
fn token_in(page: &str) -> String {
  let marker = "x-csrf-token&quot;:&quot;";
  let start = page.find(marker).expect("hx-headers") + marker.len();
  let len = page[start..].find("&quot;").unwrap();
  page[start..start + len].to_owned()
}

fn htmx_post(uri: &str, cookie: &str, token: &str, body: &'static str) -> Request {
  Request::post(uri)
    .header(header::COOKIE, cookie)
    .header("x-csrf-token", token)
    .header("hx-request", "true")
    .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
    .body(Body::from(body))
    .unwrap()
}

async fn self_test() {
  let app = app(App::default());

  // The full page: layout, htmx from this origin, CSP `script-src 'self'`, CSRF cookie + token.
  let (status, headers, page) = send(&app, Request::get("/").body(Body::empty()).unwrap()).await;
  assert_eq!(status, StatusCode::OK);
  assert!(page.starts_with("<!DOCTYPE html>"), "{page}");
  assert!(page.contains(htmx::HTMX_PATH) && page.contains(htmx::HTMX_INTEGRITY));
  let csp = headers[header::CONTENT_SECURITY_POLICY].to_str().unwrap();
  assert!(csp.contains("script-src 'self'"), "{csp}");
  let cookie = headers[header::SET_COOKIE].to_str().unwrap();
  let cookie = cookie.split(';').next().unwrap().to_owned();
  let token = token_in(&page);

  // The same route as an htmx fragment: no layout.
  let req = Request::get("/")
    .header("hx-request", "true")
    .body(Body::empty())
    .unwrap();
  let (_, _, fragment) = send(&app, req).await;
  assert!(fragment.starts_with("<h1>Todos</h1>"), "{fragment}");

  // The vendored script, byte-for-byte what the README pins.
  let req = Request::get(htmx::HTMX_PATH).body(Body::empty()).unwrap();
  let res = app.clone().oneshot(req).await.unwrap();
  let js = res.into_body().collect().await.unwrap().to_bytes();
  let sri = base64::engine::general_purpose::STANDARD.encode(Sha384::digest(&js));
  assert_eq!(format!("sha384-{sri}"), htmx::HTMX_INTEGRITY);

  // No token, no write.
  let forged = Request::post("/todos")
    .header(header::COOKIE, &cookie)
    .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
    .body(Body::from("title=forged"))
    .unwrap();
  assert_eq!(send(&app, forged).await.0, StatusCode::FORBIDDEN);

  // A valid todo: escaped in the list, `todo-added` triggered.
  let req = htmx_post(
    "/todos",
    &cookie,
    &token,
    "title=%3Cb%3Emilk%3C%2Fb%3E&tag=shop",
  );
  let (status, headers, list) = send(&app, req).await;
  assert_eq!(status, StatusCode::OK, "{list}");
  assert_eq!(headers["hx-trigger"], "todo-added");
  assert!(list.contains("&lt;b&gt;milk&lt;/b&gt;"), "{list}");

  // An invalid one: the form again, value kept, error bound to `title`.
  let req = htmx_post(
    "/todos",
    &cookie,
    &token,
    "title=&tag=this-tag-is-far-too-long",
  );
  let (status, headers, form) = send(&app, req).await;
  assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
  assert_eq!(headers["hx-retarget"], "#new-todo");
  assert!(
    form.contains("data-message-key=\"schema-must-not-be-empty\""),
    "{form}"
  );
  assert!(
    form.contains("value=\"this-tag-is-far-too-long\""),
    "{form}"
  );

  // Done: deleted, the list is empty again.
  let req = Request::delete("/todos/1")
    .header(header::COOKIE, &cookie)
    .header("x-csrf-token", &token)
    .header("hx-request", "true")
    .body(Body::empty())
    .unwrap();
  let (status, _, list) = send(&app, req).await;
  assert_eq!(status, StatusCode::OK);
  assert!(list.contains("Nothing to do."), "{list}");

  // Clear: a same-origin client redirect.
  let req = htmx_post("/todos/clear", &cookie, &token, "");
  let (_, headers, _) = send(&app, req).await;
  assert_eq!(headers["hx-redirect"], "/");

  println!("095_htmx_todo self-test: ok (page, fragment, htmx asset, CSRF, form errors, redirect)");
}

#[tokio::main]
async fn main() -> std::io::Result<()> {
  if std::env::args().any(|a| a == "--self-test") {
    self_test().await;
    return Ok(());
  }
  let listener = tokio::net::TcpListener::bind("127.0.0.1:3002").await?;
  println!("http://localhost:{}/", listener.local_addr()?.port());
  serve_listener(
    listener,
    app(App::default()),
    ConnectionLimits::new(),
    std::future::pending(),
  )
  .await
}

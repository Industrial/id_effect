# Server-rendered UI: maud views and htmx

`id_effect_axum` renders HTML on the server with feature `ui`: [maud](https://maud.lambda.xyz/)
templates checked at compile time, components with slots, one layout for the full page and the
fragments htmx asks for, htmx helpers, and HTML forms that show `Schema` decode errors next to the
fields. There is no bundler and no npm: htmx is one vendored, hash-pinned file the app serves
itself.

```toml
[dependencies]
id_effect_axum = { version = "0.13", features = ["ui"] }
# `html!` expands to `extern crate maud;`: templates need maud itself, at the pinned version.
maud = "=0.27.0"
```

| Need | API |
|---|---|
| HTML, escaped by default | `ui::html!` → `ui::Markup`, a response (`text/html; charset=utf-8`) |
| Reusable views with children | `ui::Component` + `ui::Slots` |
| Full page for browsers, fragment for htmx | `ui::Page` + `ui::LayoutLayer`, `ui::BasicLayout` |
| htmx headers | `HxRequest`, `HxTarget` (request); `HxTrigger`, `HxRedirect`, `HxReswap`, `HxRetarget` (response) |
| The htmx script | `ui::htmx::asset_route()`, `ui::htmx::htmx_head()` |
| Forms bound to `Schema` errors | `ui::HtmlForm<T>`, `ui::FormState` |
| Raw HTML | `ui::trusted_html` only |

The runnable version is
[`crates/id_effect_axum/examples/095_htmx_todo.rs`](https://github.com/Industrial/id_effect/blob/main/crates/id_effect_axum/examples/095_htmx_todo.rs):

```bash
cargo run -p id_effect_axum --features ui --example 095_htmx_todo               # http://localhost:3002
cargo run -p id_effect_axum --features ui --example 095_htmx_todo -- --self-test
```

## Markup is a response

`html!` builds a `Markup` at compile time. Every `(expression)` it interpolates is escaped, in text
and in attribute values. A `Markup` is an Axum response, so a typed route's handler returns it like
any other value:

```rust
use std::convert::Infallible;

use axum::body::Body;
use axum::http::Request;
use id_effect::schema::{self, HasSchema, Schema};
use id_effect::{Effect, succeed};
use id_effect_axum::route::{Input, Route, Router};
use id_effect_axum::ui::{Markup, html};
use tower::ServiceExt;

struct Who;

impl HasSchema for Who {
  type A = String;
  type I = String;
  type E = ();

  fn schema() -> Schema<String, String> {
    schema::string()
  }
}

fn greet(_: &mut (), input: Input<(), String>) -> Effect<Markup, Infallible, ()> {
  succeed(html! { p { "Hello, " (input.query) } })
}

let app: axum::Router = Router::new()
  .route(Route::get("/hello").public().query::<Who>().handler(greet))
  .with_state(())
  .into_axum();

# tokio::runtime::Runtime::new().unwrap().block_on(async {
let req = Request::get("/hello?who=%3Cscript%3E").body(Body::empty()).unwrap();
let res = app.oneshot(req).await.unwrap();
assert_eq!(res.headers()["content-type"], "text/html; charset=utf-8");
let body = axum::body::to_bytes(res.into_body(), usize::MAX).await.unwrap();
assert_eq!(&body[..], b"<p>Hello, &lt;script&gt;</p>");
# });
```

## Components and slots

A component is a props struct implementing `Component`. Its children arrive as `Slots`: the
default slot (`child`, in order) and named slots (`slot`). `with` pairs props and children; the
result renders inside any template, including another component's slot.

```rust
use id_effect_axum::ui::{Component, Markup, Slots, html};

struct Card {
  title: String,
}

impl Component for Card {
  fn render(&self, slots: &Slots) -> Markup {
    html! {
      section.card {
        h2 { (self.title) }
        (slots.children())
        @if slots.has("footer") { footer { (slots.named("footer")) } }
      }
    }
  }
}

let card = Card { title: "Inbox".into() }.with(
  Slots::new()
    .child(html! { p { "one" } })
    .slot("footer", "2 unread")
    .child(html! { p { "two" } }),
);
assert_eq!(
  html! { (card) }.into_string(),
  "<section class=\"card\"><h2>Inbox</h2><p>one</p><p>two</p><footer>2 unread</footer></section>"
);
```

## Layouts and partials: page or fragment

A handler returns a `Page`: a title and its content. Behind a `LayoutLayer`, a browser gets the
content inside the layout and htmx (an `HX-Request: true` request that is neither boosted nor a
history restore) gets the content alone. The handler does not branch, which matters for typed
routes: they never see the request headers. `Page` responses carry `Vary: HX-Request`.

`BasicLayout` writes the document: `lang`, the title (plus an optional site name), stylesheets,
htmx and `<body hx-headers='{"x-csrf-token": …}'>`. The token comes from the `SecurityLayer`, so
that layer goes **outside** the `LayoutLayer` (a later `.layer` call):

```rust
use std::convert::Infallible;

use axum::body::Body;
use axum::http::Request;
use id_effect::{Effect, succeed};
use id_effect_axum::route::{Input, Route, Router};
use id_effect_axum::server::security::{CookieKey, SecurityLayer};
use id_effect_axum::ui::{BasicLayout, LayoutLayer, Page, htmx, html};
use tower::ServiceExt;

fn inbox(_: &mut (), _: Input) -> Effect<Page, Infallible, ()> {
  succeed(Page::new("Inbox", html! { ul #messages { li { "hello" } } }))
}

let app: axum::Router = Router::new()
  .route(Route::get("/").public().handler(inbox))
  .merge(htmx::asset_route())
  .with_state(())
  .into_axum()
  .layer(LayoutLayer::new(BasicLayout::new().site_name("Mail")))
  .layer(SecurityLayer::recommended(CookieKey::generate()));

# tokio::runtime::Runtime::new().unwrap().block_on(async {
let page = app.clone().oneshot(Request::get("/").body(Body::empty()).unwrap()).await.unwrap();
let csp = page.headers()["content-security-policy"].to_str().unwrap().to_owned();
assert!(csp.contains("script-src 'self'"));
let page = axum::body::to_bytes(page.into_body(), usize::MAX).await.unwrap();
let page = String::from_utf8(page.to_vec()).unwrap();
assert!(page.starts_with("<!DOCTYPE html>"));
assert!(page.contains("<title>Inbox · Mail</title>"));
assert!(page.contains("hx-headers=\"{&quot;x-csrf-token&quot;:"));

let htmx_request = Request::get("/").header("hx-request", "true").body(Body::empty()).unwrap();
let fragment = app.oneshot(htmx_request).await.unwrap();
let fragment = axum::body::to_bytes(fragment.into_body(), usize::MAX).await.unwrap();
assert_eq!(&fragment[..], b"<ul id=\"messages\"><li>hello</li></ul>");
# });
```

A layout is any `Layout` (or a closure `Fn(&LayoutContext, &str, Markup) -> Markup`). The
`LayoutContext` holds the `HX-*` headers, the CSRF token (`hx_headers()`, `csrf_field()`) and the
CSP nonce. Plain Axum handlers can take `LayoutContext` as an extractor and call `ui::respond`
instead of installing the layer. Partials are plain `Markup` or components: return them directly
from fragment endpoints.

## htmx headers

`HxRequest` reads every `HX-*` request header and never rejects; `wants_fragment()` is the rule the
layout uses. The response helpers go in a tuple before the body:

- `HxTrigger::event("todo-added")` fires client events (`and_detail` adds JSON details,
  `after_swap` / `after_settle` pick the header);
- `HxRedirect::to(path)` sends the browser elsewhere **on this origin only**: absolute paths are
  accepted, `https://other`, `//other`, `/\other` and `javascript:` are refused with
  `ForeignRedirect`, so a `?next=` parameter cannot become an open redirect;
- `HxReswap` and `HxRetarget` change where and how htmx swaps the response.

```rust
use axum::response::IntoResponse;
use id_effect_axum::ui::htmx::{HxRedirect, HxReswap, HxTrigger};
use id_effect_axum::ui::{html};

let res = (
  HxTrigger::event("saved").after_settle(),
  HxReswap::OuterHtml,
  html! { p { "saved" } },
)
  .into_response();
assert_eq!(res.headers()["hx-trigger-after-settle"], "saved");
assert_eq!(res.headers()["hx-reswap"], "outerHTML");

assert!(HxRedirect::to("/inbox").is_ok());
assert!(HxRedirect::to("https://evil.example/").is_err());
assert!(HxRedirect::to("//evil.example").is_err());
```

## The htmx script

htmx 2.0.11 is compiled into the crate (`ui::htmx::HTMX_JS`, 0BSD). `asset_route()` serves it at
`HTMX_PATH` (`/assets/htmx-2.0.11.min.js`) with a year of `immutable` caching and an `ETag`.
`htmx_head()` (used by `BasicLayout`) loads it with `integrity="sha384-…"` and sets
`<meta name="htmx-config">`:

| Setting | Value | Why |
|---|---|---|
| `allowEval` | `false` | no `eval` (`hx-on`, `js:` values) |
| `allowScriptTags` | `false` | `<script>` in swapped content is not run |
| `selfRequestsOnly` | `true` | htmx only talks to this origin |
| `includeIndicatorStyles` | `false` | no inline `<style>` (a strict `style-src` holds) |
| `historyCacheSize` | `0` | no page snapshots in `localStorage` |
| `responseHandling` | `422` swaps | a form re-rendered with its errors replaces the submitted one |

Since the script is same-origin, the page keeps `script-src 'self'`. The sha384 and the license are
recorded in `crates/id_effect_axum/assets/README.md`, and a test checks the file against them.
Updating htmx is its own reviewed change.

## Forms bound to `Schema` errors

`HtmlForm<T>` decodes an urlencoded body through `T`'s schema like `ValidForm<T>`, but input that
does not decode is not a `422` problem document: the handler gets a `FormState` with the submitted
values and every issue bound to its field, and renders the form again.

```rust
use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use id_effect::schema::{self, HasSchema, Schema};
use id_effect_axum::ui::{FormState, HtmlForm, InputKind, Markup, html};
use tower::ServiceExt;

struct SignUp;

impl HasSchema for SignUp {
  type A = (String, i64);
  type I = (String, i64);
  type E = ();

  fn schema() -> Schema<Self::A, Self::I> {
    schema::struct_(
      "name",
      schema::max_length(schema::non_empty(schema::string()), 20),
      "age",
      schema::i64(),
    )
  }
}

fn form(state: &FormState) -> Markup {
  html! {
    form #sign-up hx-post="/sign-up" {
      (state.error_summary())
      (state.field("name", "Name").required())
      (state.field("age", "Age").kind(InputKind::Number))
      button { "Sign up" }
    }
  }
}

async fn sign_up(submitted: HtmlForm<SignUp>) -> Response {
  match submitted.into_result() {
    Ok(((name, _age), _)) => html! { p { "Welcome, " (name) } }.into_response(),
    Err(state) => (StatusCode::UNPROCESSABLE_ENTITY, form(&state)).into_response(),
  }
}

let app: axum::Router = axum::Router::new().route("/sign-up", post(sign_up));
# tokio::runtime::Runtime::new().unwrap().block_on(async {
let req = Request::post("/sign-up")
  .header("content-type", "application/x-www-form-urlencoded")
  .body(Body::from("name=&age=old"))
  .unwrap();
let res = app.oneshot(req).await.unwrap();
assert_eq!(res.status(), StatusCode::UNPROCESSABLE_ENTITY);
let html = axum::body::to_bytes(res.into_body(), usize::MAX).await.unwrap();
let html = String::from_utf8(html.to_vec()).unwrap();
assert!(html.contains("<li data-message-key=\"schema-must-not-be-empty\">must not be empty</li>"));
assert!(html.contains("value=\"old\""));
assert!(html.contains("data-message-key=\"schema-expected-i64\""));
# });
```

Each `FieldError` has a message key derived from the schema message (`schema-must-not-be-empty`,
`schema-must-be-at-most-n-characters` with the number in `args()`), rendered as
`data-message-key`. `FormState::translate` replaces the text per key; the i18n crate's catalogs
supply translations, and the English schema message is the fallback. Password fields never echo
their value; the CSRF field is never kept in the state.

## Secure by default

- **Escaping.** maud escapes every interpolation. Raw HTML exists only in `ui::trusted_html`
  (`from_static(&'static str)`: developer-written markup such as an inline SVG icon). The test
  `no_preescaped_outside_trusted_module` fails on any other use of maud's unescaped wrapper in
  `crates/id_effect_axum/src` or `crates/id_effect_admin/src`. Escaping does not make a URL safe:
  do not put user input in `href` without checking its scheme.
- **CSRF.** Every htmx request carries the session's masked token (`hx-headers` on `<body>`), and
  the 0.11 `SecurityLayer` refuses state-changing requests without it. Plain forms render
  `LayoutContext::csrf_field()`.
- **Scripts.** One vendored, hash-pinned file served from the app; `script-src 'self'`; `eval` and
  swapped `<script>` tags off; requests to the same origin only.
- **Redirects.** `HxRedirect` refuses another origin.

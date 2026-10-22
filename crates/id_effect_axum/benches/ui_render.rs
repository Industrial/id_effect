//! Server-rendered UI cost (0.13, `r13-e-benches`; feature `ui`).
//!
//! - `ui/render_list_50`: one full page of a 50-row list rendered to its HTML string, as a
//!   `Page` behind `LayoutLayer` answers a browser: a `TodoList` component whose rows are
//!   `TodoRow` components (title, owner, a checkbox and an htmx delete button each), passed as
//!   slots, inside [`BasicLayout`] (document head, htmx script tag). Every title and owner holds
//!   characters maud must escape (`<`, `&`, `"`), so the escaping is on the path. No HTTP: the
//!   request path is `gate/axum_request_*` in `gate_request.rs`.
//! - `ui/empty_iter`: an iteration that does nothing (`black_box(())`), the harness floor the
//!   budget is measured over.
//!
//! The D15 budget (measured median × 1.5, 0.13 decision D15) is a `delta` gate in
//! `docs/benchmarks/gates.toml`: `ui/render_list_50 − ui/empty_iter ≤ 14,710 ns`, both from one
//! run, so it needs no base that has these benches (`docs/benchmarks/README.md`, "Breadth gates
//! (0.13)").
//!
//! Run: `cargo bench -p id_effect_axum --features ui --bench ui_render`

use std::hint::black_box;

use criterion::{Criterion, criterion_group, criterion_main};
use id_effect_axum::ui::{BasicLayout, Component, Layout, LayoutContext, Markup, Slots, html};

/// Rows on the page.
const ROWS: usize = 50;

/// One row's data, as a handler would have read it.
struct Todo {
  id: u64,
  title: String,
  owner: String,
  done: bool,
}

/// The list around its rows (the default slot).
struct TodoList<'a> {
  heading: &'a str,
}

impl Component for TodoList<'_> {
  fn render(&self, slots: &Slots) -> Markup {
    html! {
      section #todos {
        h1 { (self.heading) }
        ul.todo-list { (slots.children()) }
        p.count { (slots.children().len()) " items" }
      }
    }
  }
}

/// One row.
struct TodoRow<'a>(&'a Todo);

impl Component for TodoRow<'_> {
  fn render(&self, _slots: &Slots) -> Markup {
    let todo = self.0;
    html! {
      li #{ "todo-" (todo.id) } .done[todo.done] {
        input type="checkbox" checked[todo.done] hx-post={ "/todos/" (todo.id) "/toggle" }
          hx-target={ "#todo-" (todo.id) } hx-swap="outerHTML";
        span.title { (todo.title) }
        span.owner { "by " (todo.owner) }
        button hx-delete={ "/todos/" (todo.id) } hx-target={ "#todo-" (todo.id) }
          hx-confirm={ "Delete \"" (todo.title) "\"?" } { "Delete" }
      }
    }
  }
}

/// The full page for `todos`, as a string.
fn render_page(layout: &BasicLayout, ctx: &LayoutContext, todos: &[Todo]) -> String {
  let rows = todos.iter().fold(Slots::new(), |slots, todo| {
    slots.child(TodoRow(todo).alone())
  });
  let content = html! { main { (TodoList { heading: "Things <to> do" }.with(rows)) } };
  layout.render(ctx, "Todos", content).into_string()
}

fn bench_ui(c: &mut Criterion) {
  let todos: Vec<Todo> = (0..ROWS as u64)
    .map(|id| Todo {
      id,
      title: format!("Task #{id}: fix <b>bold</b> & \"quoted\" input"),
      owner: format!("user{id}@example.com <script>"),
      done: id % 3 == 0,
    })
    .collect();
  let layout = BasicLayout::new().site_name("Bench").stylesheet("/app.css");
  let ctx = LayoutContext::default();

  // The page is complete and nothing user-supplied comes out unescaped.
  let page = render_page(&layout, &ctx, &todos);
  assert!(page.starts_with("<!DOCTYPE html>"));
  assert_eq!(page.matches(" id=\"todo-").count(), ROWS);
  assert!(page.contains("50 items"));
  assert!(!page.contains("<b>") && !page.contains("com <script>"));
  assert!(page.contains("&lt;b&gt;bold&lt;/b&gt; &amp; &quot;quoted&quot;"));
  assert!(page.contains("user7@example.com &lt;script&gt;"));

  let mut group = c.benchmark_group("ui");
  group.bench_function("render_list_50", |b| {
    b.iter(|| black_box(render_page(&layout, &ctx, black_box(&todos))));
  });
  group.bench_function("empty_iter", |b| b.iter(|| black_box(())));
  group.finish();
}

criterion_group!(benches, bench_ui);
criterion_main!(benches);

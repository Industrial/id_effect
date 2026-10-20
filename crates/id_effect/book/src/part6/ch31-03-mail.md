# Mail (`id_effect_mail`)

`id_effect_mail` sends a service's outgoing mail — sign-up confirmations, password resets, invoices — through SMTP or the SendGrid API, renders it from templates shared by the HTML and text bodies, and shows every mail in the browser while you write it.

```toml
id_effect_mail = { version = "0.12", features = ["preview"] }  # `preview` only for the preview router
```

## Messages and transports

A `Message` is built and checked by `Message::builder()`: a sender, `To` / `Cc` / `Bcc` recipients, a subject, a text and an HTML body, attachments and inline `cid:` images. It is sent through the `Mailer` capability with `deliver(message)`, and a layer decides how:

| Layer | Sends |
|---|---|
| `SmtpMailer::layer(SmtpConfig)` | over pooled SMTP connections (lettre): implicit TLS, `STARTTLS` or plaintext |
| `SendGridMailer::layer(SendGridConfig)` | to the SendGrid v3 API through the `HttpClient` capability |
| `TestMailbox::layer()` | into memory, with `assert_sent_to`, `assert_subject` and `assert_contains` |

A refused send is a `MailError` that says whether a later attempt may succeed (`is_transient`); neither transport retries by itself. Retries belong to the next section.

## Delivering later

A request handler should not wait on an SMTP server, and a greylisting provider should not cost a customer their invoice. With feature `jobs`, mail goes out through the background job queue of `id_effect_jobs` instead:

```toml
id_effect_mail = { version = "0.12", features = ["jobs"] }
```

| Call | Does |
|---|---|
| `deliver_later(message)` | enqueues a `DeliverMail` job on the `JobQueueService` in the environment |
| `deliver_later_in(&tx, message, &clock)` | inserts the job with the caller's `SqlTransaction`: the mail is sent only if `tx` commits |
| `register_mail_jobs(registry)` | adds the handler that sends the job's message with the `MailerService` in the worker's environment |

`deliver_later_in` is the one to reach for when the mail announces a database change — "your order is confirmed" enqueued in the transaction that inserts the order. A rolled-back order sends nothing, and a committed one cannot lose its mail to a crash between the commit and the send.

The handler maps the transport's verdict onto the job's:

| `MailError` | Job |
|---|---|
| `Temporary` (SMTP `4xx`, HTTP `429` / `5xx`), `Transport` | retried: exponential backoff from 30 s, capped at 1 h, jittered; at most 12 attempts (about five hours) |
| `Rejected`, `Unauthorized`, `InvalidMessage`, `Config` | `dead` at once, the error kept as the job's `last_error` for the console and a manual `retry_now` |

`DeliverMail` runs on the queue `mail` (`DeliverMail::QUEUE`), so a slow provider never holds up other jobs: serve it with `Worker::queue(DeliverMail::QUEUE, n)`. Delivery is at least once: a worker that dies after the provider accepted the message, or a `Transport` failure after the provider received it, sends it again. To keep the same mail from being *enqueued* twice (a retried request), give it a unique key with `enqueue(DeliverMail::new(message)?, JobOptions::new().unique(..))`; the same call takes `RunAt` for a mail sent at a later time.

The job's payload is the whole message, attachments included (base64), stored as JSON in the queue table until the finished job is pruned (seven days by default): recipients, `Bcc`, bodies and files. Attachment and inline content together may be at most `DeliverMail::MAX_CONTENT_BYTES` (2 MiB); a larger message fails to enqueue with `JobError::Encode`. Keep large files in object storage and send a link.

```rust
use std::sync::Arc;
use std::time::Duration;

use id_effect::{Executor, HCons, HNil, TestExecutor, UtcDateTime, WallClock, hlist};
use id_effect_jobs::{Job, JobQueueService, JobRegistry, MemoryJobQueue, Worker, WorkerConfig};
use id_effect_mail::{
    Address, DeliverMail, MailError, MailerService, Message, TestMailbox, deliver_later,
    register_mail_jobs,
};

type Env = HCons<JobQueueService, HCons<MailerService, HNil>>;

let exec = TestExecutor::new();
let clock = WallClock::anchored(exec.now(), UtcDateTime::unsafe_make(1_800_000_000_000));
let mailbox = TestMailbox::new();
let env: Env = hlist![
    JobQueueService::new(MemoryJobQueue::new(clock.clone())),
    MailerService::new(mailbox.clone())
];
let worker = Worker::new(register_mail_jobs(JobRegistry::<Env>::new()).unwrap())
    .queue(DeliverMail::QUEUE, 4)
    .with_config(WorkerConfig { clock, ..WorkerConfig::default() });

let message = Message::builder()
    .from(Address::parse("billing@ledger.example").unwrap())
    .to(Address::parse("carol@ledger.example").unwrap())
    .subject("Your invoice")
    .text("Your invoice is attached.")
    .build()
    .unwrap();

// The provider greylists the first attempt.
mailbox.fail_next(MailError::Temporary { code: 451, message: "try later".into() });
let drain = {
    let (worker, env) = (worker.clone(), env.clone());
    move || worker.clone().run_until_idle().provide_all(env.clone())
};
let first = deliver_later::<Env, _>(message)
    .provide_all(env.clone())
    .flat_map(move |_| drain())
    .fork_root(Arc::new(exec.clone()));
exec.run_until_stalled();
assert!(first.is_done());
mailbox.assert_none_sent();

// Thirty seconds (± 20 %) later the retry goes out.
exec.advance(Duration::from_secs(60));
let second = worker.run_until_idle().provide_all(env).fork_root(Arc::new(exec.clone()));
exec.run_until_stalled();
assert!(second.is_done());
mailbox.assert_sent_to("carol@ledger.example");
```

`crates/id_effect_mail/tests/mail.rs` checks the retry on virtual time, a permanent refusal going `dead` after one attempt, and — against PostgreSQL — that a mail enqueued with `deliver_later_in` is neither visible to a worker nor sent until its transaction commits, and never after a rollback. The runnable example is `cargo run -p id_effect_mail --example 010_mail --features jobs`.

## Templates

`Templates` is a [minijinja](https://docs.rs/minijinja) registry (Jinja syntax, rendered at runtime). A mail named `welcome` is up to three templates:

| Template | Renders | Escaping |
|---|---|---|
| `welcome.subject.txt` | the subject (trimmed) | none |
| `welcome.txt` | the text body | none |
| `welcome.html` | the HTML body | every interpolated value |

At least one body must exist. Layouts and partials are ordinary templates — any name, conventionally `layouts/` and `partials/` — used with `{% extends %}` and `{% include %}`, so the HTML and text layouts each hold the header, footer and signature once.

The context is any `Serialize` value, usually one struct per mail. A variable the context does not have is an **error** naming the template and the line, never an empty string: a renamed field fails the first render instead of mailing customers a blank. An optional value is tested with `{% if note is defined %}` or given a default with `{{ note | default("") }}`.

```rust
use id_effect_mail::{Address, TemplateError, Templates};
use serde::Serialize;

#[derive(Serialize)]
struct Welcome {
    name: String,
    confirm_url: String,
}

let templates = Templates::new()
    .with_template("layouts/base.html", "<body>{% block body %}{% endblock %}</body>").unwrap()
    .with_template("layouts/base.txt", "{% block body %}{% endblock %}\n-- Ledger").unwrap()
    .with_template("welcome.subject.txt", "Welcome, {{ name }}").unwrap()
    .with_template(
        "welcome.html",
        r#"{% extends "layouts/base.html" %}{% block body %}<a href="{{ confirm_url }}">Confirm</a>, {{ name }}{% endblock %}"#,
    )
    .unwrap()
    .with_template(
        "welcome.txt",
        r#"{% extends "layouts/base.txt" %}{% block body %}Confirm: {{ confirm_url }}{% endblock %}"#,
    )
    .unwrap();

let context = Welcome { name: "Carol & Co".into(), confirm_url: "https://ledger.example/c?t=1".into() };
let message = templates
    .message("welcome", &context)
    .unwrap()
    .from(Address::parse("hello@ledger.example").unwrap())
    .to(Address::parse("carol@ledger.example").unwrap())
    .build()
    .unwrap();
assert_eq!(message.subject(), "Welcome, Carol & Co");
assert_eq!(message.text(), Some("Confirm: https://ledger.example/c?t=1\n-- Ledger"));
assert!(message.html().unwrap().contains(", Carol &amp; Co</body>"));

#[derive(Serialize)]
struct Renamed {
    username: String,
}
let Err(TemplateError::Render { template, message }) =
    templates.render("welcome", &Renamed { username: "carol".into() })
else {
    panic!("a missing variable fails")
};
assert_eq!(template, "welcome.html");
assert!(message.contains("undefined"));
```

`templates.message(mail, &context)` returns a `MessageBuilder` with the subject and bodies set; `templates.render` returns them as a `RenderedMail` (`subject`, `text`, `html`) when you build the message yourself. A `TemplateError` converts into `MailError::InvalidMessage`, so `?` works in code returning either.

In a service the templates live in a directory next to the code:

```rust,no_run
use id_effect_mail::Templates;

let templates = Templates::from_dir("templates/mail").unwrap();
assert_eq!(templates.reloads(), cfg!(debug_assertions));
```

`Templates::from_dir` names templates by their path below the directory. In a **debug build** it re-reads them on every render, so an edited template shows on the next one; in a release build each is read once, on first use. `.reload(bool)` overrides either.

## Previewing mail in development

With feature `preview`, `MailPreviews` registers named previews — a mail and the fixture context to render it with — and serves them from an axum router at `/_mail/previews`:

| Path | Shows |
|---|---|
| `/_mail/previews` | every registered preview, with links to its views |
| `/_mail/previews/{name}` | the subject, sender and recipient, the HTML body in a frame and the text body |
| `/_mail/previews/{name}/html` | the HTML body alone |
| `/_mail/previews/{name}/text` | the text body |
| `/_mail/previews/{name}/raw` | the MIME message a transport would send |

Each request renders again, so with `Templates::from_dir` in a debug build you edit a template, reload the page and see the result — no restart, no mail sent. A preview whose template fails answers `500` with the template error. Preview messages go from `previews@example.com` to `recipient@example.com` unless `.sender(..)` / `.recipient(..)` say otherwise.

```rust
use id_effect_mail::{MailPreviews, PREVIEWS_PATH, Templates};
use serde_json::json;

let templates = Templates::new()
    .with_template("receipt.subject.txt", "Receipt {{ number }}").unwrap()
    .with_template("receipt.txt", "Paid {{ amount }}.").unwrap();
let previews = MailPreviews::new(templates)
    .preview("receipt-paid", "receipt", json!({ "number": 7, "amount": "12.00 EUR" }));

let message = previews.message("receipt-paid").unwrap().unwrap();
assert_eq!(message.subject(), "Receipt 7");

// Mounted next to the app's routes, in development only.
let app: axum::Router = axum::Router::new().merge(previews.router());
assert_eq!(PREVIEWS_PATH, "/_mail/previews");
# drop(app);
```

Mount the router only in development: it serves whatever the fixtures contain to anyone who can reach it.

## Testing against a real SMTP server

`TestMailbox` covers most tests. To exercise the SMTP transport itself, `id_effect_test` has a **Mailpit** container (`Mailpit`): an SMTP server on port 1025 that keeps every message and lists it on an HTTP API on port 8025 (`GET /api/v1/messages`, `GET /api/v1/message/{id}`).

```rust,no_run
use id_effect::{Effect, need};
use id_effect_mail::SmtpConfig;
use id_effect_test::{ContainerError, ContainerImage, Mailpit, MailpitContainer};

// The SMTP config for `SmtpMailer::layer` and the API base URL to read the messages from.
let endpoints: Effect<(SmtpConfig, String), ContainerError, id_effect::caps!(MailpitContainer)> =
    need::<MailpitContainer>().map(|mailpit| {
        (SmtpConfig::plaintext(mailpit.host(), mailpit.smtp_port()), mailpit.api_url())
    });
let endpoints = endpoints.provide_layer(Mailpit::new().layer());
# let _ = endpoints;
```

The image is **Mailpit 1.31.6** (`axllent/mailpit:v1.31.6@sha256:23fe39e81bde596a918000b3135c0f85dc8bb4c9ea6be274b051143f3e300387`, MIT), pinned by the digest of its multi-arch index. `crates/id_effect_mail/tests/mailpit.rs` renders a templated mail, sends it through `SmtpMailer` and checks the sender, recipient, subject, `Message-ID` and both bodies through the API. Without Docker it prints `SKIP` and passes; with `ID_EFFECT_REQUIRE_DOCKER=1` (the CI `containers-integration` job) a missing Docker daemon fails it.

The runnable example `cargo run -p id_effect_mail --example 020_templates --features preview` renders an invoice from `examples/mail_templates/` (a layout and a partial shared by both bodies), sends it to a `TestMailbox` and prints the preview's raw MIME message.

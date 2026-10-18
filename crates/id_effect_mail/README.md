# `id_effect_mail`

Outgoing mail for id_effect services: SMTP over lettre, the SendGrid v3 API over the HTTP client,
minijinja templates, and delivery through `id_effect_jobs` (ADR 0010 Amendment 6).

The crate grows over the 0.10 release. Today it has the `Message` model (text and HTML bodies,
attachments, inline `cid:` parts, extra headers), the `Mailer` capability with three layers
(`SmtpMailer::layer`, pooled SMTP over rustls; `SendGridMailer::layer`, the SendGrid v3 API;
`TestMailbox::layer`, an in-memory mailbox with `assert_sent_to` / `assert_subject` /
`assert_contains`), `Templates` (minijinja subject, text and HTML templates with shared layouts
and partials, HTML autoescaped, missing variables an error), `MailPreviews` (feature `preview`:
an axum router at `/_mail/previews` showing each registered preview as HTML, text and raw MIME,
re-reading the templates from disk in a debug build), `deliver_later` / `deliver_later_in`
(feature `jobs`: a `DeliverMail` job on `id_effect_jobs`, enqueued alone or with the caller's
SQL transaction, sent by the handler `register_mail_jobs` adds, transient failures retried with
backoff and permanent ones dead at once), and `Address`, a validated `local@domain`
mailbox address (lengths per RFC 5321, dot-separated domain labels, no whitespace):

```rust
use id_effect_mail::Address;

let to = Address::parse("carol@ledger.example").unwrap();
assert_eq!(to.local(), "carol");
assert_eq!(to.domain(), "ledger.example");
assert!(Address::parse("carol@").is_err());
```

Tier `platform`: the Ledger reference app under `examples/reference/` depends on it.

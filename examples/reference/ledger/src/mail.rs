//! Outgoing mail: the transport of the profile ([`mailer`]), the templates under
//! `templates/mail/` ([`templates`]), the account mails of [`crate::signup`] and the previews.
//!
//! | Profile | Transport |
//! |---|---|
//! | `test` | `memory`: [`TestMailbox`], nothing leaves the process (a test swaps in Mailpit) |
//! | `dev` | `smtp` to Mailpit from `compose.yaml`, and `/_mail/previews` |
//! | `prod` | `sendgrid`: the SendGrid v3 API over a [`TracedHttpClient`] (one client span per send) |
//!
//! Every mail is rendered from minijinja templates: `<name>.subject.txt`, `<name>.txt` and
//! `<name>.html`, sharing `layout.txt` / `layout.html`. The statement mail goes out through the
//! job queue (`deliver_later`); the verification and reset mails never do (their token would
//! sit in the jobs table), see [`AccountMails`].

use id_effect_kit::mail::{
  AccountMails, Address, MailPreviews, Mailbox, MailerService, SendGridConfig, SendGridMailer,
  SmtpConfig, SmtpMailer, Templates, TestMailbox,
};
use id_effect_kit::otel::TracedHttpClient;
use id_effect_kit::platform::http::{
  HttpClientConfig, HttpClientService, ReqwestHttpClient, RetryPolicy, SsrfPolicy,
};
use serde_json::json;

use crate::config::{MailConfig, MailTransport};
use crate::error::StartupError;

/// The mail of the monthly statement.
pub const STATEMENT_MAIL: &str = "statement";

/// What sending mail takes: the transport, the templates and the sender.
#[derive(Clone)]
pub struct Mail {
  /// Where messages go.
  pub mailer: MailerService,
  /// Every mail's templates.
  pub templates: Templates,
  /// The sender.
  pub from: Mailbox,
}

/// The transport of `config`: in memory, SMTP or SendGrid (over `http`, the traced client).
pub fn mailer(config: &MailConfig, http: HttpClientService) -> Result<MailerService, StartupError> {
  let settings = |e: String| StartupError::Settings(format!("mail: {e}"));
  match config.transport {
    MailTransport::Memory => Ok(MailerService::new(TestMailbox::new())),
    MailTransport::Smtp => {
      let smtp = SmtpConfig::plaintext(config.smtp_host.clone(), config.smtp_port);
      SmtpMailer::new(smtp)
        .map(MailerService::new)
        .map_err(|e| settings(e.to_string()))
    }
    MailTransport::Sendgrid => {
      let key = config
        .sendgrid_api_key
        .as_ref()
        .ok_or_else(|| settings("transport `sendgrid` needs `sendgrid_api_key`".to_owned()))?;
      let mut sendgrid = SendGridConfig::new(key.expose().clone());
      if let Some(url) = &config.sendgrid_base_url {
        sendgrid = sendgrid.base_url(url.clone());
      }
      Ok(MailerService::new(SendGridMailer::new(sendgrid, http)))
    }
  }
}

/// The outbound HTTP client: every call in a client span carrying the W3C `traceparent`, with
/// the default retry inside the span (each attempt an event on it), under `ssrf` (`[outbound]`).
pub fn http_client(ssrf: &SsrfPolicy) -> HttpClientService {
  HttpClientService::new(TracedHttpClient::with_retry(
    ReqwestHttpClient::from_config(&HttpClientConfig::default().with_ssrf(ssrf.clone())),
    RetryPolicy::default(),
  ))
}

/// The templates under `config.templates` (re-read on every render in a debug build).
pub fn templates(config: &MailConfig) -> Result<Templates, StartupError> {
  Templates::from_dir(config.templates.clone())
    .map_err(|e| StartupError::Settings(format!("mail templates: {e}")))
}

/// The sender of `config`.
pub fn sender(config: &MailConfig) -> Result<Mailbox, StartupError> {
  let address = Address::parse(config.from.clone())
    .map_err(|e| StartupError::Settings(format!("mail.from: {e}")))?;
  Ok(Mailbox::named(config.from_name.clone(), address))
}

/// The verification and reset mails, rendered from the Ledger's templates and sent directly
/// through `mail`'s transport.
pub fn account_mails(mail: &Mail, config: &MailConfig) -> AccountMails {
  AccountMails::new(
    mail.mailer.clone(),
    mail.from.clone(),
    config.verify_url.clone(),
    config.reset_url.clone(),
  )
  .with_templates(mail.templates.clone())
}

/// `/_mail/previews`: every mail rendered with a fixture, from the templates on disk.
pub fn previews(mail: &Mail) -> axum::Router {
  let account = json!({
    "email": "carol@ledger.example",
    "link": "http://localhost:8080/verify?token=0123456789abcdef_preview",
    "valid_for": "24 hours",
    "expires_at": "2026-11-01T06:00:00Z",
  });
  let statement = json!({
    "owner": "carol",
    "org": "acme",
    "month": "2026-09",
    "account": {"code": "1000", "name": "Cash"},
    "opening": "1,200.00",
    "postings": 3,
    "closing": "1,450.00",
    "filename": "acme-1000-2026-09.csv",
  });
  let mut linked = statement.clone();
  linked["link"] = json!("https://statements.example/acme/1000/2026-09.csv?X-Amz-Signature=…");
  linked["link_valid_for"] = json!("7 days");
  MailPreviews::new(mail.templates.clone())
    .sender(mail.from.clone())
    .preview("statement", STATEMENT_MAIL, statement)
    .preview("statement-linked", STATEMENT_MAIL, linked)
    .preview("account-verify", "account_verify", account.clone())
    .preview("account-reset", "account_reset", account)
    .router()
}

//! `Templates` over the directory `tests/templates/`: layouts and partials shared by the HTML and
//! text bodies, HTML autoescaping, strict undefined values, and re-reading edited templates in
//! reload mode (msg.email#2).

use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use id_effect_mail::{Address, TemplateError, Templates};
use serde::Serialize;

#[derive(Serialize)]
struct User {
  name: String,
}

/// The typed context of the `welcome` mail.
#[derive(Serialize)]
struct Welcome {
  user: User,
  confirm_url: String,
  company: String,
}

fn welcome(name: &str) -> Welcome {
  Welcome {
    user: User { name: name.into() },
    confirm_url: "https://ledger.example/confirm?t=1&u=2".into(),
    company: "Ledger GmbH".into(),
  }
}

fn templates() -> Templates {
  Templates::from_dir(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/templates")).unwrap()
}

/// A fresh directory under the system temp dir.
fn scratch_dir(label: &str) -> PathBuf {
  let nanos = SystemTime::now()
    .duration_since(UNIX_EPOCH)
    .unwrap()
    .as_nanos();
  let dir = std::env::temp_dir().join(format!(
    "id-effect-mail-{label}-{}-{nanos}",
    std::process::id()
  ));
  std::fs::create_dir_all(&dir).unwrap();
  dir
}

#[test]
fn layout_wraps_html_and_text() {
  let mail = templates()
    .render("welcome", &welcome("Carol <&> Co"))
    .unwrap();
  assert_eq!(
    mail.subject.as_deref(),
    Some("Welcome to Ledger, Carol <&> Co")
  );

  let html = mail.html.expect("an HTML body");
  assert!(html.starts_with("<!doctype html>"), "{html}");
  assert!(html.contains("<title>Welcome</title>"), "{html}");
  assert!(
    html.contains("<h1>Hello Carol &lt;&amp;&gt; Co</h1>"),
    "{html}"
  );
  assert!(
    html.contains(r#"href="https:&#x2f;&#x2f;ledger.example&#x2f;confirm?t=1&amp;u=2""#),
    "attribute values are escaped too: {html}"
  );
  assert!(
    html.contains(r#"<p class="footer">Ledger &middot; Ledger GmbH</p>"#),
    "the partial: {html}"
  );
  assert!(html.trim_end().ends_with("</html>"), "{html}");

  let text = mail.text.expect("a text body");
  assert_eq!(
    text,
    "Hello Carol <&> Co,\n\nconfirm your account: https://ledger.example/confirm?t=1&u=2\n\n\
     -- Ledger, Ledger GmbH"
  );
}

#[test]
fn a_text_only_mail_renders_without_html() {
  #[derive(Serialize)]
  struct Receipt {
    number: u32,
    amount: &'static str,
    company: &'static str,
  }
  let message = templates()
    .message(
      "receipt",
      &Receipt {
        number: 42,
        amount: "12.00 EUR",
        company: "Ledger GmbH",
      },
    )
    .unwrap()
    .from(Address::parse("billing@ledger.example").unwrap())
    .to(Address::parse("carol@ledger.example").unwrap())
    .build()
    .unwrap();
  assert_eq!(message.subject(), "Receipt 42");
  assert_eq!(message.html(), None);
  assert!(
    message
      .text()
      .unwrap()
      .starts_with("Paid 12.00 EUR for invoice 42.")
  );
}

#[test]
fn a_variable_missing_in_a_partial_is_an_error() {
  #[derive(Serialize)]
  struct NoCompany {
    user: User,
    confirm_url: &'static str,
  }
  let context = NoCompany {
    user: User {
      name: "Carol".into(),
    },
    confirm_url: "https://ledger.example/confirm",
  };
  match templates().render("welcome", &context) {
    Err(TemplateError::Render { template, message }) => {
      assert_eq!(template, "welcome.html");
      assert!(message.contains("undefined"), "{message}");
      assert!(message.contains("partials/footer.html"), "{message}");
    }
    other => panic!("expected a render error, got {other:?}"),
  }
  assert_eq!(
    templates().render("nothing", &context).unwrap_err(),
    TemplateError::NotFound("nothing".into())
  );
}

#[test]
fn reload_mode_rereads_edited_templates() {
  let dir = scratch_dir("reload");
  let note = dir.join("note.txt");
  std::fs::write(&note, "first {{ n }}").unwrap();
  let reloading = Templates::from_dir(&dir).unwrap().reload(true);
  let cached = Templates::from_dir(&dir).unwrap().reload(false);
  let text = |t: &Templates| {
    t.render("note", &serde_json::json!({ "n": 1 }))
      .unwrap()
      .text
  };
  assert_eq!(text(&reloading).as_deref(), Some("first 1"));
  assert_eq!(text(&cached).as_deref(), Some("first 1"));

  std::fs::write(&note, "second {{ n }}").unwrap();
  assert_eq!(text(&reloading).as_deref(), Some("second 1"));
  assert_eq!(
    text(&cached).as_deref(),
    Some("first 1"),
    "without reload a template is read once"
  );
  assert!(reloading.reloads());
  assert!(!cached.reloads());
  assert_eq!(reloading.dir(), Some(dir.as_path()));
  std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn from_dir_reloads_in_debug_builds() {
  assert_eq!(templates().reloads(), cfg!(debug_assertions));
  assert!(!Templates::new().reloads());
}

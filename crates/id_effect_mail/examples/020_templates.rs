//! Renders the `invoice` mail from `examples/mail_templates/` (a layout and a partial shared by
//! the HTML and text bodies) with a typed context, sends it to a `TestMailbox`, and prints what
//! was sent. With `--features preview` it also renders the registered preview the way
//! `/_mail/previews/invoice/raw` serves it.
//!
//! `cargo run -p id_effect_mail --example 020_templates [--features preview]`

use id_effect::Exit;
use id_effect_mail::{Address, MailError, Mailbox, MailerEnv, Templates, TestMailbox, deliver};
use serde::Serialize;

#[derive(Serialize)]
struct Line {
  item: &'static str,
  amount: &'static str,
}

/// The `invoice` mail's context: every variable its templates use.
#[derive(Serialize)]
struct Invoice {
  customer: &'static str,
  number: u32,
  due: &'static str,
  lines: Vec<Line>,
}

fn invoice() -> Invoice {
  Invoice {
    customer: "Carol",
    number: 1042,
    due: "2026-11-01",
    lines: vec![
      Line {
        item: "Ledger Pro, October",
        amount: "49.00 EUR",
      },
      Line {
        item: "Extra seats (2)",
        amount: "18.00 EUR",
      },
    ],
  }
}

fn main() -> Result<(), MailError> {
  let templates = Templates::from_dir(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/examples/mail_templates"
  ))?;
  let message = templates
    .message("invoice", &invoice())?
    .from(Mailbox::named(
      "Ledger billing",
      Address::parse("billing@ledger.example").unwrap(),
    ))
    .to(Address::parse("carol@ledger.example").unwrap())
    .build()?;

  let mailbox = TestMailbox::new();
  match deliver::<MailerEnv, _>(message)
    .provide_layer(mailbox.layer())
    .block_on()
  {
    Exit::Success(sent) => println!("sent {:?}", sent.id),
    Exit::Failure(cause) => panic!("not sent: {cause:?}"),
  }
  let sent = mailbox.assert_subject("Invoice 1042 from Ledger");
  println!("Subject: {}\n", sent.subject());
  println!("{}\n", sent.text().unwrap_or_default());
  println!("{}", sent.html().unwrap_or_default());

  #[cfg(feature = "preview")]
  {
    let previews =
      id_effect_mail::MailPreviews::new(templates).preview("invoice", "invoice", invoice());
    let preview = previews.message("invoice").expect("registered")?;
    println!(
      "\n{}/invoice/raw:\n{}",
      id_effect_mail::PREVIEWS_PATH,
      String::from_utf8_lossy(&preview.to_mime()?)
    );
  }
  Ok(())
}

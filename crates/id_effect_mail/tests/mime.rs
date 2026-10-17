//! The MIME tree a message renders to: alternative bodies, inline `cid:` parts in a related
//! part, attachments in a mixed part.

use id_effect_mail::{Address, Attachment, InlinePart, Message, MessageBuilder};

fn base() -> MessageBuilder {
  Message::builder()
    .from(Address::parse("billing@ledger.example").unwrap())
    .to(Address::parse("carol@ledger.example").unwrap())
    .subject("Invoice 42")
}

fn mime(builder: MessageBuilder) -> String {
  String::from_utf8(builder.build().unwrap().to_mime().unwrap()).unwrap()
}

/// The byte offset of `needle` in `haystack`, panicking with both when it is absent.
fn at(haystack: &str, needle: &str) -> usize {
  haystack
    .find(needle)
    .unwrap_or_else(|| panic!("{needle:?} not in:\n{haystack}"))
}

#[test]
fn attachments_and_inline_images_nest_mixed_alternative_related() {
  let mime = mime(
    base()
      .text("Your invoice is attached.")
      .html("<p><img src=\"cid:logo\"> Your invoice is attached.</p>")
      .inline(
        InlinePart::new("logo", "image/png", vec![0x89, b'P', b'N', b'G'])
          .with_filename("logo.png"),
      )
      .attach(Attachment::new(
        "invoice-42.pdf",
        "application/pdf",
        b"%PDF\x00\xff".to_vec(),
      )),
  );
  let mixed = at(&mime, "Content-Type: multipart/mixed");
  let alternative = at(&mime, "Content-Type: multipart/alternative");
  let plain = at(&mime, "Content-Type: text/plain");
  let related = at(&mime, "Content-Type: multipart/related");
  let html = at(&mime, "Content-Type: text/html");
  let cid = at(&mime, "Content-ID: <logo>");
  let png = at(&mime, "Content-Type: image/png");
  let pdf = at(&mime, "Content-Type: application/pdf");
  assert!(mixed < alternative, "mixed wraps alternative");
  assert!(
    alternative < plain && plain < related,
    "text comes first in alternative"
  );
  assert!(related < html && html < cid, "html leads the related part");
  assert!(
    cid.abs_diff(png) < 200,
    "the inline part carries its content id"
  );
  assert!(png < pdf, "the attachment follows the body");
  assert!(mime.contains("Content-Disposition: inline; filename=\"logo.png\""));
  assert!(mime.contains("Content-Disposition: attachment; filename=\"invoice-42.pdf\""));
  assert!(mime.contains("cid:logo"));
  // Binary parts go base64: `%PDF\0\xff`.
  assert!(mime.contains("Content-Transfer-Encoding: base64"));
  assert!(mime.contains("JVBERgD/"));
  assert!(mime.contains("MIME-Version: 1.0"));
}

#[test]
fn html_with_inline_parts_and_no_text_is_a_related_part() {
  let mime = mime(
    base()
      .html("<img src=\"cid:chart\">")
      .inline(InlinePart::new(
        "chart",
        "image/svg+xml",
        b"<svg/>".to_vec(),
      )),
  );
  assert!(mime.contains("Content-Type: multipart/related"));
  assert!(mime.contains("Content-ID: <chart>"));
  assert!(mime.contains("Content-Disposition: inline\r\n"));
  assert!(!mime.contains("multipart/alternative"));
  assert!(!mime.contains("multipart/mixed"));
}

#[test]
fn text_and_html_without_parts_are_an_alternative_part() {
  let mime = mime(base().text("plain").html("<b>rich</b>"));
  let plain = at(&mime, "Content-Type: text/plain");
  let html = at(&mime, "Content-Type: text/html");
  assert!(mime.contains("Content-Type: multipart/alternative"));
  assert!(plain < html);
  assert!(!mime.contains("multipart/related"));
}

#[test]
fn an_attachment_on_a_text_body_is_a_mixed_part() {
  let mime = mime(base().text("see attached").attach(Attachment::new(
    "rows.csv",
    "text/csv",
    b"a,b\n1,2\n".to_vec(),
  )));
  assert!(mime.contains("Content-Type: multipart/mixed"));
  assert!(mime.contains("Content-Disposition: attachment; filename=\"rows.csv\""));
  assert!(!mime.contains("multipart/alternative"));
}

#[test]
fn headers_names_and_reply_to_are_rendered() {
  let mime = mime(
    base()
      .cc(id_effect_mail::Mailbox::named(
        "Dan",
        Address::parse("dan@ledger.example").unwrap(),
      ))
      .reply_to(Address::parse("help@ledger.example").unwrap())
      .header("List-Unsubscribe", "<mailto:unsubscribe@ledger.example>")
      .text("x"),
  );
  assert!(mime.contains("To: carol@ledger.example\r\n"));
  assert!(mime.contains("Cc: Dan <dan@ledger.example>\r\n"));
  assert!(mime.contains("Reply-To: help@ledger.example\r\n"));
  assert!(mime.contains("List-Unsubscribe: <mailto:unsubscribe@ledger.example>\r\n"));
  assert!(mime.contains("Date: "));
}

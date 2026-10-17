//! `SmtpMailer` against an in-process SMTP stub: the envelope and data it sends, pooled
//! connection reuse, and how reply codes map to `MailError`.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use id_effect::{Cause, Effect, Exit};
use id_effect_mail::{
  Address, MailError, MailerEnv, Message, Sent, SmtpConfig, SmtpMailer, deliver,
};
use id_effect_tokio::TokioRuntime;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;

/// One message the stub accepted.
#[derive(Clone, Debug)]
struct Received {
  from: String,
  rcpt: Vec<String>,
  data: String,
}

/// A minimal SMTP server: accepts every command, answers `RCPT` for `reject@…` with `550` and
/// for `later@…` with `451`, and records each message.
#[derive(Clone, Default)]
struct Stub {
  connections: Arc<AtomicUsize>,
  received: Arc<Mutex<Vec<Received>>>,
}

impl Stub {
  async fn start() -> (Self, u16) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let stub = Stub::default();
    let s = stub.clone();
    tokio::spawn(async move {
      while let Ok((socket, _)) = listener.accept().await {
        s.connections.fetch_add(1, Ordering::SeqCst);
        tokio::spawn(s.clone().session(socket));
      }
    });
    (stub, port)
  }

  async fn session(self, socket: tokio::net::TcpStream) {
    let (read, mut write) = socket.into_split();
    let mut lines = BufReader::new(read);
    let mut current = Received {
      from: String::new(),
      rcpt: Vec::new(),
      data: String::new(),
    };
    write.write_all(b"220 stub ESMTP ready\r\n").await.unwrap();
    let mut line = String::new();
    loop {
      line.clear();
      if lines.read_line(&mut line).await.unwrap_or(0) == 0 {
        return;
      }
      let upper = line.to_ascii_uppercase();
      let reply: &[u8] = if upper.starts_with("EHLO") || upper.starts_with("HELO") {
        b"250-stub greets you\r\n250 8BITMIME\r\n"
      } else if upper.starts_with("MAIL FROM:") {
        current.from = between_angles(&line);
        b"250 2.1.0 ok\r\n"
      } else if upper.starts_with("RCPT TO:") {
        let rcpt = between_angles(&line);
        if rcpt.starts_with("reject@") {
          b"550 5.1.1 no such user\r\n"
        } else if rcpt.starts_with("later@") {
          b"451 4.3.0 try again later\r\n"
        } else {
          current.rcpt.push(rcpt);
          b"250 2.1.5 ok\r\n"
        }
      } else if upper.starts_with("DATA") {
        write.write_all(b"354 go ahead\r\n").await.unwrap();
        loop {
          line.clear();
          if lines.read_line(&mut line).await.unwrap_or(0) == 0 {
            return;
          }
          if line == ".\r\n" {
            break;
          }
          current.data.push_str(&line);
        }
        let done = std::mem::replace(
          &mut current,
          Received {
            from: String::new(),
            rcpt: Vec::new(),
            data: String::new(),
          },
        );
        self.received.lock().unwrap().push(done);
        b"250 2.0.0 queued\r\n"
      } else if upper.starts_with("QUIT") {
        let _ = write.write_all(b"221 2.0.0 bye\r\n").await;
        return;
      } else {
        // RSET, NOOP and anything else.
        current.rcpt.clear();
        b"250 2.0.0 ok\r\n"
      };
      if write.write_all(reply).await.is_err() {
        return;
      }
    }
  }

  fn received(&self) -> Vec<Received> {
    self.received.lock().unwrap().clone()
  }
}

fn between_angles(line: &str) -> String {
  let start = line.find('<').map_or(0, |i| i + 1);
  let end = line[start..].find('>').map_or(line.len(), |i| start + i);
  line[start..end].to_owned()
}

fn message(to: &str, subject: &str) -> Message {
  Message::builder()
    .from(Address::parse("billing@ledger.example").unwrap())
    .to(Address::parse(to).unwrap())
    .bcc(Address::parse("audit@ledger.example").unwrap())
    .subject(subject)
    .text("Hello")
    .build()
    .unwrap()
}

async fn run<A: Send + 'static>(eff: Effect<A, MailError>) -> Result<A, MailError> {
  match TokioRuntime::current().expect("tokio").run(eff).await {
    Exit::Success(a) => Ok(a),
    Exit::Failure(Cause::Fail(e)) => Err(e),
    Exit::Failure(c) => panic!("unexpected cause: {c:?}"),
  }
}

fn send_two(port: u16, first: Message, second: Message) -> Effect<(Sent, Sent), MailError> {
  deliver::<MailerEnv, _>(first)
    .flat_map(move |a| deliver::<MailerEnv, _>(second).map(move |b| (a, b)))
    .provide_layer(SmtpMailer::layer(SmtpConfig::plaintext("127.0.0.1", port)))
}

#[tokio::test(flavor = "multi_thread")]
async fn delivers_the_envelope_and_reuses_the_pooled_connection() {
  let (stub, port) = Stub::start().await;
  let (a, b) = run(send_two(
    port,
    message("carol@ledger.example", "First"),
    message("dan@ledger.example", "Second"),
  ))
  .await
  .expect("both accepted");
  let received = stub.received();
  assert_eq!(received.len(), 2);
  assert_eq!(received[0].from, "billing@ledger.example");
  assert_eq!(
    received[0].rcpt,
    ["carol@ledger.example", "audit@ledger.example"]
  );
  assert!(received[0].data.contains("Subject: First\r\n"));
  assert!(
    !received[0].data.contains("audit@ledger.example"),
    "Bcc stays in the envelope"
  );
  assert!(received[1].data.contains("Subject: Second\r\n"));
  let id = a.id.expect("message id");
  assert!(received[0].data.contains(&format!("Message-ID: {id}\r\n")));
  assert_ne!(Some(id), b.id);
  assert_eq!(
    stub.connections.load(Ordering::SeqCst),
    1,
    "the second send reuses the pooled connection"
  );
}

#[tokio::test(flavor = "multi_thread")]
async fn reply_codes_map_to_mail_errors() {
  let (stub, port) = Stub::start().await;
  let send = |to: &str| {
    deliver::<MailerEnv, _>(message(to, "x"))
      .provide_layer(SmtpMailer::layer(SmtpConfig::plaintext("127.0.0.1", port)))
  };
  match run(send("reject@ledger.example")).await {
    Err(MailError::Rejected { code: 550, message }) => assert!(message.contains("no such user")),
    other => panic!("expected a 550 rejection, got {other:?}"),
  }
  match run(send("later@ledger.example")).await {
    Err(e @ MailError::Temporary { code: 451, .. }) => assert!(e.is_transient()),
    other => panic!("expected a 451 deferral, got {other:?}"),
  }
  assert!(stub.received().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_refused_connection_is_a_transport_error() {
  let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
  let port = listener.local_addr().unwrap().port();
  drop(listener);
  let program = deliver::<MailerEnv, _>(message("carol@ledger.example", "x"))
    .provide_layer(SmtpMailer::layer(SmtpConfig::plaintext("127.0.0.1", port)));
  assert!(matches!(run(program).await, Err(MailError::Transport(_))));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_bad_config_fails_the_layer() {
  let program = deliver::<MailerEnv, _>(message("carol@ledger.example", "x"))
    .provide_layer(SmtpMailer::layer(SmtpConfig::starttls("")));
  assert!(matches!(run(program).await, Err(MailError::Config(_))));
}

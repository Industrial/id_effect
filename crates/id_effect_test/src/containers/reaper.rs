//! The reaper: removes this process's [`Shared`](super::Shared) containers after it exits.
//!
//! A shared container lives in a `static`, and Rust runs no destructor for statics, so the
//! process cannot remove it on exit. The reaper is testcontainers'
//! [Ryuk](https://github.com/testcontainers/moby-ryuk), started once per process before the
//! first shared container: this process holds a TCP connection to it and registers the session
//! label ([`SESSION_LABEL`]). When the connection closes (the process exits, normally or killed)
//! Ryuk waits ten seconds, removes every container carrying the label, and exits (it is started
//! with auto-remove).
//!
//! Environment (the testcontainers names): `TESTCONTAINERS_RYUK_DISABLED=true` disables it
//! (shared containers then outlive the process); `TESTCONTAINERS_RYUK_CONTAINER_PRIVILEGED=true`
//! starts it privileged (SELinux hosts); `TESTCONTAINERS_DOCKER_SOCKET_OVERRIDE` is the Docker
//! socket path mounted into it (default: the `unix://` path of `DOCKER_HOST`, else
//! `/var/run/docker.sock`).

use std::io::{self, Read, Write};
use std::net::TcpStream;
use std::time::{Duration, Instant};

use testcontainers::core::{IntoContainerPort, Mount};
use testcontainers::{ContainerAsync, GenericImage, ImageExt};
use tokio::sync::OnceCell;

use super::ContainerError;
use super::docker::{SESSION, SESSION_LABEL};
use super::spec::start_request;

const RYUK_IMAGE: &str = "testcontainers/ryuk";
const RYUK_TAG: &str = "0.13.0";
const RYUK_PORT: u16 = 8080;

/// How long the handshake is retried while Ryuk starts listening.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(30);

/// The started reaper. Held in a static for the life of the process: dropping the connection
/// is what tells Ryuk to reap.
struct Reaper {
  _container: ContainerAsync<GenericImage>,
  _connection: TcpStream,
}

/// Starts the reaper once per process (unless disabled) and returns its outcome.
pub(crate) async fn ensure() -> Result<(), ContainerError> {
  static REAPER: OnceCell<Result<Reaper, ContainerError>> = OnceCell::const_new();
  if !env_flag("TESTCONTAINERS_RYUK_DISABLED") {
    REAPER
      .get_or_init(start)
      .await
      .as_ref()
      .map_err(ContainerError::clone)?;
  }
  Ok(())
}

fn env_flag(name: &str) -> bool {
  std::env::var(name).is_ok_and(|v| v.eq_ignore_ascii_case("true"))
}

async fn start() -> Result<Reaper, ContainerError> {
  match launch().await {
    Ok(reaper) => Ok(reaper),
    Err(e) => Err(ContainerError::Reaper {
      reason: format!("{RYUK_IMAGE}:{RYUK_TAG}: {e}"),
    }),
  }
}

async fn launch() -> Result<Reaper, Box<dyn std::error::Error + Send + Sync>> {
  let socket = socket_path(
    std::env::var("TESTCONTAINERS_DOCKER_SOCKET_OVERRIDE").ok(),
    std::env::var("DOCKER_HOST").ok(),
  );
  // Ryuk publishes a port too, and a failed start is kept for the life of the process: retry a
  // lost host-port race as the other containers do.
  let container = start_request(&format!("{RYUK_IMAGE}:{RYUK_TAG}"), || {
    GenericImage::new(RYUK_IMAGE, RYUK_TAG)
      .with_exposed_port(RYUK_PORT.tcp())
      .with_mount(Mount::bind_mount(socket.clone(), "/var/run/docker.sock"))
      .with_privileged(env_flag("TESTCONTAINERS_RYUK_CONTAINER_PRIVILEGED"))
      .with_host_config_modifier(|config| config.auto_remove = Some(true))
  })
  .await?;
  let address = format!(
    "{}:{}",
    container.get_host().await?,
    container.get_host_port_ipv4(RYUK_PORT).await?
  );
  let deadline = Instant::now() + HANDSHAKE_TIMEOUT;
  let connection =
    tokio::task::spawn_blocking(move || handshake_until(&address, deadline)).await??;
  Ok(Reaper {
    _container: container,
    _connection: connection,
  })
}

/// The Docker socket to mount into Ryuk.
fn socket_path(socket_override: Option<String>, docker_host: Option<String>) -> String {
  socket_override
    .or_else(|| docker_host?.strip_prefix("unix://").map(str::to_owned))
    .unwrap_or_else(|| "/var/run/docker.sock".to_owned())
}

/// Registers the session label with Ryuk, retrying until `deadline` while it starts listening.
fn handshake_until(address: &str, deadline: Instant) -> Result<TcpStream, String> {
  loop {
    match register(address) {
      Ok(connection) => return Ok(connection),
      Err(e) if Instant::now() >= deadline => {
        return Err(format!("no ACK from {address}: {e}"));
      }
      Err(_) => std::thread::sleep(Duration::from_millis(100)),
    }
  }
}

/// One attempt: `label=<key>=<session>\n`, answered `ACK\n`.
fn register(address: &str) -> io::Result<TcpStream> {
  let mut connection = TcpStream::connect(address)?;
  connection.set_read_timeout(Some(Duration::from_secs(5)))?;
  connection.write_all(format!("label={SESSION_LABEL}={}\n", *SESSION).as_bytes())?;
  let mut ack = [0_u8; 4];
  connection.read_exact(&mut ack)?;
  if &ack != b"ACK\n" {
    return Err(io::Error::other(format!(
      "unexpected reply {:?}",
      String::from_utf8_lossy(&ack)
    )));
  }
  connection.set_read_timeout(None)?;
  Ok(connection)
}

#[cfg(test)]
mod tests {
  use std::net::TcpListener;

  use super::*;

  #[test]
  fn socket_path_prefers_the_override_then_a_unix_docker_host() {
    let s = |o: Option<&str>, h: Option<&str>| socket_path(o.map(Into::into), h.map(Into::into));
    assert_eq!(s(Some("/o.sock"), Some("unix:///h.sock")), "/o.sock");
    assert_eq!(
      s(None, Some("unix:///run/user/1000/docker.sock")),
      "/run/user/1000/docker.sock"
    );
    assert_eq!(s(None, Some("tcp://10.0.0.1:2375")), "/var/run/docker.sock");
    assert_eq!(s(None, None), "/var/run/docker.sock");
  }

  /// A fake Ryuk on a loopback port answering each connection with `reply`.
  fn fake_ryuk(reply: &'static [u8]) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap().to_string();
    std::thread::spawn(move || {
      for stream in listener.incoming() {
        let mut stream = stream.unwrap();
        let mut line = [0_u8; 1];
        while stream.read(&mut line).unwrap() == 1 && line[0] != b'\n' {}
        stream.write_all(reply).unwrap();
        std::mem::forget(stream);
      }
    });
    address
  }

  #[test]
  fn register_sends_the_session_label_and_expects_ack() {
    assert!(register(&fake_ryuk(b"ACK\n")).is_ok());
    let nack = register(&fake_ryuk(b"NOPE")).unwrap_err();
    assert!(nack.to_string().contains("unexpected reply"), "{nack}");
  }

  #[test]
  fn handshake_retries_until_its_deadline() {
    let soon = || Instant::now() + Duration::from_millis(300);
    assert!(handshake_until(&fake_ryuk(b"ACK\n"), soon()).is_ok());
    // Nothing listens on a port that was just released.
    let address = TcpListener::bind("127.0.0.1:0")
      .unwrap()
      .local_addr()
      .unwrap()
      .to_string();
    let started = Instant::now();
    let error = handshake_until(&address, soon()).unwrap_err();
    assert!(started.elapsed() >= Duration::from_millis(300));
    assert!(error.starts_with("no ACK from "), "{error}");
  }
}

//! The UDP socket of an endpoint, with the buffers asked for (best effort, F23).

use std::io;
use std::net::{SocketAddr, UdpSocket};

use socket2::{SockRef, Socket};

use super::config::SocketBuffers;

/// Bind `addr` (UDP) and ask for `wanted` buffers: the socket and what the OS granted.
pub(crate) fn bind_udp(
  addr: SocketAddr,
  wanted: SocketBuffers,
) -> io::Result<(UdpSocket, SocketBuffers)> {
  let socket = UdpSocket::bind(addr)?;
  let granted = apply(&socket, wanted);
  Ok((socket, granted))
}

/// Ask for `wanted` on `socket` and read back the sizes granted, logging both at `debug` (a
/// shortfall names the Linux sysctl that caps it).
pub(crate) fn apply(socket: &UdpSocket, wanted: SocketBuffers) -> SocketBuffers {
  let sock = SockRef::from(socket);
  let send = one(
    &sock,
    Direction {
      name: "send",
      sysctl: "net.core.wmem_max",
      set: Socket::set_send_buffer_size,
      get: Socket::send_buffer_size,
    },
    wanted.send,
  );
  let recv = one(
    &sock,
    Direction {
      name: "receive",
      sysctl: "net.core.rmem_max",
      set: Socket::set_recv_buffer_size,
      get: Socket::recv_buffer_size,
    },
    wanted.recv,
  );
  SocketBuffers::new(send, recv)
}

struct Direction {
  name: &'static str,
  sysctl: &'static str,
  set: fn(&Socket, usize) -> io::Result<()>,
  get: fn(&Socket) -> io::Result<usize>,
}

fn one(sock: &Socket, dir: Direction, wanted: usize) -> usize {
  let Direction {
    name,
    sysctl,
    set,
    get,
  } = dir;
  if wanted > 0
    && let Err(e) = set(sock, wanted)
  {
    tracing::debug!(
      target: "id_effect_platform::quic",
      "UDP {name} buffer: asking for {wanted} bytes failed: {e}"
    );
  }
  let granted = get(sock).unwrap_or(0);
  if granted < wanted {
    tracing::debug!(
      target: "id_effect_platform::quic",
      "UDP {name} buffer: {granted} bytes granted of {wanted} asked for; raise {sysctl} (Linux) \
       so bursts are not dropped"
    );
  } else {
    tracing::debug!(
      target: "id_effect_platform::quic",
      "UDP {name} buffer: {granted} bytes granted ({wanted} asked for)"
    );
  }
  granted
}

#[cfg(test)]
mod tests {
  use super::*;

  fn local() -> SocketAddr {
    "127.0.0.1:0".parse().expect("addr")
  }

  /// A size the OS grants (Linux reports twice what it keeps, so at least the request) is
  /// applied; one above any cap comes back short, which is what is logged.
  #[test]
  fn requested_buffers_are_applied_or_the_shortfall_reported() {
    let small = SocketBuffers::new(64 * 1024, 64 * 1024);
    let (_socket, granted) = bind_udp(local(), small).expect("bind");
    assert!(granted.send >= small.send, "{granted:?}");
    assert!(granted.recv >= small.recv, "{granted:?}");

    // A cap to fall short of exists on Unix (net.core.*mem_max); Windows grants the request.
    #[cfg(unix)]
    {
      let huge = SocketBuffers::new(1 << 30, 1 << 30);
      let (_socket, granted) = bind_udp(local(), huge).expect("bind");
      assert!(granted.recv > 0 && granted.recv < huge.recv, "{granted:?}");
      assert!(granted.send > 0 && granted.send < huge.send, "{granted:?}");
    }

    // Asking for nothing keeps the OS default.
    let (_socket, granted) = bind_udp(local(), SocketBuffers::os_default()).expect("bind");
    assert!(granted.recv > 0 && granted.send > 0, "{granted:?}");
  }

  #[test]
  fn a_taken_address_fails_the_bind() {
    let (socket, _) = bind_udp(local(), SocketBuffers::default()).expect("bind");
    let taken = socket.local_addr().expect("addr");
    assert!(bind_udp(taken, SocketBuffers::default()).is_err());
  }
}

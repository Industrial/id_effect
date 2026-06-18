//! Linear session types — phantom protocol states for send/receive alternation.

use std::marker::PhantomData;

/// Terminal session (protocol complete).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SessionEnd;

/// Linear **send** phase for protocol `P`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SessionSend<P>(PhantomData<P>);

/// Linear **receive** phase for protocol `P`.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub struct SessionRecv<P>(PhantomData<P>);

impl<P> SessionSend<P> {
  /// Constructs the send-phase token (zero-sized at runtime).
  pub fn new() -> Self {
    Self(PhantomData)
  }

  /// Sends `message`, advancing to the receive phase described by `P::NextRecv`.
  pub fn send<M, NextRecv>(self, message: M) -> (M, SessionRecv<NextRecv>) {
    (message, SessionRecv::new())
  }
}

impl<P> Default for SessionSend<P> {
  fn default() -> Self {
    Self::new()
  }
}

impl<P> SessionRecv<P> {
  /// Constructs the receive-phase token (zero-sized at runtime).
  pub fn new() -> Self {
    Self(PhantomData)
  }

  /// Receives `message`, advancing to the send phase described by `P::NextSend`.
  pub fn recv<M, NextSend>(self, message: M) -> (M, SessionSend<NextSend>) {
    (message, SessionSend::new())
  }
}

impl<P> Default for SessionRecv<P> {
  fn default() -> Self {
    Self::new()
  }
}

/// Describes one protocol step: message type and next phase markers.
pub trait SessionProtocol {
  /// Message exchanged at this step.
  type Message;
  /// Next send-phase marker after this step completes.
  type NextSend;
  /// Next receive-phase marker after this step completes.
  type NextRecv;
}

/// Ping step: client sends `Ping`, server replies `Pong`.
pub struct PingStep;

impl SessionProtocol for PingStep {
  type Message = PingPong;
  type NextSend = PongStep;
  type NextRecv = PongStep;
}

/// Pong step: server sends `Pong`, protocol ends.
pub struct PongStep;

impl SessionProtocol for PongStep {
  type Message = PingPong;
  type NextSend = SessionEnd;
  type NextRecv = SessionEnd;
}

/// Ping/pong message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PingPong {
  /// Client ping.
  Ping,
  /// Server pong.
  Pong,
}

/// Client-side linear ping session starting in send phase.
pub type ClientPing = SessionSend<PingStep>;

/// Server-side linear ping session starting in receive phase.
pub type ServerPing = SessionRecv<PingStep>;

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn defaults_construct_tokens() {
    let _: SessionSend<PingStep> = SessionSend::default();
    let _: SessionRecv<PingStep> = SessionRecv::default();
    let _ = SessionSend::<PingStep>::new();
    let _ = SessionRecv::<PingStep>::new();
  }

  #[test]
  fn server_ping_pong_linear() {
    let recv = ServerPing::new();
    let (_ping, send) = recv.recv::<PingPong, PongStep>(PingPong::Ping);
    let (_pong, _end) = send.send::<PingPong, SessionEnd>(PingPong::Pong);
  }

  #[test]
  fn ping_pong_debug() {
    assert_eq!(format!("{:?}", PingPong::Ping), "Ping");
  }
}

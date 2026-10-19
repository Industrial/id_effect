//! Congestion control per route (ADR 0014 D9): one endpoint carries `h3` on BBR and the mesh on
//! CUBIC, so the algorithm is picked per connection once its ALPN is known.

use std::any::Any;
use std::sync::{Arc, OnceLock};
use std::time::Instant;

use quinn::congestion::{BbrConfig, Controller, ControllerFactory, ControllerMetrics, CubicConfig};
use quinn_proto::RttEstimator;

/// The congestion controller of a QUIC connection ([`Route::with_congestion_control`]).
///
/// [`Route::with_congestion_control`]: super::Route::with_congestion_control
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum CongestionControl {
  /// CUBIC (RFC 9438), quinn's default: the mesh (`idm/1`), every route made with
  /// [`Route::new`](super::Route::new) and [`client_config`](super::client_config).
  #[default]
  Cubic,
  /// BBR (v1), the default of [`Route::h3`](super::Route::h3) and of the HTTP/3 client. Under
  /// random loss it keeps sending at the measured bottleneck rate where CUBIC backs off on every
  /// lost packet, which is what keeps HTTP/3's tail latency below HTTP/2's (ADR 0014 D9).
  /// **quinn marks its BBR implementation experimental**; choose [`Cubic`](Self::Cubic) per
  /// route to opt out.
  Bbr,
}

impl CongestionControl {
  /// A quinn controller factory for this algorithm with quinn's default settings (for a
  /// [`quinn::TransportConfig`] of your own).
  pub fn factory(self) -> Arc<dyn ControllerFactory + Send + Sync> {
    match self {
      Self::Cubic => Arc::new(CubicConfig::default()),
      Self::Bbr => Arc::new(BbrConfig::default()),
    }
  }

  /// The algorithm's name (`cubic`, `bbr`).
  pub fn as_str(self) -> &'static str {
    match self {
      Self::Cubic => "cubic",
      Self::Bbr => "bbr",
    }
  }

  fn build(self, now: Instant, mtu: u16) -> Box<dyn Controller> {
    self.factory().build(now, mtu)
  }
}

impl std::fmt::Display for CongestionControl {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.write_str(self.as_str())
  }
}

/// The algorithm a connection's route asks for, set once the handshake has chosen the ALPN.
#[derive(Debug, Default)]
pub(crate) struct Selection(OnceLock<CongestionControl>);

impl Selection {
  /// Record the route's algorithm (the first call wins).
  pub(crate) fn choose(&self, choice: CongestionControl) {
    let _ = self.0.set(choice);
  }

  fn get(&self) -> Option<CongestionControl> {
    self.0.get().copied()
  }
}

/// Builds the controllers of one server connection: CUBIC until its [`Selection`] is made,
/// then the route's algorithm.
#[derive(Debug)]
pub(crate) struct RouteCongestion {
  selection: Arc<Selection>,
}

impl RouteCongestion {
  pub(crate) fn new(selection: Arc<Selection>) -> Self {
    Self { selection }
  }
}

impl ControllerFactory for RouteCongestion {
  fn build(self: Arc<Self>, now: Instant, current_mtu: u16) -> Box<dyn Controller> {
    let chosen = self.selection.get();
    let active = chosen.unwrap_or_default();
    Box::new(RouteController {
      inner: active.build(now, current_mtu),
      pending: chosen.is_none().then(|| Arc::clone(&self.selection)),
      mtu: current_mtu,
      active,
    })
  }
}

/// A controller that switches to the route's algorithm on its first event after the ALPN is
/// known (during the handshake, before any application data is in flight).
struct RouteController {
  inner: Box<dyn Controller>,
  /// The selection still to apply.
  pending: Option<Arc<Selection>>,
  mtu: u16,
  /// The algorithm running now.
  active: CongestionControl,
}

impl RouteController {
  fn settle(&mut self, now: Instant) {
    let Some(chosen) = self.pending.as_ref().and_then(|s| s.get()) else {
      return;
    };
    if chosen != self.active {
      self.inner = chosen.build(now, self.mtu);
      self.active = chosen;
    }
    self.pending = None;
  }
}

impl Controller for RouteController {
  fn on_sent(&mut self, now: Instant, bytes: u64, last_packet_number: u64) {
    self.settle(now);
    self.inner.on_sent(now, bytes, last_packet_number);
  }

  fn on_ack(
    &mut self,
    now: Instant,
    sent: Instant,
    bytes: u64,
    app_limited: bool,
    rtt: &RttEstimator,
  ) {
    self.settle(now);
    self.inner.on_ack(now, sent, bytes, app_limited, rtt);
  }

  fn on_end_acks(
    &mut self,
    now: Instant,
    in_flight: u64,
    app_limited: bool,
    largest_packet_num_acked: Option<u64>,
  ) {
    self.settle(now);
    self
      .inner
      .on_end_acks(now, in_flight, app_limited, largest_packet_num_acked);
  }

  fn on_congestion_event(
    &mut self,
    now: Instant,
    sent: Instant,
    is_persistent_congestion: bool,
    lost_bytes: u64,
  ) {
    self.settle(now);
    self
      .inner
      .on_congestion_event(now, sent, is_persistent_congestion, lost_bytes);
  }

  fn on_mtu_update(&mut self, new_mtu: u16) {
    self.mtu = new_mtu;
    self.inner.on_mtu_update(new_mtu);
  }

  fn window(&self) -> u64 {
    self.inner.window()
  }

  fn metrics(&self) -> ControllerMetrics {
    self.inner.metrics()
  }

  fn clone_box(&self) -> Box<dyn Controller> {
    Box::new(Self {
      inner: self.inner.clone_box(),
      pending: self.pending.clone(),
      mtu: self.mtu,
      active: self.active,
    })
  }

  fn initial_window(&self) -> u64 {
    self.inner.initial_window()
  }

  fn into_any(self: Box<Self>) -> Box<dyn Any> {
    self
  }
}

#[cfg(test)]
mod tests {
  use quinn::congestion::{Bbr, Cubic};

  use super::*;

  fn controller(selection: &Arc<Selection>) -> Box<dyn Controller> {
    Arc::new(RouteCongestion::new(Arc::clone(selection))).build(Instant::now(), 1200)
  }

  fn active(controller: Box<dyn Controller>) -> (CongestionControl, Box<dyn Any>) {
    let route = controller
      .into_any()
      .downcast::<RouteController>()
      .expect("a route controller");
    (route.active, route.inner.into_any())
  }

  #[test]
  fn cubic_until_the_route_is_known_then_the_routes_choice() {
    let selection = Arc::new(Selection::default());
    let mut c = controller(&selection);
    let now = Instant::now();
    c.on_sent(now, 1200, 0);
    c.on_mtu_update(1400);
    let (running, inner) = active(c.clone_box());
    assert_eq!(running, CongestionControl::Cubic);
    assert!(inner.downcast::<Cubic>().is_ok());

    selection.choose(CongestionControl::Bbr);
    // A second choice is ignored.
    selection.choose(CongestionControl::Cubic);
    // (`on_ack` needs quinn's RTT estimator, which only a live connection makes: the endpoint
    // tests drive it.)
    c.on_end_acks(now, 0, false, Some(0));
    c.on_congestion_event(now, now, false, 1200);
    c.on_sent(now, 1200, 1);
    assert!(c.window() > 0);
    assert!(c.initial_window() > 0);
    assert!(c.metrics().congestion_window > 0);
    let (running, inner) = active(c);
    assert_eq!(running, CongestionControl::Bbr);
    assert!(inner.downcast::<Bbr>().is_ok());
  }

  #[test]
  fn a_controller_built_after_the_choice_starts_with_it() {
    let selection = Arc::new(Selection::default());
    selection.choose(CongestionControl::Bbr);
    let (running, inner) = active(controller(&selection));
    assert_eq!(running, CongestionControl::Bbr);
    assert!(inner.downcast::<Bbr>().is_ok());
    // Choosing what already runs keeps the controller.
    let selection = Arc::new(Selection::default());
    let mut c = controller(&selection);
    selection.choose(CongestionControl::Cubic);
    c.on_congestion_event(Instant::now(), Instant::now(), true, 0);
    let (running, inner) = active(c);
    assert_eq!(running, CongestionControl::Cubic);
    assert!(inner.downcast::<Cubic>().is_ok());
  }

  #[test]
  fn names_and_factories() {
    assert_eq!(CongestionControl::default(), CongestionControl::Cubic);
    assert_eq!(CongestionControl::Bbr.to_string(), "bbr");
    assert_eq!(CongestionControl::Cubic.as_str(), "cubic");
    let bbr = CongestionControl::Bbr.factory().build(Instant::now(), 1200);
    assert!(bbr.into_any().downcast::<Bbr>().is_ok());
  }
}

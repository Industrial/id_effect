//! OTP-style supervision trees: [`ChildSpec`], [`Strategy`], restart
//! intensity on the node's executor clock, and the shutdown protocol.
//!
//! The supervisor **is a process** (it implements [`Process`]): it traps
//! exits, links every child, and reacts to `Info::Exit` messages with the
//! configured strategy. Escalation is plain link propagation — when the
//! restart intensity is exceeded the supervisor stops with
//! [`ExitReason::Shutdown`], which its own supervisor observes like any
//! child exit.
//!
//! ## Shutdown protocol (ADR 0009)
//!
//! - [`ShutdownPolicy::BrutalKill`]: untrappable kill, wait for the exit.
//! - [`ShutdownPolicy::Timeout`]: trappable `Shutdown` signal, wait up to
//!   the deadline on the supervisor fiber's executor clock, then
//!   untrappable kill.
//! - [`ShutdownPolicy::Infinity`]: trappable `Shutdown`, wait forever
//!   (reserved for child supervisors).
//!
//! ## Restarts
//!
//! Restarts are immediate (OTP semantics): the budget is the sliding
//! [`RestartIntensity`] window, not a backoff delay, so this supervisor does
//! not use `next::SupervisorPolicy`'s restart delay or a `Schedule`.

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use id_effect::Never;
use id_effect::next::{Effect, HNil, RestartIntensity, succeed, suspend};

use crate::addr::Addr;
use crate::exit::ExitReason;
use crate::msg::{CallError, Info, StartError};
use crate::node::ProcessNode;
use crate::pid::Pid;
use crate::process::{Next, Process, ProcessCtx};

/// Restart strategy applied when a child exits abnormally.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Strategy {
  /// Restart only the crashed child.
  OneForOne,
  /// Terminate all other children, then restart every non-temporary child.
  OneForAll,
  /// Terminate children started **after** the crashed one, then restart
  /// the crashed child and those siblings.
  RestForOne,
}

/// When a child should be restarted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RestartPolicy {
  /// Always restart.
  Permanent,
  /// Restart only after an abnormal exit (not `Normal` / `Shutdown`).
  Transient,
  /// Never restart.
  Temporary,
}

/// How a child is asked to stop during supervisor shutdown.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShutdownPolicy {
  /// Untrappable kill immediately.
  BrutalKill,
  /// Trappable `Shutdown`, escalating to a kill after the deadline.
  Timeout(Duration),
  /// Trappable `Shutdown`, wait forever (use for child supervisors).
  Infinity,
}

/// Worker or nested supervisor (affects conventional shutdown defaults).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChildKind {
  /// Ordinary process.
  Worker,
  /// Nested supervisor.
  Supervisor,
}

/// Start closure: spawns the child **linked to the supervisor** and
/// returns its pid. Must be re-runnable (restarts call it again).
pub type StartFn = Arc<dyn Fn(&ProcessNode, &Pid) -> Result<Pid, StartError> + Send + Sync>;

/// Declarative child description.
#[derive(Clone)]
pub struct ChildSpec {
  /// Unique id within the supervisor.
  pub id: String,
  /// Re-runnable start closure.
  pub start: StartFn,
  /// Restart policy.
  pub restart: RestartPolicy,
  /// Shutdown protocol.
  pub shutdown: ShutdownPolicy,
  /// Worker or nested supervisor.
  pub kind: ChildKind,
}

impl ChildSpec {
  /// Worker child from a behavior type and a re-runnable `(args, env)`
  /// factory. Defaults: `Permanent`, 5 s trappable shutdown.
  pub fn worker<P, F>(id: impl Into<String>, factory: F) -> Self
  where
    P: Process,
    F: Fn() -> (P::Args, P::Env) + Send + Sync + 'static,
  {
    let start: StartFn = Arc::new(move |node, supervisor| {
      let (args, env) = factory();
      Ok(
        node
          .spawn_linked_to::<P>(supervisor, args, env)
          .pid()
          .clone(),
      )
    });
    Self {
      id: id.into(),
      start,
      restart: RestartPolicy::Permanent,
      shutdown: ShutdownPolicy::Timeout(Duration::from_secs(5)),
      kind: ChildKind::Worker,
    }
  }

  /// Child from a raw start closure.
  pub fn from_start(id: impl Into<String>, start: StartFn) -> Self {
    Self {
      id: id.into(),
      start,
      restart: RestartPolicy::Permanent,
      shutdown: ShutdownPolicy::Timeout(Duration::from_secs(5)),
      kind: ChildKind::Worker,
    }
  }

  /// Nested-supervisor child from a [`SupervisorSpec`] factory.
  pub fn supervisor<F>(id: impl Into<String>, factory: F) -> Self
  where
    F: Fn() -> SupervisorSpec + Send + Sync + 'static,
  {
    let start: StartFn = Arc::new(move |node, supervisor| {
      Ok(
        node
          .spawn_linked_to::<SupervisorProcess>(supervisor, factory(), HNil)
          .pid()
          .clone(),
      )
    });
    Self {
      id: id.into(),
      start,
      restart: RestartPolicy::Permanent,
      shutdown: ShutdownPolicy::Infinity,
      kind: ChildKind::Supervisor,
    }
  }

  /// Override the restart policy.
  pub fn with_restart(mut self, restart: RestartPolicy) -> Self {
    self.restart = restart;
    self
  }

  /// Override the shutdown protocol.
  pub fn with_shutdown(mut self, shutdown: ShutdownPolicy) -> Self {
    self.shutdown = shutdown;
    self
  }
}

/// Declarative supervisor description.
#[derive(Clone)]
pub struct SupervisorSpec {
  /// Restart strategy.
  pub strategy: Strategy,
  /// Maximum restarts allowed inside [`SupervisorSpec::within`].
  pub max_restarts: u32,
  /// Sliding intensity window measured on the node's executor clock.
  pub within: Duration,
  /// Children in start order.
  pub children: Vec<ChildSpec>,
}

impl SupervisorSpec {
  /// Spec with BEAM's defaults (3 restarts / 5 seconds).
  pub fn new(strategy: Strategy) -> Self {
    Self {
      strategy,
      max_restarts: 3,
      within: Duration::from_secs(5),
      children: Vec::new(),
    }
  }

  /// Set the restart intensity.
  pub fn intensity(mut self, max_restarts: u32, within: Duration) -> Self {
    self.max_restarts = max_restarts;
    self.within = within;
    self
  }

  /// Append a child (start order = append order).
  pub fn child(mut self, spec: ChildSpec) -> Self {
    self.children.push(spec);
    self
  }

  /// Start a root supervisor on `node` (unlinked).
  pub fn start(self, node: &ProcessNode) -> SupervisorHandle {
    SupervisorHandle {
      addr: node.spawn::<SupervisorProcess>(self, HNil),
    }
  }

  /// Start a supervisor linked to the calling process.
  pub fn start_link(self, ctx: &ProcessCtx) -> SupervisorHandle {
    SupervisorHandle {
      addr: ctx.spawn_link::<SupervisorProcess>(self, HNil),
    }
  }
}

/// Requests understood by a running supervisor.
pub enum SupCall {
  /// `(child id, current pid)` for every child.
  WhichChildren,
  /// Number of children (running or not).
  CountChildren,
  /// Add and start a new child.
  StartChild(ChildSpec),
  /// Stop a child by id and remove it from the tree.
  TerminateChild(String),
}

/// Replies from a running supervisor.
#[derive(Clone, Debug)]
pub enum SupReply {
  /// Child ids with their live pids.
  Children(Vec<(String, Option<Pid>)>),
  /// Child count.
  Count(usize),
  /// Outcome of `StartChild`.
  Started(Result<Pid, StartError>),
  /// Outcome of `TerminateChild` (`false` if the id was unknown).
  Terminated(bool),
}

/// Supervisor failure (start failures during `init`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SupError {
  /// A child failed to start during supervisor init.
  StartFailed(String, StartError),
}

struct ChildState {
  spec: ChildSpec,
  pid: Option<Pid>,
}

/// The supervisor behavior. Usually driven through [`SupervisorSpec`] and
/// [`SupervisorHandle`] rather than used directly.
pub struct SupervisorProcess {
  strategy: Strategy,
  children: Vec<ChildState>,
  /// Sliding-window restart-intensity limiter. The intensity math is owned by
  /// [`RestartIntensity`]; this process only records restart events on the
  /// node's executor clock and escalates when it reports over-budget.
  intensity: RestartIntensity,
  /// Pids we terminated on purpose — their queued exit messages must not
  /// be mistaken for crashes or parent exits.
  expected_exits: HashSet<Pid>,
}

impl SupervisorProcess {
  fn child_index(&self, pid: &Pid) -> Option<usize> {
    self
      .children
      .iter()
      .position(|c| c.pid.as_ref() == Some(pid))
  }

  fn should_restart(policy: RestartPolicy, reason: &ExitReason) -> bool {
    match policy {
      RestartPolicy::Permanent => true,
      RestartPolicy::Transient => !reason.is_graceful(),
      RestartPolicy::Temporary => false,
    }
  }

  /// Start one child; on failure keep retrying while intensity allows.
  /// Returns `false` when intensity is exhausted.
  fn start_child_with_intensity(&mut self, index: usize, ctx: &ProcessCtx) -> bool {
    let node = ctx.node().clone();
    loop {
      let start = Arc::clone(&self.children[index].spec.start);
      match start(&node, ctx.pid()) {
        Ok(pid) => {
          self.children[index].pid = Some(pid);
          return true;
        }
        Err(_) => {
          if !self.intensity.note(node.now()) {
            return false;
          }
        }
      }
    }
  }

  /// Restart every non-temporary child from `from` on, in start order.
  fn restart_from(mut self, from: usize, ctx: &ProcessCtx) -> Next<Self> {
    for i in from..self.children.len() {
      if self.children[i].spec.restart == RestartPolicy::Temporary {
        continue;
      }
      if !self.start_child_with_intensity(i, ctx) {
        return Next::Stop(ExitReason::Shutdown, self);
      }
    }
    Next::Continue(self)
  }

  /// Stop a running child following its shutdown policy and wait for its
  /// exit (bounded by the policy).
  fn stop_child<R: Send + Sync + 'static>(
    mut self,
    index: usize,
    ctx: &ProcessCtx,
  ) -> Effect<Self, Never, R> {
    let Some(pid) = self.children[index].pid.take() else {
      return succeed(self);
    };
    self.expected_exits.insert(pid.clone());
    let shutdown = self.children[index].spec.shutdown;
    terminate_pid(ctx.node(), &pid, shutdown).map(move |()| self)
  }

  /// Stop the children at `indices`, one after the other, in that order.
  fn stop_children<R: Send + Sync + 'static>(
    self,
    indices: Vec<usize>,
    ctx: &ProcessCtx,
  ) -> Effect<Self, Never, R> {
    indices.into_iter().fold(succeed(self), |acc, i| {
      let ctx = ctx.clone();
      acc.flat_map(move |s| s.stop_child(i, &ctx))
    })
  }
}

/// Ask `pid` to stop per `policy` and wait until it is fully finalized.
/// The exit signal is sent when the effect runs.
pub(crate) fn terminate_pid<R: Send + Sync + 'static>(
  node: &ProcessNode,
  pid: &Pid,
  policy: ShutdownPolicy,
) -> Effect<(), Never, R> {
  let node = node.clone();
  let pid = pid.clone();
  suspend(move || {
    let Some(cell) = node.table().lookup(&pid) else {
      return succeed(());
    };
    let exited = cell.exited();
    match policy {
      ShutdownPolicy::BrutalKill => {
        node.exit(&pid, ExitReason::Killed);
        exited.wait().map(|_| ())
      }
      ShutdownPolicy::Infinity => {
        node.exit(&pid, ExitReason::Shutdown);
        exited.wait().map(|_| ())
      }
      ShutdownPolicy::Timeout(timeout) => {
        node.exit(&pid, ExitReason::Shutdown);
        exited
          .wait()
          .timeout(timeout)
          .flat_map(move |done| match done {
            Some(_) => succeed(()),
            None => {
              node.exit(&pid, ExitReason::Killed);
              exited.wait().map(|_| ())
            }
          })
      }
    }
  })
}

impl Process for SupervisorProcess {
  type Args = SupervisorSpec;
  type Env = HNil;
  type Call = SupCall;
  type Reply = SupReply;
  type Cast = ();
  type Info = ();
  type Error = SupError;

  fn init(spec: SupervisorSpec, ctx: ProcessCtx) -> Effect<Self, SupError, HNil> {
    suspend(move || {
      ctx.trap_exit(true);
      let mut state = SupervisorProcess {
        strategy: spec.strategy,
        children: spec
          .children
          .into_iter()
          .map(|spec| ChildState { spec, pid: None })
          .collect(),
        intensity: RestartIntensity::new(spec.max_restarts, spec.within),
        expected_exits: HashSet::new(),
      };
      let node = ctx.node().clone();
      for index in 0..state.children.len() {
        let start = Arc::clone(&state.children[index].spec.start);
        match start(&node, ctx.pid()) {
          Ok(pid) => state.children[index].pid = Some(pid),
          Err(err) => {
            let id = state.children[index].spec.id.clone();
            return id_effect::next::fail(SupError::StartFailed(id, err));
          }
        }
      }
      succeed(state)
    })
  }

  fn handle_call(
    mut self,
    msg: SupCall,
    ctx: ProcessCtx,
  ) -> Effect<(SupReply, Next<Self>), SupError, HNil> {
    let reply = match msg {
      SupCall::WhichChildren => SupReply::Children(
        self
          .children
          .iter()
          .map(|c| (c.spec.id.clone(), c.pid.clone()))
          .collect(),
      ),
      SupCall::CountChildren => SupReply::Count(self.children.len()),
      SupCall::StartChild(spec) => {
        let start = Arc::clone(&spec.start);
        match start(ctx.node(), ctx.pid()) {
          Ok(pid) => {
            self.children.push(ChildState {
              spec,
              pid: Some(pid.clone()),
            });
            SupReply::Started(Ok(pid))
          }
          Err(err) => SupReply::Started(Err(err)),
        }
      }
      SupCall::TerminateChild(id) => {
        return match self.children.iter().position(|c| c.spec.id == id) {
          Some(index) => self.stop_child(index, &ctx).widen().map(move |mut s| {
            s.children.remove(index);
            (SupReply::Terminated(true), Next::Continue(s))
          }),
          None => succeed((SupReply::Terminated(false), Next::Continue(self))),
        };
      }
    };
    succeed((reply, Next::Continue(self)))
  }

  fn handle_info(mut self, msg: Info<()>, ctx: ProcessCtx) -> Effect<Next<Self>, SupError, HNil> {
    let Info::Exit { from, reason } = msg else {
      return succeed(Next::Continue(self));
    };

    // Exit we caused on purpose (shutdown protocol) — already handled.
    if self.expected_exits.remove(&from) {
      return succeed(Next::Continue(self));
    }

    let Some(index) = self.child_index(&from) else {
      // Not a child: parent or linked peer. Graceful `Normal` is
      // ignored; anything else stops the whole tree with that reason.
      if reason.is_abnormal() {
        return succeed(Next::Stop(reason, self));
      }
      return succeed(Next::Continue(self));
    };

    self.children[index].pid = None;

    if !Self::should_restart(self.children[index].spec.restart, &reason) {
      return succeed(Next::Continue(self));
    }

    if !self.intensity.note(ctx.node().now()) {
      return succeed(Next::Stop(ExitReason::Shutdown, self));
    }

    match self.strategy {
      Strategy::OneForOne => succeed(if self.start_child_with_intensity(index, &ctx) {
        Next::Continue(self)
      } else {
        Next::Stop(ExitReason::Shutdown, self)
      }),
      Strategy::OneForAll => {
        let others = (0..self.children.len())
          .rev()
          .filter(|&i| i != index)
          .collect();
        self
          .stop_children(others, &ctx)
          .widen()
          .map(move |s| s.restart_from(0, &ctx))
      }
      Strategy::RestForOne => {
        let later = ((index + 1)..self.children.len()).rev().collect();
        self
          .stop_children(later, &ctx)
          .widen()
          .map(move |s| s.restart_from(index, &ctx))
      }
    }
  }

  fn terminate(self, _reason: &ExitReason, ctx: ProcessCtx) -> Effect<(), Never, HNil> {
    let all = (0..self.children.len()).rev().collect();
    self.stop_children(all, &ctx).map(|_| ())
  }
}

/// Typed handle to a running supervisor.
#[derive(Clone)]
pub struct SupervisorHandle {
  addr: Addr<SupervisorProcess>,
}

impl SupervisorHandle {
  /// The supervisor's pid.
  pub fn pid(&self) -> &Pid {
    self.addr.pid()
  }

  /// The raw typed address.
  pub fn addr(&self) -> &Addr<SupervisorProcess> {
    &self.addr
  }

  /// `(child id, live pid)` pairs, in start order.
  pub fn which_children<R: Send + Sync + 'static>(
    &self,
    timeout: Duration,
  ) -> Effect<Vec<(String, Option<Pid>)>, CallError, R> {
    self
      .addr
      .call(SupCall::WhichChildren, timeout)
      .map(|reply| match reply {
        SupReply::Children(children) => children,
        _ => Vec::new(),
      })
  }

  /// Number of children.
  pub fn count_children<R: Send + Sync + 'static>(
    &self,
    timeout: Duration,
  ) -> Effect<usize, CallError, R> {
    self
      .addr
      .call(SupCall::CountChildren, timeout)
      .map(|reply| match reply {
        SupReply::Count(n) => n,
        _ => 0,
      })
  }

  /// Add and start a new child.
  pub fn start_child<R: Send + Sync + 'static>(
    &self,
    spec: ChildSpec,
    timeout: Duration,
  ) -> Effect<Result<Pid, StartError>, CallError, R> {
    self
      .addr
      .call(SupCall::StartChild(spec), timeout)
      .map(|reply| match reply {
        SupReply::Started(result) => result,
        _ => Err(StartError::Failed("unexpected reply".to_string())),
      })
  }

  /// Stop and remove a child by id.
  pub fn terminate_child<R: Send + Sync + 'static>(
    &self,
    id: impl Into<String>,
    timeout: Duration,
  ) -> Effect<bool, CallError, R> {
    self
      .addr
      .call(SupCall::TerminateChild(id.into()), timeout)
      .map(|reply| matches!(reply, SupReply::Terminated(true)))
  }

  /// Ask the whole tree to stop per `policy` and wait until it is fully
  /// finalized.
  pub fn stop<E: Send + 'static, R: Send + Sync + 'static>(
    &self,
    node: &ProcessNode,
    policy: ShutdownPolicy,
  ) -> Effect<(), E, R> {
    terminate_pid(node, self.addr.pid(), policy).widen()
  }
}

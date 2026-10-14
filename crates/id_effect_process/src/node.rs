//! [`ProcessNode`] — the node-local process runtime: spawning, links,
//! monitors, exit signals, and the name registry.
//!
//! ## Where this crate blocks (`run_blocking` audit)
//!
//! Message passing between processes is effect-native ([`Addr::call`],
//! [`Addr::cast`], [`Addr::send`] return effects). The synchronous entry
//! points — [`ProcessNode::spawn`], [`ProcessNode::link`], monitors,
//! [`ProcessNode::exit`], [`Addr::send_now`] — interpret a few effects in
//! place, all through the crate-private `run_mailbox_op`:
//!
//! - creating a mailbox ([`Queue::unbounded`]) or a one-shot exit cell
//!   (`Deferred::make`): pure constructors that complete on the first poll;
//! - enqueueing into / closing an **unbounded** mailbox: completes on the
//!   first poll unless another thread is inside the mailbox's internal lock,
//!   whose critical section never awaits — the spin then lasts only as long
//!   as that other thread's non-blocking enqueue.
//!
//! None of them waits for another *process* to make progress, which is what
//! makes them usable from inside a current-thread async runtime (covered by
//! the `current_thread_runtime` tests). Known residual hazard: the mailbox
//! lock is fair, so if an *async* [`Addr::send`] / [`Addr::cast`] for the same
//! mailbox is already parked on it on the **same** current-thread runtime
//! (which needs a third thread holding the lock at that instant), the lock is
//! handed to that parked task and this thread spins until the runtime polls
//! it — which it cannot, so it deadlocks. From async code prefer the
//! effect-returning APIs, which never spin. (A non-async `try_offer` on
//! `id_effect::Queue` would remove the hazard; tracked for core.)
//!
//! The one long-running interpretation is the process loop itself: the job
//! handed to the [`Spawner`] drives the whole loop with `run_blocking`, so it
//! must run on a worker dedicated to that process (an OS thread for
//! [`ProcessNode::threads`], a fiber for [`ProcessNode::on_runtime`]).

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use id_effect::runtime::Never;
use id_effect::scheduling::Clock;
use id_effect::{CancellationToken, Effect, Queue, Runtime, run_blocking, succeed};

use crate::addr::Addr;
use crate::cell::{MonitorSink, ProcessCell, ProcessTable, RegistryEntry};
use crate::clock_util::SpinClock;
use crate::exit::ExitReason;
use crate::msg::{ProcMsg, SysMsg};
use crate::pid::{MonitorRef, NodeName, Pid};
use crate::process::{Process, ProcessCtx, process_loop};

/// Interpret a short, non-waiting mailbox/cell effect in place (see the
/// module docs for why this is bounded and when it is not).
pub(crate) fn run_mailbox_op<A: 'static, E: 'static>(op: Effect<A, E, ()>) -> Result<A, E> {
  run_blocking(op, ())
}

/// A unit of work executed on some worker (one long-lived job per process).
pub type Job = Box<dyn FnOnce() + Send>;

/// How process-loop jobs are executed.
pub type Spawner = Arc<dyn Fn(Job) + Send + Sync>;

struct NodeInner {
  name: NodeName,
  incarnation: u32,
  next_pid: AtomicU64,
  table: ProcessTable,
  clock: Arc<dyn Clock + Send + Sync>,
  spawner: Spawner,
}

/// Node-local process runtime. Cheap to clone (an `Arc` handle).
///
/// One `ProcessNode` per logical node. The distribution layer
/// (`id_effect_node`) wraps this with remote pids and a wire protocol; the
/// local semantics here are the normative ones from ADR 0009.
#[derive(Clone)]
pub struct ProcessNode {
  inner: Arc<NodeInner>,
}

impl ProcessNode {
  /// Build a node with a custom job spawner and clock.
  ///
  /// The spawner runs each process loop via [`run_blocking`]-compatible
  /// polling; it must dedicate a worker (thread) per job for the lifetime
  /// of the process.
  pub fn with_spawner(
    name: NodeName,
    clock: Arc<dyn Clock + Send + Sync>,
    spawner: Spawner,
  ) -> Self {
    Self {
      inner: Arc::new(NodeInner {
        name,
        incarnation: 0,
        next_pid: AtomicU64::new(1),
        table: ProcessTable::new(),
        clock,
        spawner,
      }),
    }
  }

  /// Thread-per-process node: each process loop gets a dedicated OS
  /// thread (the pragmatic BEAM analogue for moderate process counts).
  pub fn threads(name: NodeName) -> Self {
    Self::with_spawner(
      name,
      Arc::new(SpinClock),
      Arc::new(|job: Job| {
        std::thread::Builder::new()
          .name("id-effect-process".to_string())
          .spawn(job)
          .expect("failed to spawn process thread");
      }),
    )
  }

  /// Node whose process loops run as fibers on an id_effect [`Runtime`].
  ///
  /// Note: processes are long-lived fibers; make sure the runtime's worker
  /// pool is sized for the number of concurrent processes (a fixed-size
  /// fabric pool can starve if every worker hosts a process).
  pub fn on_runtime<RT>(name: NodeName, runtime: RT) -> Self
  where
    RT: Runtime + Clone + Send + Sync + 'static,
  {
    let clock = Arc::new(id_effect::scheduling::LiveClock::new(runtime.clone()));
    Self::with_spawner(
      name,
      clock,
      Arc::new(move |job: Job| {
        runtime.spawn_with(move || {
          job();
          (succeed::<(), Never, ()>(()), ())
        });
      }),
    )
  }

  /// This node's name.
  pub fn name(&self) -> &NodeName {
    &self.inner.name
  }

  /// Live-process table (read-only introspection).
  pub fn table(&self) -> &ProcessTable {
    &self.inner.table
  }

  /// The clock used for call timeouts and supervision intensity windows.
  pub fn clock(&self) -> Arc<dyn Clock + Send + Sync> {
    Arc::clone(&self.inner.clock)
  }

  // ── Spawning ────────────────────────────────────────────

  /// Spawn an unlinked process. Returns immediately with a typed address;
  /// `init` runs asynchronously inside the new process.
  pub fn spawn<P: Process>(&self, args: P::Args, env: P::Env) -> Addr<P> {
    self.spawn_inner::<P>(args, env, None)
  }

  /// Spawn a process bidirectionally linked to `parent`.
  ///
  /// The link is installed **before** the child starts, so no exit can be
  /// missed (BEAM's `spawn_link` atomicity).
  pub fn spawn_linked_to<P: Process>(&self, parent: &Pid, args: P::Args, env: P::Env) -> Addr<P> {
    self.spawn_inner::<P>(args, env, Some(parent.clone()))
  }

  fn spawn_inner<P: Process>(&self, args: P::Args, env: P::Env, link_to: Option<Pid>) -> Addr<P> {
    let id = self.inner.next_pid.fetch_add(1, Ordering::Relaxed);
    let pid = Pid::new(self.inner.name.clone(), id, self.inner.incarnation);

    let mailbox: Queue<ProcMsg<P>> = run_mailbox_op(Queue::unbounded())
      .unwrap_or_else(|_| unreachable!("Queue::unbounded is infallible"));

    let deliver = {
      let mailbox = mailbox.clone();
      Box::new(move |sys: SysMsg| {
        run_mailbox_op(mailbox.offer(ProcMsg::<P>::Info(sys.into()))).unwrap_or(false)
      })
    };
    let close = {
      let mailbox = mailbox.clone();
      Box::new(move || {
        let _ = run_mailbox_op(mailbox.shutdown());
      })
    };

    let cell = Arc::new(ProcessCell::new(
      pid.clone(),
      deliver,
      close,
      CancellationToken::new(),
    ));
    self.inner.table.insert(Arc::clone(&cell));

    if let Some(parent) = link_to {
      self.link(&parent, &pid);
    }

    let ctx = ProcessCtx::new(pid.clone(), self.clone());
    let addr = Addr::new(pid, mailbox.clone(), self.clone());

    // The only long-running interpretation: the spawner dedicates a worker
    // to this job for the lifetime of the process (see the module docs).
    let job: Job = Box::new(move || {
      let effect = process_loop::<P>(args, ctx, mailbox);
      let _ = run_blocking(effect, env);
    });
    (self.inner.spawner)(job);

    addr
  }

  // ── Links ───────────────────────────────────────────────

  /// Bidirectionally link `a` and `b`. Linking to a dead process delivers
  /// an immediate `noproc` exit signal to the survivor (BEAM semantics).
  pub fn link(&self, a: &Pid, b: &Pid) {
    let cell_a = self.inner.table.lookup(a);
    let cell_b = self.inner.table.lookup(b);
    match (cell_a, cell_b) {
      (Some(ca), Some(cb)) => {
        let ra = ca.add_link(b.clone());
        let rb = cb.add_link(a.clone());
        // A peer finalized between lookup and add_link: deliver its exit.
        if let Err(reason) = ra {
          self.deliver_link_exit(b, a.clone(), reason);
        }
        if let Err(reason) = rb {
          self.deliver_link_exit(a, b.clone(), reason);
        }
      }
      (Some(_), None) => self.deliver_link_exit(a, b.clone(), ExitReason::NoProc),
      (None, Some(_)) => self.deliver_link_exit(b, a.clone(), ExitReason::NoProc),
      (None, None) => {}
    }
  }

  /// Remove the link between `a` and `b` (both directions).
  pub fn unlink(&self, a: &Pid, b: &Pid) {
    if let Some(ca) = self.inner.table.lookup(a) {
      ca.remove_link(b);
    }
    if let Some(cb) = self.inner.table.lookup(b) {
      cb.remove_link(a);
    }
  }

  /// Deliver a link exit signal to `to`, as if `from` exited with `reason`.
  fn deliver_link_exit(&self, to: &Pid, from: Pid, reason: ExitReason) {
    let Some(cell) = self.inner.table.lookup(to) else {
      return;
    };
    cell.remove_link(&from);
    if cell.traps_exits() {
      cell.deliver_sys(SysMsg::Exit { from, reason });
    } else if reason.is_abnormal() {
      cell.kill(reason);
    }
  }

  // ── Monitors ────────────────────────────────────────────

  /// Monitor `target`, delivering `Down` into `watcher`'s mailbox.
  /// Monitoring a dead process fires `Down(noproc)` immediately.
  pub fn monitor_into(&self, watcher: &Pid, target: &Pid) -> MonitorRef {
    let table_watcher = self.inner.table.lookup(watcher);
    let sink: MonitorSink = Box::new(move |monitor, pid, reason| {
      if let Some(w) = &table_watcher {
        w.deliver_sys(SysMsg::Down {
          monitor,
          pid,
          reason,
        });
      }
    });
    self.monitor_with_sink(target, sink)
  }

  /// Monitor `target` with a raw callback (used by `call` rendezvous and
  /// the distribution layer). Fires exactly once.
  pub fn monitor_with_sink(&self, target: &Pid, sink: MonitorSink) -> MonitorRef {
    match self.inner.table.lookup(target) {
      Some(cell) => cell.add_monitor(sink),
      None => {
        let monitor = MonitorRef::fresh();
        sink(monitor, target.clone(), ExitReason::NoProc);
        monitor
      }
    }
  }

  /// Remove a monitor on `target` before it fires.
  pub fn demonitor(&self, target: &Pid, monitor: MonitorRef) {
    if let Some(cell) = self.inner.table.lookup(target) {
      cell.remove_monitor(monitor);
    }
  }

  // ── Exit signals ─────────────────────────────────────────

  /// Send an exit signal (BEAM's `exit/2`).
  ///
  /// [`ExitReason::Killed`] is untrappable: the target dies regardless of
  /// `trap_exit`. Other abnormal reasons are trappable; `Normal` only
  /// arrives as a message when the target traps exits.
  pub fn exit(&self, target: &Pid, reason: ExitReason) {
    let Some(cell) = self.inner.table.lookup(target) else {
      return;
    };
    match reason {
      ExitReason::Killed => cell.kill(ExitReason::Killed),
      _ if cell.traps_exits() => {
        cell.deliver_sys(SysMsg::Exit {
          from: target.clone(),
          reason,
        });
      }
      _ if reason.is_abnormal() => cell.kill(reason),
      _ => {}
    }
  }

  /// Finalize a dead process: fan out link exits, fire monitors, drop
  /// registered names, remove from the table. Called exactly once by the
  /// process loop.
  pub(crate) fn finalize_process(&self, pid: &Pid, reason: ExitReason) {
    let Some(cell) = self.inner.table.lookup(pid) else {
      return;
    };
    let (links, monitors, names) = cell.finalize(&reason);
    self.inner.table.remove(pid);

    {
      let mut registry = self
        .inner
        .table
        .registry()
        .lock()
        .expect("registry mutex poisoned");
      for name in names {
        registry.remove(&name);
      }
    }

    for peer in links {
      self.deliver_link_exit(&peer, pid.clone(), reason.clone());
    }
    for (monitor, sink) in monitors {
      sink(monitor, pid.clone(), reason.clone());
    }
  }

  // ── Name registry ────────────────────────────────────────

  /// Register `addr` under `name`. Fails if the name is taken or the
  /// process is dead. Names are cleaned up automatically on exit.
  pub fn register<P: Process>(&self, name: impl Into<String>, addr: &Addr<P>) -> bool {
    let name = name.into();
    let Some(cell) = self.inner.table.lookup(addr.pid()) else {
      return false;
    };
    let mut registry = self
      .inner
      .table
      .registry()
      .lock()
      .expect("registry mutex poisoned");
    if registry.contains_key(&name) {
      return false;
    }
    registry.insert(
      name.clone(),
      RegistryEntry {
        pid: addr.pid().clone(),
        addr: Box::new(addr.clone()),
      },
    );
    drop(registry);
    cell.note_registered_name(name);
    true
  }

  /// Pid registered under `name`, if any.
  pub fn whereis(&self, name: &str) -> Option<Pid> {
    self
      .inner
      .table
      .registry()
      .lock()
      .expect("registry mutex poisoned")
      .get(name)
      .map(|entry| entry.pid.clone())
  }

  /// Typed address registered under `name`, if the type matches.
  pub fn whereis_addr<P: Process>(&self, name: &str) -> Option<Addr<P>> {
    self
      .inner
      .table
      .registry()
      .lock()
      .expect("registry mutex poisoned")
      .get(name)
      .and_then(|entry| entry.addr.downcast_ref::<Addr<P>>().cloned())
  }

  /// Remove a name registration (the process keeps running).
  pub fn unregister(&self, name: &str) {
    let removed = self
      .inner
      .table
      .registry()
      .lock()
      .expect("registry mutex poisoned")
      .remove(name);
    if let Some(entry) = removed
      && let Some(cell) = self.inner.table.lookup(&entry.pid)
    {
      cell.forget_registered_name(name);
    }
  }
}

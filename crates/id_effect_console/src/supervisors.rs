//! `/_console/api/supervisors`: the registered supervisors' trees, from `which_children`, as
//! `id_effect_node` [`TreeNodeView`]s.
//!
//! A supervisor only reports `(child id, live pid)` per child, so the console nests the
//! supervisors it was given: a child whose pid is another registered supervisor's pid is expanded
//! into that supervisor's tree; every other child is a leaf. A registered supervisor that is no
//! registered supervisor's child is a root.

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use id_effect::{Cause, Exit, HNil};
use id_effect_axum::EffectExecutor;
use id_effect_node::control::TreeNodeView;
use id_effect_process::{CallError, Pid, SupervisorHandle};
use serde::{Deserialize, Serialize};

/// What `GET /_console/api/supervisors` answers.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct SupervisorsView {
  /// One tree per root supervisor, in registration order: the node's `id` is the name it was
  /// registered under (or its child id in its parent), `pid` its pid, `children` its children
  /// in start order (`pid` `None` while a child is not running).
  pub roots: Vec<TreeNodeView>,
  /// Registered supervisors that did not answer `which_children` (stopped, or past the call
  /// timeout).
  pub unreachable: Vec<UnreachableSupervisor>,
}

/// A registered supervisor that did not answer.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnreachableSupervisor {
  /// The name it was registered under.
  pub id: String,
  /// Its pid.
  pub pid: String,
  /// Why the call failed.
  pub error: String,
}

type Children = Vec<(String, Option<Pid>)>;

fn call_error(cause: Cause<CallError>) -> String {
  match cause {
    Cause::Fail(e) => format!("{e:?}"),
    other => format!("{other:?}"),
  }
}

/// Asks every supervisor in `supervisors` for its children, then nests them.
pub(crate) async fn snapshot(
  supervisors: &[(String, SupervisorHandle)],
  timeout: Duration,
) -> SupervisorsView {
  let mut answers: Vec<Option<Children>> = Vec::with_capacity(supervisors.len());
  let mut unreachable = Vec::new();
  for (id, handle) in supervisors {
    let call = handle.which_children::<HNil>(timeout);
    match EffectExecutor::global().run_effect(call).await {
      Exit::Success(children) => answers.push(Some(children)),
      Exit::Failure(cause) => {
        answers.push(None);
        unreachable.push(UnreachableSupervisor {
          id: id.clone(),
          pid: handle.pid().to_string(),
          error: call_error(cause),
        });
      }
    }
  }
  SupervisorsView {
    roots: nest(supervisors, &answers),
    unreachable,
  }
}

/// The trees of the answering supervisors in `supervisors` (`answers[i]` is the `i`-th one's
/// children).
fn nest(
  supervisors: &[(String, SupervisorHandle)],
  answers: &[Option<Children>],
) -> Vec<TreeNodeView> {
  let by_pid: HashMap<&Pid, usize> = supervisors
    .iter()
    .enumerate()
    .filter(|(i, _)| answers[*i].is_some())
    .map(|(i, (_, handle))| (handle.pid(), i))
    .collect();
  let nested: HashSet<usize> = answers
    .iter()
    .flatten()
    .flatten()
    .filter_map(|(_, pid)| pid.as_ref().and_then(|p| by_pid.get(p).copied()))
    .collect();
  let tree = Tree {
    supervisors,
    answers,
    by_pid,
  };
  (0..supervisors.len())
    .filter(|i| answers[*i].is_some() && !nested.contains(i))
    .map(|i| tree.node(i, supervisors[i].0.clone(), &mut HashSet::new()))
    .collect()
}

struct Tree<'a> {
  supervisors: &'a [(String, SupervisorHandle)],
  answers: &'a [Option<Children>],
  by_pid: HashMap<&'a Pid, usize>,
}

impl Tree<'_> {
  /// Supervisor `i` as a node named `id`; `path` holds the supervisors above it, so a cycle
  /// (which a live tree never has) stops at a leaf.
  fn node(&self, i: usize, id: String, path: &mut HashSet<usize>) -> TreeNodeView {
    path.insert(i);
    let children = self.answers[i]
      .iter()
      .flatten()
      .map(|(child, pid)| {
        let sup = pid.as_ref().and_then(|p| self.by_pid.get(p).copied());
        match sup {
          Some(j) if !path.contains(&j) => self.node(j, child.clone(), path),
          _ => TreeNodeView {
            id: child.clone(),
            pid: pid.as_ref().map(ToString::to_string),
            children: Vec::new(),
          },
        }
      })
      .collect();
    path.remove(&i);
    TreeNodeView {
      id,
      pid: Some(self.supervisors[i].1.pid().to_string()),
      children,
    }
  }
}

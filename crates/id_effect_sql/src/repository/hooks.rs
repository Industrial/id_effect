//! Entity lifecycle hooks (data.orm #7, ADR 0015 D12): [`EntityHooks`] registered on one
//! repository ([`Repository::with_hooks`]) run inside the write's transaction.
//!
//! - **Events.** `before_insert`, `after_insert`, `before_update`, `after_update`,
//!   `before_delete` and `after_delete`. A `before_insert` / `before_update` hook receives the
//!   entity by value and returns the entity to write (it may change it, e.g. normalise a field);
//!   an `after_*` hook sees the stored entity (or the deleted id). The delete hooks receive the
//!   primary key, and `after_delete` runs only when a row was deleted. Several hooks for one event
//!   run in the order they were registered, each after the previous one's effect finished.
//! - **One transaction.** [`HookedRepository::insert`] / [`update`](HookedRepository::update) /
//!   [`delete`](HookedRepository::delete) / [`hard_delete`](HookedRepository::hard_delete) open
//!   a transaction on the [`SqlClientService`] in the environment and run the `before` hooks, the
//!   statement and the `after` hooks in it; inside a transaction the caller already has,
//!   [`HookedRepository::in_transaction`] runs them in a savepoint of it. Either way a failing
//!   hook (or statement) rolls the write and every hook's statements back, also when the caller
//!   catches the error and commits the rest of its transaction.
//! - **Typed errors.** The hooks of an `EntityHooks<T, E>` fail with `E` (any error with
//!   `From<SqlError>`; the default is [`SqlError`]), so a `before_*` hook rejects a write with
//!   the caller's own error type, and the hooked repository's writes fail with `E`.
//! - **Tenant scoping is kept.** A hook receives a [`HookContext`]: the repository bound to the
//!   write's transaction ([`HookContext::repository`]) with the hooked repository's tenant and
//!   filters, so its reads and writes stay in the tenant, and the transaction itself
//!   ([`HookContext::transaction`]) for statements on other tables (an outbox row, an audit
//!   record). Lifting the filters takes the same explicit [`Repository::unscoped`] as anywhere
//!   else.
//! - **Per repository.** Hooks are not global: the plain [`Repository`] writes without them, and
//!   two hooked repositories of one entity can carry different hooks.
//!
//! The audit log (feature `audit`) keeps its explicit `AuditLog::append(&tx, ..)`; to audit every
//! write of an entity, call it from an `after_*` hook with [`HookContext::transaction`], so the
//! record commits or rolls back with the write. With feature `outbox`, `EntityOutbox`
//! (`repository::outbox`) is a ready-made `after_*` hook that stages an event in the
//! transactional outbox.
//!
//! ```rust
//! use id_effect::{Exit, fail, succeed};
//! use id_effect_sql::repository::EntityHooks;
//! use id_effect_sql::{
//!   Dialect, Entity, FromRow, Repository, SqlClient, SqlClientEnv, SqlError, TestSqlClient,
//!   sql_client_env,
//! };
//!
//! #[derive(Clone, Debug, PartialEq, FromRow, Entity)]
//! #[entity(table = "accounts")]
//! struct Account {
//!   #[entity(id)]
//!   id: i64,
//!   name: String,
//! }
//!
//! #[derive(Debug, PartialEq)]
//! enum AccountError {
//!   Sql(SqlError),
//!   EmptyName,
//! }
//!
//! impl From<SqlError> for AccountError {
//!   fn from(e: SqlError) -> Self {
//!     AccountError::Sql(e)
//!   }
//! }
//!
//! let hooks = EntityHooks::<Account, AccountError>::new().before_insert(|_ctx, account| {
//!   if account.name.is_empty() {
//!     fail(AccountError::EmptyName)
//!   } else {
//!     succeed(account)
//!   }
//! });
//! let accounts = Repository::<Account>::new(Dialect::Postgres).with_hooks(hooks);
//!
//! let db = TestSqlClient::new();
//! let _ = db.connect().block_on();
//! let insert = accounts
//!   .insert::<SqlClientEnv, _>(Account { id: 1, name: String::new() })
//!   .provide_all(sql_client_env(db.clone()));
//! assert!(matches!(insert.block_on(), Exit::Failure(c) if format!("{c:?}").contains("EmptyName")));
//! // The hook ran in the transaction the repository opened, and its failure rolled it back.
//! assert_eq!((db.commits(), db.rollbacks()), (0, 1));
//! assert!(db.recorded_queries().is_empty());
//! ```

use std::fmt;
use std::sync::Arc;

use id_effect::{Effect, HList, Selector, fail, succeed};

use super::{BoundRepository, Entity, Repository, Run, SqlExecutor};
use crate::client::SqlClientService;
use crate::error::SqlError;
use crate::query_builder::Dialect;
use crate::transaction::{SqlTransaction, SqlTransactionService, savepoint, transaction};
use crate::value::SqlValue;

/// A `before_insert` / `before_update` hook: the entity in, the entity to write out.
type BeforeHook<T, E> = Arc<dyn Fn(&HookContext<T>, T) -> Effect<T, E> + Send + Sync>;
/// An `after_insert` / `after_update` hook: the stored entity.
type AfterHook<T, E> = Arc<dyn Fn(&HookContext<T>, &T) -> Effect<(), E> + Send + Sync>;
/// A `before_delete` / `after_delete` hook: the primary key.
type IdHook<T, E> = Arc<dyn Fn(&HookContext<T>, &<T as Entity>::Id) -> Effect<(), E> + Send + Sync>;

/// What a hook works with: the write's transaction and the hooked repository bound to it.
pub struct HookContext<T> {
  repo: BoundRepository<T>,
  tx: Arc<dyn SqlTransaction>,
}

impl<T> Clone for HookContext<T> {
  fn clone(&self) -> Self {
    Self {
      repo: self.repo.clone(),
      tx: Arc::clone(&self.tx),
    }
  }
}

impl<T> fmt::Debug for HookContext<T> {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("HookContext")
      .field("repository", &self.repo.repo)
      .finish_non_exhaustive()
  }
}

impl<T: Entity> HookContext<T> {
  fn new(repo: &Repository<T>, tx: &Arc<dyn SqlTransaction>) -> Self {
    Self {
      repo: repo.on(SqlTransactionService(Arc::clone(tx))),
      tx: Arc::clone(tx),
    }
  }

  /// The hooked repository bound to the write's transaction: the same dialect, tenant and
  /// filters, so what a hook reads or writes through it stays in the tenant.
  #[inline]
  pub fn repository(&self) -> &BoundRepository<T> {
    &self.repo
  }

  /// The write's transaction, for statements on other tables (an outbox row, an audit record):
  /// they commit or roll back with the write. Raw statements are not tenant-filtered.
  #[inline]
  pub fn transaction(&self) -> &Arc<dyn SqlTransaction> {
    &self.tx
  }

  /// The dialect the repository renders statements in.
  #[inline]
  pub fn dialect(&self) -> Dialect {
    self.repo.repo.dialect
  }

  /// The tenant the repository is scoped to, if any.
  #[inline]
  pub fn tenant(&self) -> Option<&SqlValue> {
    self.repo.repo.tenant.as_ref()
  }
}

/// The lifecycle hooks of the entity `T`, failing with `E` (see the [module docs](self)). Build
/// it with the event methods, then register it on a repository with
/// [`Repository::with_hooks`]. Cloning shares the hook functions.
pub struct EntityHooks<T: Entity, E = SqlError> {
  before_insert: Vec<BeforeHook<T, E>>,
  after_insert: Vec<AfterHook<T, E>>,
  before_update: Vec<BeforeHook<T, E>>,
  after_update: Vec<AfterHook<T, E>>,
  before_delete: Vec<IdHook<T, E>>,
  after_delete: Vec<IdHook<T, E>>,
}

impl<T: Entity, E> Clone for EntityHooks<T, E> {
  fn clone(&self) -> Self {
    Self {
      before_insert: self.before_insert.clone(),
      after_insert: self.after_insert.clone(),
      before_update: self.before_update.clone(),
      after_update: self.after_update.clone(),
      before_delete: self.before_delete.clone(),
      after_delete: self.after_delete.clone(),
    }
  }
}

impl<T: Entity, E> Default for EntityHooks<T, E> {
  fn default() -> Self {
    Self {
      before_insert: Vec::new(),
      after_insert: Vec::new(),
      before_update: Vec::new(),
      after_update: Vec::new(),
      before_delete: Vec::new(),
      after_delete: Vec::new(),
    }
  }
}

impl<T: Entity, E> fmt::Debug for EntityHooks<T, E> {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("EntityHooks")
      .field("entity", &T::NAME)
      .field("before_insert", &self.before_insert.len())
      .field("after_insert", &self.after_insert.len())
      .field("before_update", &self.before_update.len())
      .field("after_update", &self.after_update.len())
      .field("before_delete", &self.before_delete.len())
      .field("after_delete", &self.after_delete.len())
      .finish()
  }
}

impl<T: Entity, E: Send + 'static> EntityHooks<T, E> {
  /// No hooks.
  pub fn new() -> Self {
    Self::default()
  }

  /// Run `hook` before every insert, in the transaction: it returns the entity to insert, or
  /// fails to reject the insert.
  #[must_use]
  pub fn before_insert<F>(mut self, hook: F) -> Self
  where
    F: Fn(&HookContext<T>, T) -> Effect<T, E> + Send + Sync + 'static,
  {
    self.before_insert.push(Arc::new(hook));
    self
  }

  /// Run `hook` after every insert, in the transaction, with the stored row (generated id and
  /// column defaults included). A failure rolls the insert back.
  #[must_use]
  pub fn after_insert<F>(mut self, hook: F) -> Self
  where
    F: Fn(&HookContext<T>, &T) -> Effect<(), E> + Send + Sync + 'static,
  {
    self.after_insert.push(Arc::new(hook));
    self
  }

  /// Run `hook` before every update, in the transaction: it returns the entity to write, or
  /// fails to reject the update.
  #[must_use]
  pub fn before_update<F>(mut self, hook: F) -> Self
  where
    F: Fn(&HookContext<T>, T) -> Effect<T, E> + Send + Sync + 'static,
  {
    self.before_update.push(Arc::new(hook));
    self
  }

  /// Run `hook` after every update, in the transaction, with the written entity (its version
  /// incremented). A failure rolls the update back.
  #[must_use]
  pub fn after_update<F>(mut self, hook: F) -> Self
  where
    F: Fn(&HookContext<T>, &T) -> Effect<(), E> + Send + Sync + 'static,
  {
    self.after_update.push(Arc::new(hook));
    self
  }

  /// Run `hook` before every delete (soft or hard), in the transaction, with the primary key; a
  /// failure rejects the delete.
  #[must_use]
  pub fn before_delete<F>(mut self, hook: F) -> Self
  where
    F: Fn(&HookContext<T>, &T::Id) -> Effect<(), E> + Send + Sync + 'static,
  {
    self.before_delete.push(Arc::new(hook));
    self
  }

  /// Run `hook` after every delete (soft or hard) that deleted a row, in the transaction, with
  /// the primary key. A failure rolls the delete back.
  #[must_use]
  pub fn after_delete<F>(mut self, hook: F) -> Self
  where
    F: Fn(&HookContext<T>, &T::Id) -> Effect<(), E> + Send + Sync + 'static,
  {
    self.after_delete.push(Arc::new(hook));
    self
  }

  /// Whether no hook is registered.
  pub fn is_empty(&self) -> bool {
    self.before_insert.is_empty()
      && self.after_insert.is_empty()
      && self.before_update.is_empty()
      && self.after_update.is_empty()
      && self.before_delete.is_empty()
      && self.after_delete.is_empty()
  }
}

/// `entity` through every `before` hook, in order.
fn run_before<T: Entity, E: Send + 'static>(
  hooks: &[BeforeHook<T, E>],
  ctx: &HookContext<T>,
  entity: T,
) -> Effect<T, E> {
  hooks.iter().fold(succeed(entity), |acc, hook| {
    let (hook, ctx) = (Arc::clone(hook), ctx.clone());
    acc.flat_map(move |entity| hook(&ctx, entity))
  })
}

/// Every `after` hook on `entity`, in order; then `entity`.
fn run_after<T: Entity, E: Send + 'static>(
  hooks: &[AfterHook<T, E>],
  ctx: &HookContext<T>,
  entity: T,
) -> Effect<T, E> {
  hooks.iter().fold(succeed(entity), |acc, hook| {
    let (hook, ctx) = (Arc::clone(hook), ctx.clone());
    acc.flat_map(move |entity| hook(&ctx, &entity).map(move |()| entity))
  })
}

/// Every id hook on `id`, in order; then `id`.
fn run_id<T: Entity, E: Send + 'static>(
  hooks: &[IdHook<T, E>],
  ctx: &HookContext<T>,
  id: T::Id,
) -> Effect<T::Id, E> {
  hooks.iter().fold(succeed(id), |acc, hook| {
    let (hook, ctx) = (Arc::clone(hook), ctx.clone());
    acc.flat_map(move |id| hook(&ctx, &id).map(move |()| id))
  })
}

/// Run the repository operation `op` on `ex`, its error as `E`.
fn write<A, E>(op: Result<Run<A>, SqlError>, ex: Arc<dyn SqlExecutor>) -> Effect<A, E>
where
  A: Send + 'static,
  E: From<SqlError> + Send + 'static,
{
  match op {
    Ok(run) => run(ex).map_err(E::from),
    Err(e) => fail(E::from(e)),
  }
}

/// A [`Repository`] with [`EntityHooks`] ([`Repository::with_hooks`]): its writes run the hooks
/// and the statement in one transaction (see the [module docs](self)). Reads go through
/// [`repository`](Self::repository). Cloning shares the hooks.
pub struct HookedRepository<T: Entity, E = SqlError> {
  repo: Repository<T>,
  hooks: Arc<EntityHooks<T, E>>,
}

impl<T: Entity, E> Clone for HookedRepository<T, E> {
  fn clone(&self) -> Self {
    Self {
      repo: self.repo.clone(),
      hooks: Arc::clone(&self.hooks),
    }
  }
}

impl<T: Entity, E> fmt::Debug for HookedRepository<T, E> {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("HookedRepository")
      .field("repository", &self.repo)
      .field("hooks", &self.hooks)
      .finish()
  }
}

impl<T: Entity> Repository<T> {
  /// This repository with `hooks`: its [`insert`](HookedRepository::insert),
  /// [`update`](HookedRepository::update), [`delete`](HookedRepository::delete) and
  /// [`hard_delete`](HookedRepository::hard_delete) run them inside the write's transaction
  /// (see [`hooks`](self)). The hooks belong to the returned repository only.
  pub fn with_hooks<E>(&self, hooks: EntityHooks<T, E>) -> HookedRepository<T, E> {
    HookedRepository {
      repo: self.clone(),
      hooks: Arc::new(hooks),
    }
  }
}

impl<T: Entity, E: From<SqlError> + Send + 'static> HookedRepository<T, E> {
  /// The repository the hooks are registered on (its reads and unhooked writes).
  #[inline]
  pub fn repository(&self) -> &Repository<T> {
    &self.repo
  }

  /// The hooks.
  #[inline]
  pub fn hooks(&self) -> &EntityHooks<T, E> {
    &self.hooks
  }

  /// The same writes inside the caller's transaction `tx`, each in a savepoint of it (so a
  /// failing hook undoes its own write, whatever the caller does next).
  pub fn in_transaction(&self, tx: &Arc<dyn SqlTransaction>) -> BoundHookedRepository<T, E> {
    BoundHookedRepository {
      hooked: self.clone(),
      tx: Arc::clone(tx),
    }
  }

  fn insert_on(&self, tx: &Arc<dyn SqlTransaction>, entity: T) -> Effect<T, E> {
    let ctx = HookContext::new(&self.repo, tx);
    let (hooks, write_ctx, after_ctx) = (Arc::clone(&self.hooks), ctx.clone(), ctx.clone());
    run_before(&self.hooks.before_insert, &ctx, entity)
      .flat_map(move |entity| write(write_ctx.repo.repo.insert_op(entity), write_ctx.repo.ex))
      .flat_map(move |stored| run_after(&hooks.after_insert, &after_ctx, stored))
  }

  fn update_on(&self, tx: &Arc<dyn SqlTransaction>, entity: T) -> Effect<T, E> {
    let ctx = HookContext::new(&self.repo, tx);
    let (hooks, write_ctx, after_ctx) = (Arc::clone(&self.hooks), ctx.clone(), ctx.clone());
    run_before(&self.hooks.before_update, &ctx, entity)
      .flat_map(move |entity| write(write_ctx.repo.repo.update_op(entity), write_ctx.repo.ex))
      .flat_map(move |stored| run_after(&hooks.after_update, &after_ctx, stored))
  }

  fn delete_on(&self, tx: &Arc<dyn SqlTransaction>, id: T::Id, hard: bool) -> Effect<bool, E> {
    let ctx = HookContext::new(&self.repo, tx);
    let (hooks, write_ctx) = (Arc::clone(&self.hooks), ctx.clone());
    run_id(&self.hooks.before_delete, &ctx, id).flat_map(move |id| {
      let op = if hard {
        write_ctx.repo.repo.hard_delete_op(&id)
      } else {
        write_ctx.repo.repo.delete_op(&id)
      };
      let ex = Arc::clone(&write_ctx.repo.ex);
      write(op, ex).flat_map(move |deleted| {
        if deleted {
          run_id(&hooks.after_delete, &write_ctx, id).map(|_| true)
        } else {
          succeed(false)
        }
      })
    })
  }

  /// Insert `entity` in a new transaction on the [`SqlClientService`] in `R`: the
  /// `before_insert` hooks, the `INSERT`, then the `after_insert` hooks; a failure anywhere rolls
  /// all of it back. Returns the stored row (see [`Repository::insert`]).
  pub fn insert<R, I>(&self, entity: T) -> Effect<T, E, R>
  where
    R: Selector<SqlClientService, I> + HList,
  {
    let this = self.clone();
    transaction(move |tx| this.insert_on(&tx, entity).narrow())
  }

  /// Update `entity` in a new transaction on the [`SqlClientService`] in `R`: the
  /// `before_update` hooks, the `UPDATE` (optimistic locking as [`Repository::update`]), then the
  /// `after_update` hooks; a failure anywhere rolls all of it back.
  pub fn update<R, I>(&self, entity: T) -> Effect<T, E, R>
  where
    R: Selector<SqlClientService, I> + HList,
  {
    let this = self.clone();
    transaction(move |tx| this.update_on(&tx, entity).narrow())
  }

  /// Delete the row `id` (soft when the entity soft-deletes, as [`Repository::delete`]) in a new
  /// transaction on the [`SqlClientService`] in `R`: the `before_delete` hooks, the statement,
  /// then the `after_delete` hooks when a row was deleted. `false` when none was.
  pub fn delete<R, I>(&self, id: T::Id) -> Effect<bool, E, R>
  where
    R: Selector<SqlClientService, I> + HList,
  {
    let this = self.clone();
    transaction(move |tx| this.delete_on(&tx, id, false).narrow())
  }

  /// [`delete`](Self::delete) with a `DELETE` statement even when the entity soft-deletes (as
  /// [`Repository::hard_delete`]); the same delete hooks run.
  pub fn hard_delete<R, I>(&self, id: T::Id) -> Effect<bool, E, R>
  where
    R: Selector<SqlClientService, I> + HList,
  {
    let this = self.clone();
    transaction(move |tx| this.delete_on(&tx, id, true).narrow())
  }
}

/// A [`HookedRepository`] inside the caller's transaction
/// ([`HookedRepository::in_transaction`]): each write runs its hooks and statement in a
/// savepoint of that transaction, released on success and rolled back to on failure, so a
/// failing hook undoes its write even when the caller carries on; the caller commits.
pub struct BoundHookedRepository<T: Entity, E = SqlError> {
  hooked: HookedRepository<T, E>,
  tx: Arc<dyn SqlTransaction>,
}

impl<T: Entity, E> Clone for BoundHookedRepository<T, E> {
  fn clone(&self) -> Self {
    Self {
      hooked: self.hooked.clone(),
      tx: Arc::clone(&self.tx),
    }
  }
}

impl<T: Entity, E> fmt::Debug for BoundHookedRepository<T, E> {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_tuple("BoundHookedRepository")
      .field(&self.hooked)
      .finish()
  }
}

impl<T: Entity, E: From<SqlError> + Send + 'static> BoundHookedRepository<T, E> {
  /// The hooked repository this is bound from.
  #[inline]
  pub fn hooked(&self) -> &HookedRepository<T, E> {
    &self.hooked
  }

  /// [`HookedRepository::insert`] in a savepoint of the caller's transaction.
  pub fn insert(&self, entity: T) -> Effect<T, E> {
    let this = self.hooked.clone();
    savepoint(&self.tx, "hooks_insert", move |sp| {
      this.insert_on(&sp, entity)
    })
  }

  /// [`HookedRepository::update`] in a savepoint of the caller's transaction.
  pub fn update(&self, entity: T) -> Effect<T, E> {
    let this = self.hooked.clone();
    savepoint(&self.tx, "hooks_update", move |sp| {
      this.update_on(&sp, entity)
    })
  }

  /// [`HookedRepository::delete`] in a savepoint of the caller's transaction.
  pub fn delete(&self, id: T::Id) -> Effect<bool, E> {
    let this = self.hooked.clone();
    savepoint(&self.tx, "hooks_delete", move |sp| {
      this.delete_on(&sp, id, false)
    })
  }

  /// [`HookedRepository::hard_delete`] in a savepoint of the caller's transaction.
  pub fn hard_delete(&self, id: T::Id) -> Effect<bool, E> {
    let this = self.hooked.clone();
    savepoint(&self.tx, "hooks_delete", move |sp| {
      this.delete_on(&sp, id, true)
    })
  }
}

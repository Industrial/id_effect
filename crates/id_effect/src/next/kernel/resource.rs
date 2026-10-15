//! Finalizer and mask combinators: `ensuring`, `uninterruptible`, `uninterruptible_mask`,
//! `Restore` and `acquire_use_release`. Filled by `k-fiber-tree`.
//!
//! All of them are `Instr::Mask` regions, built from `Instr::WithFiber` where the current mask
//! has to be read first. An interrupt requested inside a masked region stays pending and is
//! honoured when the region exits. Finalizers run masked; a finalizer that dies turns the
//! outcome into `Cause::Then(original, Die)` (or `Die` after a success).

use super::anybox::{AnyExit, unbox};
use super::cause::ExitKind;
use super::effect::Effect;
use super::runloop::{Instr, MaskOp};
use crate::failure::Cause;
use crate::runtime::Never;

/// Runs the finalizer `fin`, then re-raises `exit`. A failing finalizer is sequenced after the
/// original cause (`Then`), or replaces a success.
fn then_finalize(fin: Instr, exit: AnyExit) -> Instr {
  Instr::Fold(
    Box::new(fin),
    Box::new(move |fin_exit| match fin_exit {
      Ok(_) => Instr::Pure(exit),
      Err(fc) => Instr::Pure(Err(match exit {
        Ok(_) => fc,
        Err(c) => Cause::then(c, fc),
      })),
    }),
  )
}

impl<A, E, R> Effect<A, E, R>
where
  A: Send + 'static,
  E: Send + 'static,
  R: Send + Sync + 'static,
{
  /// Runs `fin` after this effect however it ends (success, failure, defect or interrupt). The
  /// body keeps the caller's interruptibility; `fin` runs uninterruptibly.
  pub fn ensuring(self, fin: Effect<(), Never, R>) -> Self {
    Self::from_instr(Instr::WithFiber(Box::new(move |_, fiber| {
      let m = fiber.mask();
      Instr::Mask(
        Box::new(Instr::Fold(
          Box::new(Instr::Mask(Box::new(self.into_instr()), MaskOp::Set(m))),
          Box::new(move |exit| then_finalize(fin.into_instr(), exit)),
        )),
        MaskOp::Inc,
      )
    })))
  }

  /// Defers interrupts until this effect finishes: a request that arrives inside is honoured
  /// when the region exits.
  pub fn uninterruptible(self) -> Self {
    Self::from_instr(Instr::Mask(Box::new(self.into_instr()), MaskOp::Inc))
  }

  /// Makes this effect interruptible again, even inside an uninterruptible region.
  pub fn interruptible(self) -> Self {
    Self::from_instr(Instr::Mask(Box::new(self.into_instr()), MaskOp::Set(0)))
  }
}

/// Runs `f`'s effect uninterruptibly, except inside [`Restore::apply`], which restores the
/// interruptibility the caller had.
pub fn uninterruptible_mask<A, E, R>(
  f: impl FnOnce(Restore) -> Effect<A, E, R> + Send + 'static,
) -> Effect<A, E, R>
where
  A: Send + 'static,
  E: Send + 'static,
  R: Send + Sync + 'static,
{
  Effect::from_instr(Instr::WithFiber(Box::new(move |_, fiber| {
    let m = fiber.mask();
    Instr::Mask(Box::new(f(Restore(m)).into_instr()), MaskOp::Inc)
  })))
}

/// Restores the interruptibility outside an [`uninterruptible_mask`] region.
#[derive(Clone, Copy, Debug)]
pub struct Restore(u32);

impl Restore {
  /// Runs `eff` with the mask the enclosing [`uninterruptible_mask`] was entered with.
  pub fn apply<A, E, R>(self, eff: Effect<A, E, R>) -> Effect<A, E, R>
  where
    A: Send + 'static,
    E: Send + 'static,
    R: Send + Sync + 'static,
  {
    Effect::from_instr(Instr::Mask(Box::new(eff.into_instr()), MaskOp::Set(self.0)))
  }
}

/// Acquires a resource uninterruptibly, uses it with the caller's interruptibility, and
/// releases it uninterruptibly however `use_` ends.
///
/// `release` runs exactly once when `acquire` succeeded, and receives the [`ExitKind`] of
/// `use_` (`Success`, `Failure`, `Die` or `Interrupt`). A failed `acquire` releases nothing.
pub fn acquire_use_release<T, B, E, R>(
  acquire: Effect<T, E, R>,
  use_: impl FnOnce(T) -> Effect<B, E, R> + Send + 'static,
  release: impl FnOnce(T, ExitKind) -> Effect<(), Never, R> + Send + 'static,
) -> Effect<B, E, R>
where
  T: Clone + Send + 'static,
  B: Send + 'static,
  E: Send + 'static,
  R: Send + Sync + 'static,
{
  Effect::from_instr(Instr::WithFiber(Box::new(move |_, fiber| {
    let m = fiber.mask();
    Instr::Mask(
      Box::new(Instr::FlatMap(
        Box::new(acquire.into_instr()),
        Box::new(move |t| {
          let t: T = unbox(t);
          let t2 = t.clone();
          Instr::Fold(
            // `use_` is a user closure: called inside a `Sync` step so a panic becomes `Die`
            // and still reaches the release handler.
            Box::new(Instr::Mask(
              Box::new(Instr::Sync(Box::new(move |_| use_(t).into_instr()))),
              MaskOp::Set(m),
            )),
            Box::new(move |exit| {
              let kind = ExitKind::of_result(&exit);
              let fin = Instr::Sync(Box::new(move |_| release(t2, kind).into_instr()));
              then_finalize(fin, exit)
            }),
          )
        }),
      )),
      MaskOp::Inc,
    )
  })))
}

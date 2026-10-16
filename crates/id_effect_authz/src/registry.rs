//! Named policies: [`Policies`] maps names (`"ledger.post"`, `"account.view"`) to policies so
//! handlers and route guards refer to a rule by name, and the rule is defined once.
//!
//! The registry is a capability ([`PoliciesEnv`], [`provide_policies`]); [`authorize`] and
//! [`check`] evaluate a named policy for the [`CurrentPrincipal`] in the environment.
//!
//! ```
//! use id_effect::{Cause, Effect, Exit, HCons, HNil, hlist};
//! use id_effect_authz::{
//!   AuthzError, BasicPrincipal, CurrentPrincipal, Owned, Policies, PolicyExt, authorize, owner,
//!   require_role,
//! };
//!
//! #[derive(Clone)]
//! struct Account {
//!   owner: String,
//! }
//! impl Owned for Account {
//!   fn owner_id(&self) -> &str {
//!     &self.owner
//!   }
//! }
//!
//! let policies = Policies::builder()
//!   .define::<Account>("account.view", owner().or(require_role("auditor")))
//!   .build();
//!
//! type Env = HCons<CurrentPrincipal, HCons<Policies, HNil>>;
//! fn view(account: Account) -> Effect<(), AuthzError, Env> {
//!   authorize("account.view", account)
//! }
//!
//! let account = Account { owner: "alice".into() };
//! let as_bob = hlist![CurrentPrincipal::new(BasicPrincipal::new("bob")), policies.clone()];
//! let Exit::Failure(Cause::Fail(AuthzError::Denied(denial))) =
//!   view(account.clone()).provide_all(as_bob).block_on()
//! else {
//!   panic!("bob is neither the owner nor an auditor")
//! };
//! assert_eq!(denial.policy(), Some("account.view"));
//!
//! let as_alice = hlist![CurrentPrincipal::new(BasicPrincipal::new("alice")), policies];
//! assert_eq!(view(account).provide_all(as_alice).block_on(), Exit::Success(()));
//! ```

use std::any::{Any, type_name};
use std::collections::BTreeMap;
use std::fmt;
use std::sync::Arc;

use id_effect::{Effect, HCons, HList, HNil, Layer, Selector, fail, hlist, need};

use crate::policy::{
  AuthzError, BoxPolicy, Decision, Policy, PolicyEffect, PolicyExt, into_effect,
};
use crate::principal::CurrentPrincipal;

struct Entry {
  resource: &'static str,
  /// A `BoxPolicy<Res>` for the `Res` named by `resource`.
  policy: Arc<dyn Any + Send + Sync>,
}

/// A registry of named policies, each over its own resource type. Cheap to clone (shared).
#[derive(Clone, Default)]
pub struct Policies(Arc<BTreeMap<String, Entry>>);

/// Builds a [`Policies`] registry.
#[derive(Default)]
pub struct PoliciesBuilder(BTreeMap<String, Entry>);

impl PoliciesBuilder {
  /// Defines (or redefines) the policy `name` over resources of type `Res` (`()` for rules that
  /// look only at the principal).
  #[must_use]
  pub fn define<Res: ?Sized + 'static>(
    mut self,
    name: impl Into<String>,
    policy: impl Policy<Res>,
  ) -> Self {
    let boxed: BoxPolicy<Res> = policy.boxed();
    self.0.insert(
      name.into(),
      Entry {
        resource: type_name::<Res>(),
        policy: Arc::new(boxed),
      },
    );
    self
  }

  /// The registry.
  pub fn build(self) -> Policies {
    Policies(Arc::new(self.0))
  }
}

impl fmt::Debug for PoliciesBuilder {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_set().entries(self.0.keys()).finish()
  }
}

impl Policies {
  /// An empty registry builder.
  pub fn builder() -> PoliciesBuilder {
    PoliciesBuilder::default()
  }

  /// Whether a policy is registered as `name`.
  pub fn contains(&self, name: &str) -> bool {
    self.0.contains_key(name)
  }

  /// The registered names, sorted.
  pub fn names(&self) -> impl Iterator<Item = &str> {
    self.0.keys().map(String::as_str)
  }

  /// The resource type the policy `name` was defined for.
  pub fn resource_type(&self, name: &str) -> Option<&'static str> {
    self.0.get(name).map(|e| e.resource)
  }

  /// The policy `name` over `Res`: [`AuthzError::UnknownPolicy`] when absent,
  /// [`AuthzError::ResourceMismatch`] when defined for another resource type.
  pub fn get<Res: ?Sized + 'static>(&self, name: &str) -> Result<BoxPolicy<Res>, AuthzError> {
    let entry = self
      .0
      .get(name)
      .ok_or_else(|| AuthzError::UnknownPolicy(name.to_owned()))?;
    entry
      .policy
      .downcast_ref::<BoxPolicy<Res>>()
      .cloned()
      .ok_or_else(|| AuthzError::ResourceMismatch {
        policy: name.to_owned(),
        expected: entry.resource,
        found: type_name::<Res>(),
      })
  }

  /// Evaluates the policy `name`; a denial is attributed to `name`
  /// ([`Denial::policy`](crate::Denial::policy)).
  pub fn evaluate<Res: ?Sized + 'static>(
    &self,
    name: &str,
    principal: &CurrentPrincipal,
    resource: &Res,
  ) -> PolicyEffect {
    match self.get::<Res>(name) {
      Ok(policy) => {
        let name = name.to_owned();
        policy
          .evaluate(principal, resource)
          .map(move |decision| match decision {
            Decision::Deny(denial) => Decision::Deny(denial.attributed_to(&name)),
            allow => allow,
          })
      }
      Err(e) => fail(e),
    }
  }

  /// Like [`evaluate`](Self::evaluate), failing with [`AuthzError::Denied`] (or
  /// [`AuthzError::Unauthenticated`]) instead of returning a denial.
  pub fn authorize<Res: ?Sized + 'static>(
    &self,
    name: &str,
    principal: &CurrentPrincipal,
    resource: &Res,
  ) -> Effect<(), AuthzError> {
    let principal_owned = principal.clone();
    self
      .evaluate(name, principal, resource)
      .flat_map(move |d| into_effect(d, &principal_owned))
  }
}

impl fmt::Debug for Policies {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_map()
      .entries(self.0.iter().map(|(k, e)| (k, e.resource)))
      .finish()
  }
}

/// The minimal typed environment holding only [`Policies`].
pub type PoliciesEnv = HCons<Policies, HNil>;

/// A [`Layer`] providing `policies` as a capability.
#[inline]
pub fn provide_policies<E: Send + 'static>(policies: Policies) -> Layer<PoliciesEnv, E> {
  Layer::succeed(hlist![policies])
}

/// Evaluates the named policy for the [`CurrentPrincipal`] in `R` acting on `resource`, with the
/// [`Policies`] in `R`.
pub fn check<Res, R, I1, I2>(
  name: impl Into<String>,
  resource: Res,
) -> Effect<Decision, AuthzError, R>
where
  Res: Send + Sync + 'static,
  R: Selector<CurrentPrincipal, I1> + Selector<Policies, I2> + HList,
{
  let name = name.into();
  need::<Policies>().flat_map(move |policies| {
    need::<CurrentPrincipal>().flat_map(move |p| policies.evaluate(&name, &p, &resource).narrow())
  })
}

/// Like [`check`], failing with [`AuthzError::Denied`] (or [`AuthzError::Unauthenticated`])
/// instead of returning a denial.
pub fn authorize<Res, R, I1, I2>(
  name: impl Into<String>,
  resource: Res,
) -> Effect<(), AuthzError, R>
where
  Res: Send + Sync + 'static,
  R: Selector<CurrentPrincipal, I1> + Selector<Policies, I2> + HList,
{
  let name = name.into();
  need::<Policies>().flat_map(move |policies| {
    need::<CurrentPrincipal>().flat_map(move |p| policies.authorize(&name, &p, &resource).narrow())
  })
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::policy::{Denial, allow_all, deny_all, require_role};
  use crate::principal::{Anonymous, BasicPrincipal};
  use id_effect::{Cause, Exit};

  struct Doc;

  fn registry() -> Policies {
    Policies::builder()
      .define::<()>("admin", require_role("admin"))
      .define::<Doc>("doc.read", allow_all())
      .define::<()>("closed", allow_all())
      .define::<()>("closed", deny_all("maintenance"))
      .build()
  }

  fn bob() -> CurrentPrincipal {
    CurrentPrincipal::new(BasicPrincipal::new("bob"))
  }

  #[test]
  fn lookup_and_introspection() {
    let r = registry();
    assert!(r.contains("admin"));
    assert!(!r.contains("nope"));
    assert_eq!(
      r.names().collect::<Vec<_>>(),
      ["admin", "closed", "doc.read"]
    );
    assert_eq!(r.resource_type("admin"), Some("()"));
    assert_eq!(r.resource_type("nope"), None);
    assert!(r.get::<Doc>("doc.read").is_ok());
    assert_eq!(
      r.get::<()>("nope").err(),
      Some(AuthzError::UnknownPolicy("nope".into()))
    );
    let Err(AuthzError::ResourceMismatch { policy, found, .. }) = r.get::<()>("doc.read") else {
      panic!("expected a mismatch")
    };
    assert_eq!(policy, "doc.read");
    assert_eq!(found, "()");
    let dbg = format!("{r:?}");
    assert!(dbg.contains("doc.read"), "{dbg}");
    let builder_dbg = format!("{:?}", Policies::builder().define::<()>("x", allow_all()));
    assert_eq!(builder_dbg, "{\"x\"}");
  }

  #[test]
  fn evaluation_attributes_denials_and_redefinition_wins() {
    let r = registry();
    let denial = Denial::new("maintenance").attributed_to("closed");
    assert_eq!(
      r.evaluate("closed", &bob(), &()).block_on(),
      Exit::Success(Decision::Deny(denial.clone()))
    );
    assert_eq!(
      r.evaluate("doc.read", &bob(), &Doc).block_on(),
      Exit::Success(Decision::Allow)
    );
    assert_eq!(
      r.evaluate("missing", &bob(), &()).block_on(),
      Exit::Failure(Cause::Fail(AuthzError::UnknownPolicy("missing".into())))
    );
    assert_eq!(
      r.authorize("closed", &bob(), &()).block_on(),
      Exit::Failure(Cause::Fail(AuthzError::Denied(denial)))
    );
    assert_eq!(
      r.authorize("admin", &CurrentPrincipal::new(Anonymous), &())
        .block_on(),
      Exit::Failure(Cause::Fail(AuthzError::Unauthenticated))
    );
  }

  #[test]
  fn check_and_authorize_read_both_capabilities() {
    type Env = HCons<CurrentPrincipal, PoliciesEnv>;
    let admin = || CurrentPrincipal::new(BasicPrincipal::new("root").with_role("admin"));
    let checked: Effect<Decision, AuthzError, Env> = check("admin", ());
    let checked = checked
      .provide(admin())
      .provide_layer(provide_policies(registry()));
    assert_eq!(checked.block_on(), Exit::Success(Decision::Allow));
    let ok: Effect<(), AuthzError, Env> = authorize("doc.read", Doc);
    assert_eq!(
      ok.provide_all(hlist![bob(), registry()]).block_on(),
      Exit::Success(())
    );
    let denied: Effect<(), AuthzError, Env> = authorize("admin", ());
    assert!(matches!(
      denied.provide_all(hlist![bob(), registry()]).block_on(),
      Exit::Failure(Cause::Fail(AuthzError::Denied(_)))
    ));
  }
}

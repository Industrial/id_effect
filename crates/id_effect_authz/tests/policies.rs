//! End-to-end: a ledger's roles, named policies, ownership, tenancy and claims, evaluated for
//! principals installed as capabilities (typed `HCons` environments and a `DynEnv`, the shape
//! request handlers use).

use id_effect::{Cause, DynEnv, Effect, Exit, HCons, HNil, hlist};
use id_effect_authz::{
  Acl, AuthzError, BasicPrincipal, CurrentPrincipal, Decision, HasAcl, Owned, Policies, PolicyExt,
  Principal, Rbac, Tenanted, acl_permits, authorize, check, claim_contains, claim_equals,
  from_effect, owner, predicate, same_tenant,
};

#[derive(Clone)]
struct Account {
  owner: String,
  org: String,
  closed: bool,
  acl: Acl,
}

impl Owned for Account {
  fn owner_id(&self) -> &str {
    &self.owner
  }
}

impl Tenanted for Account {
  fn tenant_id(&self) -> &str {
    &self.org
  }
}

impl HasAcl for Account {
  fn acl(&self) -> &Acl {
    &self.acl
  }
}

fn account() -> Account {
  Account {
    owner: "olga".into(),
    org: "acme".into(),
    closed: false,
    acl: Acl::new().allow_user("aldo", "account:read"),
  }
}

fn rbac() -> Rbac {
  Rbac::builder()
    .grant("viewer", ["ledger:read"])
    .grant("accountant", ["ledger:post"])
    .inherit("accountant", "viewer")
    .grant("owner", ["ledger:*"])
    .inherit("owner", "accountant")
    .build()
}

fn policies() -> Policies {
  let rbac = rbac();
  Policies::builder()
    .define::<Account>(
      "ledger.post",
      rbac
        .require("ledger:post")
        .and(same_tenant())
        .and(predicate("account is closed", |_, a: &Account| !a.closed)),
    )
    .define::<Account>(
      "account.read",
      same_tenant()
        .and(rbac.require("ledger:read"))
        .or(acl_permits("account:read")),
    )
    .define::<Account>("account.close", owner().and(claim_equals("mfa", true)))
    .define::<()>(
      "reports.export",
      claim_contains("groups", "finance").or(rbac.require_role("owner")),
    )
    .build()
}

type Env = HCons<CurrentPrincipal, HCons<Policies, HNil>>;

fn decide<Res: Send + Sync + 'static>(who: impl Principal, name: &str, res: Res) -> Decision {
  let eff: Effect<Decision, AuthzError, Env> = check(name.to_owned(), res);
  match eff
    .provide_all(hlist![CurrentPrincipal::new(who), policies()])
    .block_on()
  {
    Exit::Success(d) => d,
    other => panic!("unexpected exit: {other:?}"),
  }
}

fn acme(id: &str, role: &str) -> BasicPrincipal {
  BasicPrincipal::new(id).with_role(role).with_tenant("acme")
}

#[test]
fn roles_and_tenancy_gate_posting() {
  assert!(decide(acme("carl", "accountant"), "ledger.post", account()).is_allowed());
  assert!(decide(acme("olga", "owner"), "ledger.post", account()).is_allowed());
  let Decision::Deny(denial) = decide(acme("vera", "viewer"), "ledger.post", account()) else {
    panic!("a viewer cannot post")
  };
  assert_eq!(denial.policy(), Some("ledger.post"));
  assert_eq!(denial.reason(), "missing permission ledger:post");
  let foreign = BasicPrincipal::new("eve")
    .with_role("accountant")
    .with_tenant("globex");
  assert!(decide(foreign, "ledger.post", account()).is_denied());
  let closed = Account {
    closed: true,
    ..account()
  };
  assert_eq!(
    decide(acme("carl", "accountant"), "ledger.post", closed),
    Decision::Deny(id_effect_authz::Denial::new("account is closed").attributed_to("ledger.post"))
  );
}

#[test]
fn ownership_acl_and_claims() {
  let aldo = BasicPrincipal::new("aldo");
  assert!(decide(aldo.clone().with_tenant("acme"), "account.read", account()).is_allowed());
  // The access list grants within the account's tenant only (M19: tenant match by default).
  assert!(decide(aldo, "account.read", account()).is_denied());
  assert!(decide(acme("vera", "viewer"), "account.read", account()).is_allowed());
  assert!(decide(BasicPrincipal::new("mallory"), "account.read", account()).is_denied());

  assert!(decide(acme("olga", "owner"), "account.close", account()).is_denied());
  let olga_mfa = acme("olga", "owner").with_claim("mfa", true);
  assert!(decide(olga_mfa, "account.close", account()).is_allowed());
  let carl_mfa = acme("carl", "owner").with_claim("mfa", true);
  assert!(decide(carl_mfa, "account.close", account()).is_denied());

  let analyst = BasicPrincipal::new("ann").with_claim("groups", serde_json::json!(["finance"]));
  assert!(decide(analyst, "reports.export", ()).is_allowed());
  assert!(decide(acme("olga", "owner"), "reports.export", ()).is_allowed());
  assert!(decide(acme("carl", "accountant"), "reports.export", ()).is_denied());
}

#[test]
fn authorize_fails_with_401_or_403() {
  let run = |who: CurrentPrincipal| {
    let eff: Effect<(), AuthzError, Env> = authorize("ledger.post", account());
    eff.provide_all(hlist![who, policies()]).block_on()
  };
  let Exit::Failure(Cause::Fail(forbidden)) = run(CurrentPrincipal::new(acme("vera", "viewer")))
  else {
    panic!("expected a denial")
  };
  assert_eq!(forbidden.http_status(), 403);
  let Exit::Failure(Cause::Fail(anon)) = run(CurrentPrincipal::anonymous()) else {
    panic!("expected a denial")
  };
  assert_eq!(anon, AuthzError::Unauthenticated);
  assert_eq!(anon.http_status(), 401);
  assert_eq!(
    run(CurrentPrincipal::new(acme("carl", "accountant"))),
    Exit::Success(())
  );
}

#[test]
fn request_handlers_read_both_capabilities_from_a_dyn_env() {
  let base = DynEnv::new().with(policies());
  let mut request = base.clone();
  request.insert_arc(std::sync::Arc::new(CurrentPrincipal::new(acme(
    "carl",
    "accountant",
  ))));
  let principal = request.try_get::<CurrentPrincipal>().expect("principal");
  let registry = request.try_get::<Policies>().expect("policies");
  assert_eq!(
    registry
      .authorize("ledger.post", &principal, &account())
      .block_on(),
    Exit::Success(())
  );
  assert!(base.try_get::<CurrentPrincipal>().is_err());
}

#[test]
fn effectful_policies_run_their_lookup() {
  let banned = std::sync::Arc::new(vec!["mallory".to_owned()]);
  let not_banned = from_effect(move |p: &CurrentPrincipal, _: &()| {
    let banned = std::sync::Arc::clone(&banned);
    let id = p.id().to_owned();
    id_effect::sync(move || Decision::allow_if(!banned.contains(&id), "banned"))
  });
  let registry = Policies::builder()
    .define::<()>("login", not_banned)
    .build();
  let mallory = CurrentPrincipal::new(BasicPrincipal::new("mallory"));
  let Exit::Success(Decision::Deny(denial)) = registry.evaluate("login", &mallory, &()).block_on()
  else {
    panic!("mallory is banned")
  };
  assert_eq!(denial.to_string(), "login: banned");
  let alice = CurrentPrincipal::new(BasicPrincipal::new("alice"));
  assert_eq!(
    registry.evaluate("login", &alice, &()).block_on(),
    Exit::Success(Decision::Allow)
  );
}

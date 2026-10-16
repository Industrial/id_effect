//! Ex 010 — roles, named policies, ownership and claims for a small ledger.
//!
//! The policies are built once into a [`Policies`] registry (a capability, provided by a
//! `Layer`); each "request" installs its principal as the [`CurrentPrincipal`] capability and
//! asks for a named policy with [`authorize`].
//!
//! Run: `cargo run -p id_effect_authz --example 010_policies`

use id_effect::{Cause, Effect, Exit, HCons, HNil, Layer};
use id_effect_authz::{
  AuthzError, BasicPrincipal, CurrentPrincipal, Owned, Policies, PolicyExt, Principal, Rbac,
  Tenanted, authorize, claim_equals, owner, predicate, provide_policies, same_tenant,
};

#[derive(Clone)]
struct Account {
  owner: String,
  org: String,
  closed: bool,
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

fn policies() -> Policies {
  let rbac = Rbac::builder()
    .grant("viewer", ["ledger:read"])
    .grant("accountant", ["ledger:post"])
    .inherit("accountant", "viewer")
    .grant("owner", ["ledger:*"])
    .inherit("owner", "accountant")
    .build();

  Policies::builder()
    // #1 roles + #4 tenant: accountants post to open accounts of their own organisation.
    .define::<Account>(
      "ledger.post",
      rbac
        .require("ledger:post")
        .and(same_tenant())
        .and(predicate("account is closed", |_, a: &Account| !a.closed)),
    )
    // #4 ownership, #6 attributes: the account's owner closes it, with MFA.
    .define::<Account>("account.close", owner().and(claim_equals("mfa", true)))
    .build()
}

type Env = HCons<CurrentPrincipal, HCons<Policies, HNil>>;

fn request(policy: &'static str, account: Account) -> Effect<(), AuthzError, Env> {
  authorize(policy, account)
}

fn run(who: impl Principal, policy: &'static str, account: &Account) -> String {
  let id = who.id().to_owned();
  let app: Layer<HCons<Policies, HNil>, AuthzError> = provide_policies(policies());
  let effect = request(policy, account.clone())
    .provide(CurrentPrincipal::new(who))
    .provide_layer(app);
  let outcome = match effect.block_on() {
    Exit::Success(()) => "allowed".to_owned(),
    Exit::Failure(Cause::Fail(e)) => format!("{} {e}", e.http_status()),
    Exit::Failure(cause) => format!("failed: {cause:?}"),
  };
  format!("{id:>6} {policy:<14} {outcome}")
}

fn main() {
  let account = Account {
    owner: "olga".into(),
    org: "acme".into(),
    closed: false,
  };
  let olga = || {
    BasicPrincipal::new("olga")
      .with_role("owner")
      .with_tenant("acme")
  };
  let carl = BasicPrincipal::new("carl")
    .with_role("accountant")
    .with_tenant("acme");
  let vera = BasicPrincipal::new("vera")
    .with_role("viewer")
    .with_tenant("acme");
  let ext = BasicPrincipal::new("eve")
    .with_role("accountant")
    .with_tenant("globex");

  println!("{}", run(carl, "ledger.post", &account));
  println!("{}", run(vera, "ledger.post", &account));
  println!("{}", run(ext, "ledger.post", &account));
  println!("{}", run(olga(), "account.close", &account));
  println!(
    "{}",
    run(olga().with_claim("mfa", true), "account.close", &account)
  );
}

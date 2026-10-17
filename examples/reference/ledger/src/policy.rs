//! Who may do what. Three roles, each a set of permissions ([`rbac`]); every route names a
//! policy of the registry ([`policies`]) in its guard, and handlers check the resource policies
//! (`account.post`, `account.manage`, `token.revoke`) on the rows they loaded.
//!
//! | Role | Permissions |
//! |---|---|
//! | `viewer` | `ledger:read` |
//! | `accountant` | the viewer's, `ledger:post`, `accounts:open`, `tokens:issue` |
//! | `owner` | the accountant's, `accounts:manage`, `users:manage`, `audit:verify`, `routes:read` |
//!
//! Managing users also needs [`MFA`]: the direct permission a login that passed its TOTP step
//! carries.

use id_effect_kit::authz::{
  AuthzEvents, Owned, Policies, Policy, PolicyExt, Rbac, Tenanted, allow_all, authenticated, owner,
  predicate, same_tenant,
};

use crate::accounts::Account;
use crate::auth::ApiToken;

/// Reads the ledger.
pub const VIEWER: &str = "viewer";
/// Posts entries and opens accounts.
pub const ACCOUNTANT: &str = "accountant";
/// Runs the organisation: users, locks, closes, audits.
pub const OWNER: &str = "owner";
/// The roles, for input validation.
pub const ROLES: [&str; 3] = [OWNER, ACCOUNTANT, VIEWER];

/// The direct permission of a session that passed its second factor (TOTP).
pub const MFA: &str = "auth:mfa";

/// The role → permission table.
pub fn rbac() -> Rbac {
  Rbac::builder()
    .grant(VIEWER, ["ledger:read"])
    .grant(ACCOUNTANT, ["ledger:post", "accounts:open", "tokens:issue"])
    .inherit(ACCOUNTANT, VIEWER)
    .grant(
      OWNER,
      [
        "accounts:manage",
        "users:manage",
        "audit:verify",
        "routes:read",
      ],
    )
    .inherit(OWNER, ACCOUNTANT)
    .build()
}

impl Owned for Account {
  fn owner_id(&self) -> &str {
    &self.owner_id
  }
}

impl Tenanted for Account {
  fn tenant_id(&self) -> &str {
    &self.org_id
  }
}

impl Owned for ApiToken {
  fn owner_id(&self) -> &str {
    &self.owner_id
  }
}

impl Tenanted for ApiToken {
  fn tenant_id(&self) -> &str {
    &self.org_id
  }
}

/// `can_post(account)`: the caller may post (`ledger:post`) in the account's organisation, and a
/// locked account takes postings only from its owner or an organisation owner.
pub fn can_post(rbac: &Rbac) -> impl Policy<Account> + use<> {
  let unlocked = predicate("the account is locked", |_, account: &Account| {
    !account.locked
  });
  rbac
    .require("ledger:post")
    .and(same_tenant())
    .and(unlocked.or(owner()).or(rbac.require_role(OWNER)))
}

/// The named policies; every decision is recorded to `events`.
pub fn policies(events: AuthzEvents) -> Policies {
  let rbac = rbac();
  Policies::builder()
    .define::<()>("public", allow_all())
    .define::<()>("signed_in", authenticated())
    .define::<()>("ledger.read", rbac.require("ledger:read"))
    .define::<()>("ledger.post", rbac.require("ledger:post"))
    .define::<()>("accounts.open", rbac.require("accounts:open"))
    .define::<()>("tokens.issue", rbac.require("tokens:issue"))
    .define::<()>(
      "users.manage",
      rbac.require("users:manage").and(rbac.require(MFA)),
    )
    .define::<()>("audit.verify", rbac.require("audit:verify"))
    .define::<()>("routes.read", rbac.require("routes:read"))
    .define::<Account>("account.post", can_post(&rbac))
    .define::<Account>(
      "account.manage",
      rbac
        .require("accounts:open")
        .and(same_tenant())
        .and(owner().or(rbac.require_role(OWNER))),
    )
    .define::<ApiToken>("token.revoke", same_tenant().and(owner()))
    .events(events)
    .build()
}

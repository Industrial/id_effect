# `id_effect_authz`

Authorization for id_effect services. Policies are capabilities:

- **`Principal`** — who is asking (id, roles, permissions, claims, tenant). `id_effect_auth`'s
  principals implement it; `BasicPrincipal` covers API keys, service accounts and tests. The
  current one is the `CurrentPrincipal` capability, installed per request or job.
- **`Policy<Res>`** — evaluates the principal against a resource as an effect returning a
  `Decision` (`Allow` or `Deny` with a reason). Combine with `and`, `or`, `negate`, `all_of`,
  `any_of`.
- **Roles and permissions** — `Rbac`: role → permission table with inheritance and `area:*`
  wildcards.
- **Resources** — ownership (`owner`), tenant match (`same_tenant`), access lists (`Acl`,
  `acl_permits`).
- **Attributes** — claim predicates (`claim_equals`, `claim_contains`, `claim`, `claim_matches`).
- **Named policies** — `Policies`, a registry capability; `authorize(name, resource)` fails with
  `AuthzError` (401 for an anonymous principal, 403 otherwise).
- **Decision events** — every decision as a serialisable `DecisionEvent` delivered to the
  `AuthzEvents` sink capability (`MemorySink`, closures, fan-out).
- **Route guards** (feature `axum`) — `Guard`, a Tower layer evaluating a policy for the
  request's principal; `.authorize(guard)` on axum and typed routes; 401/403 as Problem Details.
- **Test helpers** (feature `testing`) — `AuthzTest::assert_allows` / `assert_denies` /
  `assert_denied_because`, with the decisions it recorded.

```rust
use id_effect::{Effect, Exit, HCons, HNil, hlist};
use id_effect_authz::{
  AuthzError, BasicPrincipal, CurrentPrincipal, Owned, Policies, PolicyExt, Rbac, authorize,
  owner,
};

#[derive(Clone)]
struct Account {
  owner: String,
}

impl Owned for Account {
  fn owner_id(&self) -> &str {
    &self.owner
  }
}

let rbac = Rbac::builder()
  .grant("auditor", ["account:read"])
  .build();
let policies = Policies::builder()
  .define::<Account>("account.read", owner().or(rbac.require("account:read")))
  .build();

type Env = HCons<CurrentPrincipal, HCons<Policies, HNil>>;
fn read(account: Account) -> Effect<(), AuthzError, Env> {
  authorize("account.read", account)
}

let account = Account { owner: "olga".into() };
let auditor = CurrentPrincipal::new(BasicPrincipal::new("ada").with_role("auditor"));
let stranger = CurrentPrincipal::new(BasicPrincipal::new("sam"));

let ok = read(account.clone()).provide_all(hlist![auditor, policies.clone()]);
assert_eq!(ok.block_on(), Exit::Success(()));
let denied = read(account).provide_all(hlist![stranger, policies]);
assert!(matches!(denied.block_on(), Exit::Failure(_)));
```

Examples: `cargo run -p id_effect_authz --example 010_policies`;
`cargo run -p id_effect_authz --example 020_route_guards --features axum,testing`. Book: Part VI,
"Authorization".

Tier: `platform` ([ADR 0010](../../docs/adrs/0010-crate-tiers.md) Amendment 5): the Ledger reference app ([`examples/reference/ledger`](../../examples/reference/ledger)) depends on it.

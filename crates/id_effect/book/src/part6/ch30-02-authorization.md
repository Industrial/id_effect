# Authorization

**`id_effect_authz`** decides what a principal may do. Its idea is the one the rest of id_effect
uses for dependencies: **policies are capabilities**. The principal making a request is a
capability, the policy registry is a capability, and checking access is an effect that reads
both.

| Need | Type |
|---|---|
| Who is asking | `Principal` trait, `CurrentPrincipal` capability |
| A rule | `Policy<Res>`: principal + resource → `Effect<Decision, AuthzError>` |
| Roles and permissions | `Rbac` (inheritance, `area:*` wildcards) |
| Resource rules | `owner()`, `same_tenant()`, `Acl` + `acl_permits(..)` |
| Attribute rules | `claim_equals`, `claim_contains`, `claim`, `claim_matches` |
| Rules by name | `Policies` registry, `authorize(name, resource)` |

## The principal

`Principal` is a small trait: an id and roles are required; direct permissions, claims, a tenant
and `is_authenticated` have defaults. `id_effect_auth`'s session and token principals implement
it. `BasicPrincipal` is the plain implementation for API keys, service accounts and tests, and
`Anonymous` is the unauthenticated caller.

The principal of the current request or job is the **`CurrentPrincipal`** capability (a shared
`Arc<dyn Principal>`). Authentication middleware installs it per request (in an axum app,
`id_effect_axum::CapOverrideLayer` is the injection point); a job can get a fixed one from
`provide_principal(..)`, a `Layer`.

```rust
use id_effect::{Effect, Exit, HCons, HNil};
use id_effect_authz::{BasicPrincipal, CurrentPrincipal, current_principal};

fn greeting() -> Effect<String, (), HCons<CurrentPrincipal, HNil>> {
  current_principal().map(|p: CurrentPrincipal| format!("hello {}", p.id()))
}

let ada = BasicPrincipal::new("ada").with_role("auditor").with_claim("mfa", true);
let program = greeting().provide(CurrentPrincipal::new(ada));
assert_eq!(program.block_on(), Exit::Success("hello ada".to_owned()));
```

## Policies and decisions

A `Policy<Res>` looks at a principal and a resource of type `Res` (`()` when only the principal
matters) and returns an effect producing a `Decision`: `Allow`, or `Deny` with a `Denial` that
says why. Because the result is an effect, a policy may look something up (an ACL row, a flag
service). The dependencies of that lookup are given to the policy when it is built, usually in a
`Layer`, so evaluating it needs nothing else.

Policies compose with `and`, `or` and `negate` (the `PolicyExt` trait) and with `all_of` /
`any_of`. `and` runs its right side only when the left side allows; `or` only when it denies.

```rust
use id_effect::Exit;
use id_effect_authz::{
  BasicPrincipal, CurrentPrincipal, Decision, Policy, PolicyExt, predicate, require_role,
};

struct Invoice {
  amount: u64,
}

let approve = require_role("accountant")
  .and(predicate("amount above the approval limit", |_, i: &Invoice| i.amount <= 10_000));

let carl = CurrentPrincipal::new(BasicPrincipal::new("carl").with_role("accountant"));
assert_eq!(
  approve.evaluate(&carl, &Invoice { amount: 900 }).block_on(),
  Exit::Success(Decision::Allow)
);
assert_eq!(
  approve.evaluate(&carl, &Invoice { amount: 90_000 }).block_on(),
  Exit::Success(Decision::deny("amount above the approval limit"))
);
```

`from_effect(|principal, resource| ..)` builds a policy whose decision is an arbitrary effect.
Copy what the effect needs out of the resource first: the effect outlives the borrow.

## Roles and permissions

`Rbac` is a role → permission table. A role can inherit another role's permissions, and a grant
of `ledger:*` covers every `ledger:` permission (`*` covers everything). A principal's direct
permissions (OAuth scopes, API-key grants) count too.

```rust
use id_effect_authz::{BasicPrincipal, Rbac};

let rbac = Rbac::builder()
  .grant("viewer", ["ledger:read"])
  .grant("accountant", ["ledger:post"])
  .inherit("accountant", "viewer")
  .grant("owner", ["ledger:*"])
  .inherit("owner", "accountant")
  .build();

let carl = BasicPrincipal::new("carl").with_role("accountant");
assert!(rbac.permits(&carl, "ledger:read"));
assert!(!rbac.permits(&carl, "ledger:close"));
assert!(rbac.has_role(&BasicPrincipal::new("olga").with_role("owner"), "viewer"));
```

`rbac.require("ledger:post")` and `rbac.require_role("accountant")` are policies over any
resource. `Rbac` is also a capability (`provide_rbac`), and `has_permission("ledger:post")` reads
it together with the current principal. `require_role` / `require_any_role` check roles held
directly, without a table.

## Resources: ownership, tenants, access lists

Resource rules read the resource through small traits:

- `Owned::owner_id` → `owner()` allows the owning principal;
- `Tenanted::tenant_id` → `same_tenant()` allows principals of the same tenant;
- `HasAcl::acl` → `acl_permits("doc:read")` allows principals an `Acl` entry covers (a user, a
  role, every authenticated principal or anyone).

## Attributes and claims

Claim policies read `Principal::claim` (JWT/OIDC claims, profile attributes; JSON values). A
missing claim denies.

```rust
use id_effect::Exit;
use id_effect_authz::{
  BasicPrincipal, CurrentPrincipal, Decision, Policy, PolicyExt, claim, claim_contains,
  claim_matches,
};

struct Report {
  region: &'static str,
}

let finance = claim_contains("groups", "finance")
  .and(claim("level", |v| v.as_u64().is_some_and(|l| l >= 2)))
  .and(claim_matches("region", |v, r: &Report| v == r.region));

let dana = CurrentPrincipal::new(
  BasicPrincipal::new("dana")
    .with_claim("groups", serde_json::json!(["finance", "staff"]))
    .with_claim("level", 3)
    .with_claim("region", "eu"),
);
assert_eq!(
  finance.evaluate(&dana, &Report { region: "eu" }).block_on(),
  Exit::Success(Decision::Allow)
);
assert!(finance.evaluate(&dana, &Report { region: "us" }).block_on() != Exit::Success(Decision::Allow));
```

## Named policies

Rules are easier to audit when they are defined once and referred to by name. `Policies` is a
registry of named policies, each over its own resource type, and a capability
(`provide_policies`). `authorize(name, resource)` evaluates the rule for the current principal
and fails with an `AuthzError`; `check(name, resource)` returns the `Decision` instead. A denial
from the registry carries the policy's name.

```rust
use id_effect::{Cause, Effect, Exit, HCons, HNil, hlist};
use id_effect_authz::{
  AuthzError, BasicPrincipal, CurrentPrincipal, Owned, Policies, PolicyExt, Rbac, Tenanted,
  authorize, owner, same_tenant,
};

#[derive(Clone)]
struct Account {
  owner: String,
  org: String,
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

let rbac = Rbac::builder().grant("accountant", ["ledger:post"]).build();
let policies = Policies::builder()
  .define::<Account>("ledger.post", rbac.require("ledger:post").and(same_tenant()))
  .define::<Account>("account.close", owner())
  .build();

type Env = HCons<CurrentPrincipal, HCons<Policies, HNil>>;
fn post(account: Account) -> Effect<(), AuthzError, Env> {
  authorize("ledger.post", account)
  // ... then write the posting
}

let account = Account { owner: "olga".into(), org: "acme".into() };
let carl = BasicPrincipal::new("carl").with_role("accountant").with_tenant("acme");
let eve = BasicPrincipal::new("eve").with_role("accountant").with_tenant("globex");

let ok = post(account.clone()).provide_all(hlist![CurrentPrincipal::new(carl), policies.clone()]);
assert_eq!(ok.block_on(), Exit::Success(()));

let Exit::Failure(Cause::Fail(AuthzError::Denied(denial))) =
  post(account).provide_all(hlist![CurrentPrincipal::new(eve), policies]).block_on()
else {
  panic!("another tenant's accountant cannot post")
};
assert_eq!(denial.to_string(), "ledger.post: resource belongs to another tenant");
```

## Errors

`AuthzError::http_status` maps a failure to the answer a route gives: `Unauthenticated` (the
principal is `Anonymous`) is 401, `Denied` is 403, and a missing policy, a resource type that does
not match the registered one, or a failed lookup (`Evaluation`) is 500.

See `cargo run -p id_effect_authz --example 010_policies` for a small ledger with every rule
kind.

## See also

- Application host and request capabilities: [Application host](ch30-00-application.md)
- Capabilities and layers: Part II

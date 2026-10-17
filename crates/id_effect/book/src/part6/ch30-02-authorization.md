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
| Route guards (feature `axum`) | `Guard`, `.authorize(guard)` on routes |
| Decision events | `DecisionEvent`, the `AuthzEvents` sink capability |
| Tests (feature `testing`) | `AuthzTest::assert_allows` / `assert_denies` |

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

## Route guards

With the `axum` feature, a **`Guard`** checks a policy before a route's handler runs. It is a
Tower layer; `.authorize(guard)` (the `AuthorizeExt` trait) attaches it to an axum
`MethodRouter` or to a typed `id_effect_axum::route::Endpoint`.

- `Guard::new(name, policy)` evaluates a policy over `()` (roles, permissions, claims);
- `Guard::named(&policies, name)` evaluates a registry policy, so the rule stays defined once;
- `Guard::request(name, policy)` evaluates a policy over the request head (`http::request::Parts`),
  e.g. a tenant taken from the path.

The guard finds the principal in the request's extensions: a `CurrentPrincipal` extension (what
an authentication layer inserts), else one installed by `CapOverrideLayer`, else `Anonymous`.
The authentication layer therefore wraps the router (`Router::layer`) and the guards sit on the
routes. A refusal is answered per `AuthzError::http_status` — 401 for an anonymous caller, 403
for an authenticated one, 500 when the policy cannot be evaluated — with an
`application/problem+json` body naming the policy. The denial reason stays out of the body
unless the guard opts in with `.expose_reason(true)`.

```rust
use axum::extract::Request;
use axum::middleware::{self, Next};
use axum::response::Response;
use axum::routing::{get, post};
use id_effect_authz::{
  AuthorizeExt, BasicPrincipal, CurrentPrincipal, Guard, Policies, Rbac, authenticated,
};

// The authentication layer: whatever proves who the caller is ends here.
async fn authenticate(mut req: Request, next: Next) -> Response {
  if let Some(user) = req.headers().get("x-user").and_then(|v| v.to_str().ok()) {
    let principal = BasicPrincipal::new(user).with_role("viewer");
    req.extensions_mut().insert(CurrentPrincipal::new(principal));
  }
  next.run(req).await
}

let rbac = Rbac::builder().grant("viewer", ["ledger:read"]).build();
let policies = Policies::builder()
  .define::<()>("ledger.read", rbac.require("ledger:read"))
  .define::<()>("ledger.post", rbac.require("ledger:post"))
  .build();

let app: axum::Router = axum::Router::new()
  .route("/me", get(|| async { "signed in" }).authorize(Guard::new("me", authenticated())))
  .route(
    "/ledger",
    get(|| async { "entries" })
      .authorize(Guard::named(&policies, "ledger.read"))
      .merge(post(|| async { "posted" }).authorize(Guard::named(&policies, "ledger.post"))),
  )
  .layer(middleware::from_fn(authenticate));
# let _ = app;
```

A typed endpoint keeps its declaration: `.authorize(..)` on an `Endpoint` returns an
`AuthorizedEndpoint` that lists its guards' policy names and mounts with its `RouteInfo`
recorded in the route registry.

```rust
use id_effect::{Effect, succeed};
use id_effect_authz::{AuthorizeExt, Guard, authenticated};
use id_effect_axum::route::{Input, Route, Router};

#[derive(Clone)]
struct App;

fn report(_: &mut App, _: Input) -> Effect<&'static str, std::convert::Infallible, App> {
  succeed("report")
}

let endpoint = Route::get("/reports")
  .handler(report)
  .authorize(Guard::new("reports.view", authenticated()));
assert_eq!(endpoint.policies(), ["reports.view"]);

let (plain, mut registry) = Router::<App>::new().into_parts();
let app: axum::Router = endpoint.mount(plain, &mut registry).with_state(App);
assert_eq!(registry.len(), 1);
# let _ = app;
```

`AuthzError` also implements `IntoResponse` with the same Problem Details body, so a handler that
calls `authorize(name, resource)` on a loaded resource can return its error as is.

## Data-layer scoping

A guard decides whether a caller may run a route; it does not decide **which rows** the route
reads. `same_tenant()` checks one loaded resource, but a list, a count or a lookup by id has to be
filtered before the rows leave the database, or a forgotten check leaks another tenant's data.
`id_effect_sql`'s repositories (feature `repository`, see
[Query builder, entities and repositories](ch28-03-repositories.md)) do that filtering:

- an entity declared with `#[entity(tenant = "org_id")]` is **tenant-scoped**;
- its `Repository` adds `"org_id" = <tenant>` to **every** statement it builds — `find`,
  `find_all`, `find_by`, `find_page`, `count`, `exists`, `update`, `delete` — and an insert writes
  the tenant column whatever the struct says (an update never changes it);
- the tenant comes from the request: `Repository::for_current_tenant(dialect)` reads the
  **`CurrentTenant`** capability from the environment, `repo.with_tenant(value)` takes it from a
  value you hold (the caller's organisation);
- it **fails closed**: a scoped repository of a tenant-scoped entity without a tenant fails every
  call with `SqlError::Unsupported` and sends nothing. Only `unscoped()`, visible in the code that
  calls it, reads across tenants.

`CurrentTenant` is set where the principal is known. Taking it from `Principal::tenant` keeps the
two in step, and a principal of no tenant gets no repository at all:

```rust
use id_effect::{Cause, Effect, Exit, HCons, HNil, fail, hlist, succeed};
use id_effect_authz::{BasicPrincipal, CurrentPrincipal, Principal};
use id_effect_sql::{
  CurrentTenant, Dialect, Entity, FromRow, Repository, SqlClient, SqlClientService, SqlError,
  SqlValue, TestSqlClient,
};

#[derive(Clone, Debug, PartialEq, FromRow, Entity)]
#[entity(table = "invoices", tenant = "org_id")]
struct Invoice {
  #[entity(id)]
  id: i64,
  amount: i64,
}

type Env = HCons<CurrentTenant, HCons<SqlClientService, HNil>>;

/// The caller's invoices: whatever the query, only rows of the request's tenant come back.
fn invoices() -> Effect<Vec<Invoice>, SqlError, Env> {
  Repository::<Invoice>::for_current_tenant(Dialect::Postgres).flat_map(|repo| repo.find_all())
}

/// The tenant of an authenticated principal; none means no data access (fail closed).
fn tenant_of(principal: &CurrentPrincipal) -> Effect<CurrentTenant, SqlError> {
  match principal.tenant() {
    Some(org) => succeed(CurrentTenant::new(org.to_owned())),
    None => fail(SqlError::Unsupported("the principal has no tenant".into())),
  }
}

let test = TestSqlClient::new();
let _ = test.connect().block_on();
let carl = CurrentPrincipal::new(BasicPrincipal::new("carl").with_tenant("acme"));
let Exit::Success(tenant) = tenant_of(&carl).block_on() else { panic!("carl has a tenant") };
let env = hlist![tenant, SqlClientService::new(test.clone())];
assert!(matches!(invoices().provide_all(env).block_on(), Exit::Success(_)));
assert_eq!(
  test.recorded_queries()[0],
  (
    r#"SELECT "id", "amount" FROM "invoices" WHERE "org_id" = $1 ORDER BY "id" ASC"#.to_owned(),
    vec![SqlValue::Text("acme".into())]
  )
);

// No tenant: nothing is queried.
let nobody = CurrentPrincipal::new(BasicPrincipal::new("svc"));
assert!(matches!(tenant_of(&nobody).block_on(), Exit::Failure(Cause::Fail(SqlError::Unsupported(_)))));
let no_tenant = Repository::<Invoice>::new(Dialect::Postgres)
  .find_all::<HCons<SqlClientService, HNil>, _>()
  .provide_all(hlist![SqlClientService::new(test.clone())]);
assert!(matches!(
  no_tenant.block_on(),
  Exit::Failure(Cause::Fail(SqlError::Unsupported(m))) if m.contains("tenant column `org_id`")
));
assert_eq!(test.recorded_queries().len(), 1, "the call without a tenant sent nothing");
```

The reference app `examples/reference/ledger` scopes every account, entry and API-token
repository to the caller's organisation (`accounts_of(caller.org())` in `src/accounts.rs`, built
from the principal's tenant by `Caller::of`, which answers 403 for a principal without one). Its
test `accounts::organisations_never_see_each_other` (`tests/accounts.rs`) signs in two
organisations and checks that one cannot list, read, read the balance of, or post to the other's
accounts. Statements written by hand (`query`, raw SQL, the query builder) are not filtered: put
the tenant in their `WHERE` yourself, as the Ledger's balance query does.

## Recording decisions

Every decision can be recorded as a **`DecisionEvent`**: the principal's id, whether it was
authenticated, its tenant, the policy name, a description of the resource, the outcome (`allow`,
`deny` or `error`) and the reason. The event serialises with serde in a stable shape, so an
audit log can persist it.

Events go to the **`AuthzEvents`** capability, a handle to one or more sinks (`AuthzSink`):
`MemorySink` for tests, `AuthzEvents::from_fn` for a logger or a metrics counter,
`AuthzEvents::and` to fan out. Decisions are recorded where they are made:

- a `Policies` registry built with `.events(sink)` records every named evaluation, which covers
  `authorize` and `check`;
- `events.observe(name, policy)` wraps any policy;
- a route guard records with `.events(sink)`, the resource being `METHOD /path`.

```rust
use id_effect::Exit;
use id_effect_authz::{
  AuthzEvents, BasicPrincipal, CurrentPrincipal, MemorySink, Outcome, Policies, require_role,
};

let sink = MemorySink::new();
let policies = Policies::builder()
  .define::<()>("reports.export", require_role("auditor"))
  .events(AuthzEvents::new(sink.clone()))
  .build();

let sam = CurrentPrincipal::new(BasicPrincipal::new("sam"));
assert!(matches!(
  policies.evaluate("reports.export", &sam, &()).block_on(),
  Exit::Success(_)
));

let event = &sink.events()[0];
assert_eq!((event.principal.as_str(), event.outcome), ("sam", Outcome::Deny));
assert_eq!(event.reason.as_deref(), Some("requires role auditor"));
let json = serde_json::to_value(event).unwrap();
assert_eq!(json["outcome"], "deny");
```

Inside an effect, `emit_decision(event)` reads the sink from the environment
(`provide_authz_events(events)` is its `Layer`).

## Testing policies

The `testing` feature adds **`AuthzTest`**. It evaluates a policy for a principal and asserts
the outcome — `assert_allows`, `assert_denies` (returns the `Denial`), `assert_denied_because`
(checks the reason) and their `_named` forms for a registry — and records every decision it
sees. Its `policies()` record into the harness, and `sink()` plugs into a route guard, so an
HTTP test can assert the decisions a request triggered.

```rust
use id_effect_authz::testing::AuthzTest;
use id_effect_authz::{BasicPrincipal, CurrentPrincipal, Outcome, Policies, Rbac, owner};

struct Account {
  owner: String,
}
impl id_effect_authz::Owned for Account {
  fn owner_id(&self) -> &str {
    &self.owner
  }
}

let rbac = Rbac::builder().grant("accountant", ["ledger:post"]).build();
let authz = AuthzTest::with_policies(
  Policies::builder()
    .define::<()>("ledger.post", rbac.require("ledger:post"))
    .build(),
);

let carl = CurrentPrincipal::new(BasicPrincipal::new("carl").with_role("accountant"));
let vera = CurrentPrincipal::new(BasicPrincipal::new("vera").with_role("viewer"));
authz.assert_allows_named(&carl, "ledger.post", &());
authz.assert_denied_because_named(&vera, "ledger.post", &(), "missing permission ledger:post");

let account = Account { owner: "olga".into() };
authz.assert_denies(&carl, &owner(), &account);

let outcomes: Vec<Outcome> = authz.events().iter().map(|e| e.outcome).collect();
assert_eq!(outcomes, [Outcome::Allow, Outcome::Deny, Outcome::Deny]);
```

See `cargo run -p id_effect_authz --example 020_route_guards --features axum,testing` for guards,
Problem Details answers and the decision events of a few requests.

## See also

- Application host and request capabilities: [Application host](ch30-00-application.md)
- Capabilities and layers: Part II

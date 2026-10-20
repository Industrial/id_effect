//! M19 (ADR 0013 R1): tenant isolation is automatic. A policy defined for a `Tenanted` resource,
//! or run on one with `check_with` / `authorize_with`, checks the tenant before it decides;
//! crossing tenants is the named, recorded opt-out `dangerous_cross_tenant`; resources of no
//! tenant decide as before.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use id_effect::observability::posture::dangerous_opt_outs;
use id_effect::{Cause, Exit, succeed};
use id_effect_authz::{
  Acl, AuthzError, BasicPrincipal, CurrentPrincipal, Decision, Policies, Policy, PolicyExt, Rbac,
  TenantScope, Tenanted, allow_all, authorize_with, check_with, from_effect, principal_env,
  tenant_scoped,
};

const OTHER_TENANT: &str = "resource belongs to another tenant";

#[derive(Clone)]
struct Invoice {
  org: String,
}

impl Tenanted for Invoice {
  fn tenant_id(&self) -> &str {
    &self.org
  }
}

/// A resource of no tenant, declared as such.
#[derive(Clone)]
struct Report;

impl TenantScope for Report {}

fn acme_invoice() -> Invoice {
  Invoice { org: "acme".into() }
}

fn rbac() -> Rbac {
  Rbac::builder()
    .grant("accountant", ["invoice:read"])
    .grant("support", ["invoice:read"])
    .build()
}

fn accountant(tenant: Option<&str>) -> BasicPrincipal {
  let p = BasicPrincipal::new("carl").with_role("accountant");
  match tenant {
    Some(t) => p.with_tenant(t),
    None => p,
  }
}

fn decide<Res: 'static>(
  policies: &Policies,
  name: &str,
  who: BasicPrincipal,
  res: &Res,
) -> Decision {
  match policies
    .evaluate(name, &CurrentPrincipal::new(who), res)
    .block_on()
  {
    Exit::Success(d) => d,
    other => panic!("unexpected exit: {other:?}"),
  }
}

#[test]
fn m19_tenanted_resource_requires_tenant_match_by_default() {
  // The policy only asks for a permission; it never mentions the tenant.
  let ran = Arc::new(AtomicUsize::new(0));
  let counted = {
    let ran = Arc::clone(&ran);
    from_effect(move |_: &CurrentPrincipal, _: &Invoice| {
      ran.fetch_add(1, Ordering::SeqCst);
      succeed(Decision::Allow)
    })
  };
  let policies = Policies::builder()
    .define::<Invoice>("invoice.read", rbac().require("invoice:read"))
    .define::<Invoice>("invoice.counted", counted)
    .build();

  // Same tenant: the policy decides.
  assert!(
    decide(
      &policies,
      "invoice.read",
      accountant(Some("acme")),
      &acme_invoice()
    )
    .is_allowed()
  );

  // Another tenant, or none: denied before the policy runs.
  for who in [accountant(Some("globex")), accountant(None)] {
    let Decision::Deny(denial) = decide(&policies, "invoice.read", who, &acme_invoice()) else {
      panic!("a principal outside the invoice's tenant must be denied")
    };
    assert_eq!(denial.reason(), OTHER_TENANT);
    assert_eq!(denial.to_string(), format!("invoice.read: {OTHER_TENANT}"));
  }
  let before = ran.load(Ordering::SeqCst);
  assert!(
    decide(
      &policies,
      "invoice.counted",
      accountant(Some("globex")),
      &acme_invoice()
    )
    .is_denied()
  );
  assert_eq!(
    ran.load(Ordering::SeqCst),
    before,
    "the policy ran across tenants"
  );
  assert!(
    decide(
      &policies,
      "invoice.counted",
      accountant(Some("acme")),
      &acme_invoice()
    )
    .is_allowed()
  );
  assert_eq!(ran.load(Ordering::SeqCst), before + 1);

  // The unnamed entry points scope the same way.
  let checked = check_with(rbac().require("invoice:read"), acme_invoice())
    .provide_all(principal_env(accountant(Some("globex"))));
  assert_eq!(
    checked.block_on(),
    Exit::Success(Decision::deny(OTHER_TENANT))
  );
  let authorized = authorize_with(allow_all(), acme_invoice())
    .provide_all(principal_env(accountant(Some("globex"))));
  assert!(matches!(
    authorized.block_on(),
    Exit::Failure(Cause::Fail(AuthzError::Denied(_)))
  ));
  let same = authorize_with(allow_all(), acme_invoice())
    .provide_all(principal_env(accountant(Some("acme"))));
  assert_eq!(same.block_on(), Exit::Success(()));

  // Evaluated directly, a policy is the bare rule; `tenant_scoped` adds the check.
  let globex = CurrentPrincipal::new(accountant(Some("globex")));
  assert_eq!(
    allow_all().evaluate(&globex, &acme_invoice()).block_on(),
    Exit::Success(Decision::Allow)
  );
  assert_eq!(
    tenant_scoped(allow_all())
      .evaluate(&globex, &acme_invoice())
      .block_on(),
    Exit::Success(Decision::deny(OTHER_TENANT))
  );
}

#[test]
fn m19_cross_tenant_requires_named_opt_out() {
  let support = || {
    BasicPrincipal::new("sue")
      .with_role("support")
      .with_tenant("platform")
  };
  let cross = rbac()
    .require("invoice:read")
    .dangerous_cross_tenant("the support desk reads every tenant's invoices");
  assert_eq!(
    cross.reason(),
    "the support desk reads every tenant's invoices"
  );
  assert!(Policy::<Invoice>::crosses_tenants(&cross));
  assert!(!Policy::<Invoice>::crosses_tenants(
    &rbac().require("invoice:read")
  ));

  let policies = Policies::builder()
    .define::<Invoice>("invoice.support", cross.clone())
    .define::<Invoice>("invoice.read", rbac().require("invoice:read"))
    .build();

  // Without the opt-out, support is held to its own tenant.
  assert!(decide(&policies, "invoice.read", support(), &acme_invoice()).is_denied());
  // With it, the policy decides across tenants (and still decides: no role, no access).
  assert!(decide(&policies, "invoice.support", support(), &acme_invoice()).is_allowed());
  let stranger = BasicPrincipal::new("eve").with_tenant("globex");
  let Decision::Deny(denial) = decide(&policies, "invoice.support", stranger, &acme_invoice())
  else {
    panic!("the opt-out must not grant what the policy refuses")
  };
  assert_ne!(denial.reason(), OTHER_TENANT);

  let direct = authorize_with(cross, acme_invoice()).provide_all(principal_env(support()));
  assert_eq!(direct.block_on(), Exit::Success(()));

  // The opt-out is recorded for the prod posture check.
  let recorded = dangerous_opt_outs();
  assert!(
    recorded
      .iter()
      .any(|(id, _)| *id == "id_effect_authz::PolicyExt::dangerous_cross_tenant"),
    "{recorded:?}"
  );
}

#[test]
fn m19_untenanted_resource_unaffected() {
  assert_eq!(Report.tenant_scope(), None);
  assert_eq!(().tenant_scope(), None);
  assert_eq!(Acl::new().tenant_scope(), None);
  assert_eq!(acme_invoice().tenant_scope(), Some("acme"));

  let policies = Policies::builder()
    .define::<Report>("report.read", rbac().require("invoice:read"))
    .define::<()>("open", allow_all())
    .define::<Acl>("acl", allow_all())
    .build();
  for who in [
    accountant(Some("globex")),
    accountant(Some("acme")),
    accountant(None),
  ] {
    assert!(decide(&policies, "report.read", who.clone(), &Report).is_allowed());
    assert!(decide(&policies, "open", who.clone(), &()).is_allowed());
    assert!(decide(&policies, "acl", who, &Acl::new()).is_allowed());
  }
  let Decision::Deny(denial) = decide(&policies, "report.read", BasicPrincipal::new("x"), &Report)
  else {
    panic!("the policy still decides")
  };
  assert_eq!(denial.reason(), "missing permission invoice:read");

  let checked = check_with(allow_all(), Report).provide_all(principal_env(accountant(None)));
  assert_eq!(checked.block_on(), Exit::Success(Decision::Allow));
}

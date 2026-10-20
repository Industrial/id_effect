// ADR 0013 R1 (M19): every resource type a policy is defined for states its tenant scope
// (`Tenanted`, or `impl TenantScope for T {}` for none); an undeclared type does not register.
use id_effect_kit::authz::{Policies, require_role};

struct Report;

fn main() {
  let _ = Policies::builder().define::<Report>("report.view", require_role("auditor"));
}

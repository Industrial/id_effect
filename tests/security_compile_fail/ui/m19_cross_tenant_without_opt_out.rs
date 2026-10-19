// ADR 0013 R1 (M19): a tenanted resource is checked against the principal's tenant; declaring
// it "of no tenant" to skip that check conflicts with its `Tenanted` scope. Crossing tenants
// takes the recorded opt-out `PolicyExt::dangerous_cross_tenant`.
use id_effect_kit::authz::{TenantScope, Tenanted};

struct Account {
  tenant: String,
}

impl Tenanted for Account {
  fn tenant_id(&self) -> &str {
    &self.tenant
  }
}

impl TenantScope for Account {}

fn main() {}

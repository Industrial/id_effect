//! What the Pipeline states to the kit's startup security check (ADR 0013 R10,
//! `id_effect_kit::posture`): every surface it serves, so the prod profile can refuse to start
//! when one is weakened.
//!
//! | Probe | What the Pipeline states |
//! |---|---|
//! | admin guarded | every route under `/admin` (one site per tenant) from the access report: all guarded |
//! | mcp guarded | every route under `/mcp` (one endpoint per tenant): all guarded, read-only tools |
//! | prompts not logged | the `AiConfig` the models are built from |
//! | tenant not from header | the `TenantResolver` of the tenancy layer: the principal's claim |
//! | CSRF on | the back office's `SecurityLayer` |
//! | every procedure declared | the API's and the back office's routes, and the `Intake` RPC contract |
//! | console, `/docs`, mesh | absent |
//!
//! The probes about defaults the app does not touch (redaction, SSRF, load shedding,
//! memory-only security state, 0-RTT, …) are judged from the posture registry: every
//! `dangerous_*` opt-out taken in the process is listed, and the Pipeline takes none. Its
//! accepted risks are empty: any opt-out fails the check.

use std::sync::Arc;

use id_effect_kit::ai::AiConfig;
use id_effect_kit::posture::{AcceptedRisks, Posture, Surface};
use id_effect_kit::tenancy::TenantResolver;

use crate::App;
use crate::mcp::McpSettings;
use crate::rpc::IntakeService;

/// The path every MCP endpoint is under (`/mcp/<tenant>`).
pub const MCP_PREFIX: &str = "/mcp";

/// The Pipeline's posture in `profile` (`prod`, `staging`, `dev`, `test`) for `app` served with
/// `mcp` and models built from `ai`; `.check()` it once the app is assembled, and
/// `.check().enforce()` refuses a weakened prod profile.
///
/// # Errors
///
/// When the routes do not build (invalid MCP settings).
pub fn posture(
  app: &Arc<App>,
  mcp: &McpSettings,
  ai: &AiConfig,
  profile: &str,
) -> Result<Posture, String> {
  let api = crate::routes(app, mcp)?.access_report();
  let office =
    super::routes(app, &super::catalog(), &super::LocalePreferences::new())?.access_report();
  Ok(
    Posture::new(profile)
      .accepted_risks(AcceptedRisks::new())
      .console(Surface::Absent)
      .docs(Surface::Absent)
      .no_mesh()
      .security_layer(&super::security_layer())
      .access_report(&api)
      .access_report(&office)
      .rpc_service(&IntakeService::contract())
      .admin_routes(crate::admin::PREFIX, &office)
      .mcp_routes(MCP_PREFIX, &api)
      .ai_config(ai)
      .tenant_resolver(&TenantResolver::new()),
  )
}

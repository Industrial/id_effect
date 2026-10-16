//! [`AppEnv`]: the environment every use case runs in.

use id_effect::caps;

use super::RequestPolicy;
use crate::domain::todo::TodoRepositoryService;

/// The store capability and the request policy. It is the router's state: `id_effect_axum`
/// provides it to each request's effect.
pub type AppEnv = caps!(TodoRepositoryService, RequestPolicy);

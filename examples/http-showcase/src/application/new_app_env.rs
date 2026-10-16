//! [`new_app_env`]: builds the [`AppEnv`](super::AppEnv).

use id_effect::hlist;

use super::{AppEnv, RequestPolicy};
use crate::domain::todo::TodoRepositoryService;

/// The [`AppEnv`] over `repository` and `policy`.
pub fn new_app_env(repository: TodoRepositoryService, policy: RequestPolicy) -> AppEnv {
  hlist![repository, policy]
}

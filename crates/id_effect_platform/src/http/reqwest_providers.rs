#![allow(clippy::new_ret_no_self, unused_imports)]
//! Capability DI providers for [`reqwest::Client`] and connection pools.

use std::sync::Arc;
use std::time::Duration;

use ::id_effect::{
  Cap, CapabilityId, CapabilityKey, Env, Never, Pool, ProviderBox, ProviderError, ProviderNode,
  run_blocking, succeed,
};

use super::{Client, PooledClient, ReqwestClient, ReqwestPool};

/// Default [`id_effect::ProviderSpec`] for [`reqwest::Client::new`].
#[derive(::id_effect::ProviderSpecDerive)]
#[provides(ReqwestClient)]
pub struct ReqwestClientLive;

impl ReqwestClientLive {
  fn new() -> ReqwestClient {
    ReqwestClient(Client::new())
  }
}

/// Register `client` as the [`ReqwestClient`] capability.
#[inline]
pub fn provide_reqwest_client(client: Client) -> ProviderBox {
  struct Node(Client);

  impl ProviderNode for Node {
    fn id(&self) -> &str {
      "reqwest/client"
    }

    fn requires(&self) -> &[CapabilityId] {
      &[]
    }

    fn provides(&self) -> CapabilityId {
      Cap::<ReqwestClient>::id()
    }

    fn cap_name(&self) -> &str {
      "ReqwestClient"
    }

    fn build(&self, deps: &Env) -> Result<Env, ProviderError> {
      let mut out = deps.clone();
      out.insert::<Cap<ReqwestClient>>(ReqwestClient(self.0.clone()));
      Ok(out)
    }
  }

  ProviderBox(Arc::new(Node(client)))
}

/// Register a [`Pool`] of [`PooledClient`] with the given capacity and TTL.
#[inline]
pub fn provide_reqwest_pool(capacity: usize, ttl: Duration) -> ProviderBox {
  struct Node {
    capacity: usize,
    ttl: Duration,
  }

  impl ProviderNode for Node {
    fn id(&self) -> &str {
      "reqwest/pool"
    }

    fn requires(&self) -> &[CapabilityId] {
      &[]
    }

    fn provides(&self) -> CapabilityId {
      Cap::<ReqwestPool>::id()
    }

    fn cap_name(&self) -> &str {
      "ReqwestPool"
    }

    fn build(&self, deps: &Env) -> Result<Env, ProviderError> {
      let pool = run_blocking(
        Pool::make_with_ttl(self.capacity, self.ttl, || {
          succeed::<PooledClient, Never, ()>(PooledClient(Arc::new(Client::new())))
        }),
        (),
      )
      .map_err(|e| ProviderError {
        provider: "reqwest/pool",
        message: format!("Pool::make_with_ttl: {e:?}"),
      })?;
      let mut out = deps.clone();
      out.insert::<Cap<ReqwestPool>>(pool);
      Ok(out)
    }
  }

  ProviderBox(Arc::new(Node { capacity, ttl }))
}

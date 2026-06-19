#![allow(clippy::new_ret_no_self, unused_imports)]
//! Capability DI providers for [`reqwest::Client`] and connection pools.

use std::sync::Arc;
use std::time::Duration;

use ::id_effect::{
  CapabilityId, CapabilityKey, Env, Never, Pool, ProviderBox, ProviderError, ProviderNode,
  run_blocking, succeed,
};

use super::{Client, PooledClient, ReqwestClientKey, ReqwestPoolKey};

/// Default [`id_effect::ProviderSpec`] for [`reqwest::Client::new`].
#[derive(::id_effect::ProviderSpecDerive)]
#[provides(ReqwestClientKey)]
pub struct ReqwestClientLive;

impl ReqwestClientLive {
  fn new() -> Client {
    Client::new()
  }
}

/// Register `client` as the [`ReqwestClientKey`] capability.
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
      ReqwestClientKey::id()
    }

    fn cap_name(&self) -> &str {
      "ReqwestClientKey"
    }

    fn build(&self, deps: &Env) -> Result<Env, ProviderError> {
      let mut out = deps.clone();
      out.insert::<ReqwestClientKey>(self.0.clone());
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
      ReqwestPoolKey::id()
    }

    fn cap_name(&self) -> &str {
      "ReqwestPoolKey"
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
      out.insert::<ReqwestPoolKey>(pool);
      Ok(out)
    }
  }

  ProviderBox(Arc::new(Node { capacity, ttl }))
}

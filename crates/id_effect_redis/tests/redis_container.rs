//! The client, the typed stores, the cache backend and pub/sub against a real Redis 8 server in
//! a container (see `common` for SKIP / `ID_EFFECT_REQUIRE_DOCKER`).

mod common;

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use common::{ok, redis_client, run};
use id_effect::{Cache, CacheBackend, CacheStats, Effect, Never, need, sync};
use id_effect_redis::{
  RedisCacheBackend, RedisClient, RedisConfig, RedisEnv, RedisError, RedisMessage,
};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct Profile {
  name: String,
  visits: u32,
}

fn profile(name: &str, visits: u32) -> Profile {
  Profile {
    name: name.to_owned(),
    visits,
  }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn key_value_store() {
  let Some((_redis, client)) = redis_client("key_value_store").await else {
    return;
  };
  let kv = client.kv::<Profile>("profiles");
  assert_eq!(kv.key("ada"), "profiles:ada");
  assert_eq!(ok(kv.get("ada")).await, None);
  ok(kv.set("ada", &profile("Ada", 1))).await;
  assert_eq!(ok(kv.get("ada")).await, Some(profile("Ada", 1)));
  assert_eq!(ok(kv.ttl("ada")).await, None, "set clears any expiry");

  ok(kv.set_ex("bob", &profile("Bob", 2), Duration::from_secs(60))).await;
  let ttl = ok(kv.ttl("bob")).await.expect("a ttl");
  assert!(
    ttl > Duration::from_secs(50) && ttl <= Duration::from_secs(60),
    "{ttl:?}"
  );
  assert_eq!(
    ok(kv.get_many(&["ada", "nobody", "bob"])).await,
    vec![Some(profile("Ada", 1)), None, Some(profile("Bob", 2))]
  );
  assert_eq!(ok(kv.get_many(&[])).await, vec![]);

  // Set-if-absent.
  assert!(!ok(kv.set_nx("ada", &profile("Other", 0), None)).await);
  assert!(ok(kv.set_nx("cyd", &profile("Cyd", 3), Some(Duration::from_millis(150)))).await);
  assert!(ok(kv.exists("cyd")).await);
  tokio::time::sleep(Duration::from_millis(300)).await;
  assert!(!ok(kv.exists("cyd")).await, "expired");

  assert!(ok(kv.expire("ada", Duration::from_secs(30))).await);
  assert!(!ok(kv.expire("nobody", Duration::from_secs(30))).await);
  assert_eq!(ok(kv.keys()).await, ["ada", "bob"]);
  assert!(ok(kv.delete("bob")).await);
  assert!(!ok(kv.delete("bob")).await);

  // Counters, and a value that is not a `V`.
  let counters = client.kv::<i64>("counters");
  assert_eq!(ok(counters.incr_by("hits", 2)).await, 2);
  assert_eq!(ok(counters.incr_by("hits", 3)).await, 5);
  assert_eq!(ok(counters.get("hits")).await, Some(5));
  ok(client.query::<()>(redis::cmd("SET").arg("profiles:junk").arg("{oops").clone())).await;
  assert!(matches!(
    run(kv.get("junk")).await,
    Err(RedisError::Decode(_))
  ));
  assert!(matches!(
    run(kv.get_many(&["junk"])).await,
    Err(RedisError::Decode(_))
  ));

  // Clearing a namespace leaves the others alone; an empty namespace is the whole database.
  assert_eq!(ok(kv.clear()).await, 2);
  assert_eq!(ok(kv.keys()).await, Vec::<String>::new());
  let root = client.kv::<i64>("");
  assert_eq!(root.key("counters:hits"), "counters:hits");
  assert_eq!(ok(root.keys()).await, ["counters:hits"]);
  assert!(format!("{kv:?}").contains("profiles"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn hash_store() {
  let Some((_redis, client)) = redis_client("hash_store").await else {
    return;
  };
  let carts = client.hash::<u32>("carts");
  assert_eq!(carts.key("u1"), "carts:u1");
  assert!(ok(carts.set("u1", "apple", &2)).await);
  assert!(!ok(carts.set("u1", "apple", &3)).await, "an existing field");
  ok(carts.set_all(
    "u1",
    &BTreeMap::from([("pear".to_owned(), 1), ("fig".to_owned(), 4)]),
  ))
  .await;
  ok(carts.set_all("u1", &BTreeMap::new())).await;
  assert_eq!(ok(carts.get("u1", "apple")).await, Some(3));
  assert_eq!(ok(carts.get("u1", "kiwi")).await, None);
  assert_eq!(
    ok(carts.get_all("u1")).await,
    BTreeMap::from([
      ("apple".to_owned(), 3),
      ("fig".to_owned(), 4),
      ("pear".to_owned(), 1)
    ])
  );
  assert_eq!(ok(carts.get_all("nobody")).await, BTreeMap::new());
  assert_eq!(ok(carts.incr_by("u1", "fig", 6)).await, 10);
  assert_eq!(ok(carts.len("u1")).await, 3);
  assert!(ok(carts.delete_field("u1", "pear")).await);
  assert!(!ok(carts.delete_field("u1", "pear")).await);
  assert!(ok(carts.expire("u1", Duration::from_secs(60))).await);
  ok(carts.set("u2", "apple", &1)).await;
  assert_eq!(ok(carts.keys()).await, ["u1", "u2"]);
  assert!(ok(carts.delete("u2")).await);
  ok(
    client.query::<()>(
      redis::cmd("HSET")
        .arg("carts:u1")
        .arg("bad")
        .arg("x")
        .clone(),
    ),
  )
  .await;
  assert!(matches!(
    run(carts.get_all("u1")).await,
    Err(RedisError::Decode(_))
  ));
  assert!(matches!(
    run(carts.get("u1", "bad")).await,
    Err(RedisError::Decode(_))
  ));
  assert_eq!(ok(carts.clear()).await, 1);
  assert!(format!("{carts:?}").contains("carts"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn json_document_store() {
  let Some((_redis, client)) = redis_client("json_document_store").await else {
    return;
  };
  #[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
  struct Order {
    status: String,
    total_cents: i64,
    lines: Vec<String>,
  }
  let orders = client.json::<Order>("orders");
  assert_eq!(orders.key("o1"), "orders:o1");
  let o1 = Order {
    status: "new".into(),
    total_cents: 1250,
    lines: vec!["tea".into()],
  };
  ok(orders.set("o1", &o1)).await;
  assert_eq!(ok(orders.get("o1")).await, Some(o1.clone()));
  assert_eq!(ok(orders.get("nope")).await, None);
  assert_eq!(
    ok(orders.get_path::<String>("o1", "$.status")).await,
    Some("new".to_owned())
  );
  assert_eq!(ok(orders.get_path::<String>("o1", "$.missing")).await, None);
  assert_eq!(
    ok(orders.get_path::<String>("nope", "$.status")).await,
    None
  );

  assert!(ok(orders.set_path("o1", "$.status", &"shipped")).await);
  assert!(!ok(orders.set_path("nope", "$.status", &"shipped")).await);
  assert_eq!(
    ok(orders.incr_by("o1", "$.total_cents", 250.0)).await,
    Some(1500.0)
  );
  assert_eq!(ok(orders.delete_path("o1", "$.lines")).await, 1);
  // The document no longer has every field of `Order`.
  assert!(matches!(
    run(orders.get("o1")).await,
    Err(RedisError::Decode(_))
  ));
  // `JSON.NUMINCRBY` with a float delta leaves a float.
  let raw = client.json::<serde_json::Value>("orders");
  assert_eq!(
    ok(raw.get("o1")).await,
    Some(serde_json::json!({"status": "shipped", "total_cents": 1500.0}))
  );

  ok(orders.set("o2", &o1)).await;
  assert_eq!(ok(orders.ids()).await, ["o1", "o2"]);
  assert!(ok(orders.delete("o2")).await);
  assert_eq!(ok(orders.clear()).await, 1);
  assert!(format!("{orders:?}").contains("orders"));

  // A plain string key is not a JSON document.
  ok(client.query::<()>(redis::cmd("SET").arg("orders:s").arg("x").clone())).await;
  assert!(matches!(
    run(orders.get("s")).await,
    Err(RedisError::Command(_))
  ));
}

/// The same `Cache` code, whatever holds the values.
async fn use_cache(cache: &Cache<u32, String>) -> (String, String, CacheStats) {
  let first = ok(cache.get(7)).await;
  let second = ok(cache.get(7)).await;
  let stats = ok(cache.stats::<Never, id_effect::HNil>()).await;
  (first, second, stats)
}

fn counting(calls: &Arc<AtomicUsize>) -> impl Fn(u32) -> Effect<String> + Send + Sync + 'static {
  let calls = Arc::clone(calls);
  move |id| {
    calls.fetch_add(1, Ordering::SeqCst);
    sync(move || format!("user-{id}"))
  }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cache_code_runs_in_process_or_on_redis() {
  let Some((_redis, client)) = redis_client("cache_code_runs_in_process_or_on_redis").await else {
    return;
  };
  let calls = Arc::new(AtomicUsize::new(0));
  let local: Cache<u32, String> = Cache::new(100, None, counting(&calls));
  let backend = RedisCacheBackend::new(client.clone(), "users", Some(Duration::from_millis(400)));
  let shared: Cache<u32, String> = Cache::with_backend(backend.clone(), counting(&calls));
  let expected = (
    "user-7".to_owned(),
    "user-7".to_owned(),
    CacheStats {
      hits: 1,
      misses: 1,
      size: 1,
    },
  );
  assert_eq!(use_cache(&local).await, expected);
  assert_eq!(use_cache(&shared).await, expected);
  assert_eq!(calls.load(Ordering::SeqCst), 2);
  assert_eq!(
    ok(backend.store().get("7")).await,
    Some("user-7".to_owned())
  );

  // A second instance on the same namespace shares the values: a hit, no lookup.
  let other: Cache<u32, String> = Cache::with_backend(
    RedisCacheBackend::new(client.clone(), "users", Some(Duration::from_millis(400))),
    counting(&calls),
  );
  assert_eq!(ok(other.get(7)).await, "user-7");
  assert_eq!(calls.load(Ordering::SeqCst), 2);

  // Concurrent misses in one process share one lookup.
  let slow_calls = Arc::new(AtomicUsize::new(0));
  let c = Arc::clone(&slow_calls);
  let slow: Cache<u32, String> = Cache::with_backend(
    RedisCacheBackend::new(client.clone(), "slow", None),
    move |id: u32| {
      c.fetch_add(1, Ordering::SeqCst);
      id_effect::sleep(Duration::from_millis(100)).map(move |()| format!("v{id}"))
    },
  );
  let gets = (0..6).map(|_| slow.get(1)).collect();
  let values = ok(id_effect::fork_all(gets, id_effect::Concurrency::Unbounded)).await;
  assert_eq!(values, vec!["v1".to_owned(); 6]);
  assert_eq!(slow_calls.load(Ordering::SeqCst), 1);

  // Redis expires the values after the backend's time-to-live.
  tokio::time::sleep(Duration::from_millis(600)).await;
  assert!(!ok(shared.contains::<Never, id_effect::HNil>(&7)).await);
  assert_eq!(ok(shared.get(7)).await, "user-7");
  assert_eq!(calls.load(Ordering::SeqCst), 3);

  // Invalidation reaches every instance; `invalidate_all` clears only this namespace.
  ok(other.invalidate::<Never, id_effect::HNil>(&7)).await;
  assert_eq!(
    ok(shared.get_if_present::<Never, id_effect::HNil>(&7)).await,
    None
  );
  ok(shared.set::<Never, id_effect::HNil>(8, "eight".into())).await;
  assert_eq!(ok(CacheBackend::size(&backend)).await, 1);
  ok(shared.invalidate_all::<Never, id_effect::HNil>()).await;
  assert_eq!(ok(shared.size::<Never, id_effect::HNil>()).await, 0);
  assert_eq!(ok(slow.size::<Never, id_effect::HNil>()).await, 1);
  assert_eq!(
    ok(shared.backend_errors::<Never, id_effect::HNil>()).await,
    0
  );
  assert!(format!("{backend:?}").contains("users"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pubsub_is_a_stream() {
  let Some((_redis, client)) = redis_client("pubsub_is_a_stream").await else {
    return;
  };
  let orders = ok(client.subscribe(["orders", "refunds"])).await;
  let all = ok(client.psubscribe(["ord*"])).await;
  assert_eq!(ok(client.publish("orders", "o-1")).await, 2);
  assert_eq!(
    ok(client.publish_json("refunds", &serde_json::json!({"id": 9}))).await,
    1
  );
  assert_eq!(ok(client.publish("ordinal", b"x".to_vec())).await, 1);
  assert_eq!(ok(client.publish("nobody", "x")).await, 0);

  let got = ok(orders.take(2).run_collect()).await;
  assert_eq!(
    got,
    vec![
      RedisMessage::new("orders", None, "o-1"),
      RedisMessage::new("refunds", None, r#"{"id":9}"#),
    ]
  );
  assert_eq!(got[1].json::<serde_json::Value>().unwrap()["id"], 9);
  let matched = ok(all.take(2).run_collect()).await;
  assert_eq!(matched[0].channel(), "orders");
  assert_eq!(matched[1].channel(), "ordinal");
  assert_eq!(matched[1].pattern(), Some("ord*"));

  // A message published while a consumer waits is delivered.
  let waiting = ok(client.subscribe(["late"])).await;
  let publisher = client.clone();
  let delayed = tokio::spawn(async move {
    tokio::time::sleep(Duration::from_millis(100)).await;
    ok(publisher.publish("late", "hello")).await
  });
  let late = ok(waiting.take(1).run_collect()).await;
  assert_eq!(late[0].text(), Ok("hello"));
  assert_eq!(delayed.await.unwrap(), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn client_layer_commands_and_errors() {
  let Some((_redis, client)) = redis_client("client_layer_commands_and_errors").await else {
    return;
  };
  let url = client.config().url().to_owned();

  // The layer provides the client as a capability.
  let program: Effect<i64, RedisError, RedisEnv> =
    need::<RedisClient>().flat_map(|redis| redis.kv::<i64>("visits").incr_by("/", 1).narrow());
  let layer = RedisClient::layer::<RedisError>(RedisConfig::new(url.clone()));
  assert_eq!(ok(program.provide_layer(layer)).await, 1);

  // Raw commands and pipelines, spread over the pool.
  ok(client.ping()).await;
  let (a, b): (i64, i64) = ok(
    client.query_pipeline(
      redis::pipe()
        .atomic()
        .cmd("INCR")
        .arg("n")
        .cmd("INCR")
        .arg("n")
        .clone(),
    ),
  )
  .await;
  assert_eq!((a, b), (1, 2));
  let mut conn = client.connection();
  let n: i64 = redis::cmd("GET")
    .arg("n")
    .query_async(&mut conn)
    .await
    .unwrap();
  assert_eq!(n, 2);
  assert!(
    client
      .redis()
      .get_connection_info()
      .addr()
      .to_string()
      .contains(':')
  );
  assert!(format!("{client:?}").contains("connections: 2"));

  // Server-side errors: an unknown command, a wrong type, a reply of another type.
  assert!(matches!(
    run(client.query::<()>(redis::cmd("NO.SUCH.COMMAND"))).await,
    Err(RedisError::Command(_))
  ));
  ok(client.query::<()>(redis::cmd("SET").arg("s").arg("x").clone())).await;
  assert!(matches!(
    run(client.query::<i64>(redis::cmd("LPUSH").arg("s").arg("y").clone())).await,
    Err(RedisError::Command(_))
  ));
  assert!(matches!(
    run(client.query::<i64>(redis::cmd("GET").arg("s").clone())).await,
    Err(RedisError::Decode(_))
  ));

  // A lazy client connects on first use.
  let lazy = ok(RedisClient::connect_lazy(
    RedisConfig::new(url).pool_size(1),
  ))
  .await;
  ok(lazy.ping()).await;
}

//! Ex 109 — End-to-end on [`TokioRuntime`]: typed capabilities (`need` / `provide_all`), an async
//! step, `catch`, and concurrent fibers on the Tokio runtime.
//!
//! Run: `cargo run -p id_effect_tokio --example 109_tokio_end_to_end`

use std::time::Duration;

use id_effect::Exit;
use id_effect::{Effect, HCons, HNil, from_async, hlist, need, sleep, succeed};
use id_effect_tokio::TokioRuntime;

struct ApiToken(&'static str);
struct MinPrice(f64);

type AppEnv = HCons<ApiToken, HCons<MinPrice, HNil>>;

#[derive(Debug, Clone, PartialEq)]
struct Quote {
  symbol: &'static str,
  price: f64,
}

#[derive(Debug, Clone, PartialEq)]
struct Report {
  kept_quotes: usize,
  total_notional: f64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum AppError {
  MissingApiToken,
}

/// An async "fetch" that needs the API token.
fn fetch_quotes() -> Effect<Vec<Quote>, AppError, AppEnv> {
  need::<ApiToken>().flat_map(|token| {
    let missing = token.0.is_empty();
    from_async(move || async move {
      if missing {
        return Err(AppError::MissingApiToken);
      }
      tokio::task::yield_now().await;
      Ok(vec![
        Quote {
          symbol: "SOL",
          price: 190.0,
        },
        Quote {
          symbol: "BTC",
          price: 64_000.0,
        },
        Quote {
          symbol: "BONK",
          price: 0.000_03,
        },
      ])
    })
  })
}

fn market_report() -> Effect<Report, AppError, AppEnv> {
  need::<MinPrice>().flat_map(|min| {
    fetch_quotes().map(move |quotes| {
      let kept: Vec<Quote> = quotes
        .into_iter()
        .filter(|q| q.price >= min.0 && !q.symbol.is_empty())
        .collect();
      Report {
        kept_quotes: kept.len(),
        total_notional: kept.iter().map(|q| q.price).sum(),
      }
    })
  })
}

fn main() {
  let rt = TokioRuntime::new_multi_thread().expect("tokio runtime should build");

  let ok = market_report().provide_all(hlist![ApiToken("secret-token"), MinPrice(1.0)]);
  let report = rt.block_on(rt.run(ok));
  assert!(matches!(
    report,
    Exit::Success(Report { kept_quotes: 2, total_notional }) if total_notional == 64_190.0
  ));

  let recovered = market_report()
    .catch(|AppError::MissingApiToken| {
      succeed::<_, AppError, AppEnv>(Report {
        kept_quotes: 0,
        total_notional: 0.0,
      })
    })
    .provide_all(hlist![ApiToken(""), MinPrice(1.0)]);
  assert!(matches!(
    rt.block_on(rt.run(recovered)),
    Exit::Success(Report { kept_quotes: 0, .. })
  ));

  // Fibers are Send tokio tasks: 100 concurrent sleepers park on the tokio timer.
  let joined = rt.block_on(async {
    let fibers: Vec<_> = (0..100_u64)
      .map(|i| rt.fork(sleep::<(), HNil>(Duration::from_millis(5)).map(move |()| i)))
      .collect();
    let mut sum = 0;
    for f in fibers {
      if let Exit::Success(i) = f.join_future().await {
        sum += i;
      }
    }
    sum
  });
  assert_eq!(joined, (0..100).sum::<u64>());

  println!("109_tokio_end_to_end ok");
}

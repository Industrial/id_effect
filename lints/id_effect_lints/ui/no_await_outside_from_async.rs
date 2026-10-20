// C-01: `.await` outside a `from_async` wrapper.
#![allow(dead_code)]

fn from_async<F, Fut>(_f: F)
where
  F: FnOnce() -> Fut,
{
}

fn sum() -> impl std::future::Future<Output = u8> {
  async { std::future::ready(1u8).await + 1 }
}

// Inside the `from_async` wrapper `.await` is the point.
fn wrapped() {
  from_async(|| async { std::future::ready(1u8).await });
}

fn main() {}

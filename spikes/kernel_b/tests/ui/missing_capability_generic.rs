// (1b) Missing capability inside generic code: a helper needs `Logger`, the caller's bounds
// only promise `Db`. rustc's native "consider restricting" help applies.
use kernel_b::{Effect, Has, Never, effect};
use spike_corpus::{Db, DbError, Logger};

fn log_line<R: Has<Logger>>(msg: &'static str) -> Effect<(), Never, R> {
  effect!(|r| ~Logger.info(msg))
}

fn count_and_log<R: Has<Db>>() -> Effect<usize, DbError, R> {
  effect!(|r| {
    ~ log_line("counting");
    ~Db.count("users").await?
  })
}

fn main() {
  let _ = count_and_log().provide(Db::default()).block_on();
}

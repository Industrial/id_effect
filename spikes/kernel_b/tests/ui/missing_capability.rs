// (1) Missing capability at the edge: the handler needs `Mailer`, the app env has none.
use kernel_b::{Effect, Env, Has, effect};
use spike_corpus::{AppError, Db, Mailer};

fn notify<R: Has<Db> + Has<Mailer>>() -> Effect<(), AppError, R> {
  effect!(|r| {
    let n = ~Db.count("users").await.map_err(AppError::from)?;
    ~Mailer.send("ops@example.com", &n.to_string()).await.map_err(AppError::from)?;
  })
}

#[derive(Clone, Env)]
struct AppEnv {
  db: Db,
}

fn main() {
  let env = AppEnv { db: Db::default() };
  let _ = notify().provide(env).block_on();
}

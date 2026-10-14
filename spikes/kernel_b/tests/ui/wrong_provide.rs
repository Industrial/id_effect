// (3) Wrong provide: overriding a slot with a value of a different type than the slot.
use kernel_b::{Effect, Env, Has, Never, effect};
use spike_corpus::{Logger, Mailer};

#[derive(Clone, Default)]
struct MockMailer;

fn send<R: Has<Mailer>>() -> Effect<(), Never, R> {
  effect!(|r| {
    let _ = ~Mailer.send("a@b", "hi").await;
  })
}

#[derive(Clone, Env)]
struct AppEnv {
  mailer: Mailer,
  logger: Logger,
}

fn main() {
  let env = AppEnv {
    mailer: Mailer::default(),
    logger: Logger::default(),
  };
  let _ = send().provide_service(MockMailer).provide(env).block_on();
}

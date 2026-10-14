// (2b) A duplicate the macro cannot see: two layers that both need `ConfigSource`, merged side by
// side, concatenate their inputs into `caps!(ConfigSource, ConfigSource)`. Narrowing into that
// env is ambiguous (design A risk R1).
use kernel_a::{Layer, caps, effect, hlist};
use spike_corpus::{AppError, ConfigSource, Db, Mailer};

fn db_layer() -> Layer<caps!(Db), AppError, caps!(ConfigSource)> {
  Layer::from_effect(effect!({
    let _url = ~ConfigSource.get_string("db_url")?;
    hlist![Db::default()]
  }))
}

fn mail_layer() -> Layer<caps!(Mailer), AppError, caps!(ConfigSource)> {
  Layer::from_effect(effect!({
    let _relay = ~ConfigSource.get_string("smtp")?;
    hlist![Mailer::default()]
  }))
}

fn main() {
  let _infra = db_layer().merge(mail_layer());
}

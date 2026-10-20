//! Cron expression parse (id_effect `Cron::parse`) and next-fire computation: a parsed
//! expression is evaluated in UTC and in a DST zone from a fixed instant.
//!
//! Checks besides "no panic": the expression text re-parses; in UTC (no gaps, no folds) the next
//! fire time is after the start and matches the expression.
#![no_main]

use id_effect::{Cron, UtcDateTime, timezone};
use libfuzzer_sys::fuzz_target;

/// 2026-03-08T00:00Z, the day New York springs forward.
const START_MS: i64 = 1_772_928_000_000;

fuzz_target!(|data: &[u8]| {
  let Ok(text) = std::str::from_utf8(data) else {
    return;
  };
  let Ok(cron) = Cron::parse(text) else {
    return;
  };
  assert!(
    Cron::parse(cron.expression()).is_ok(),
    "{:?} does not re-parse",
    cron.expression()
  );
  let start = UtcDateTime::from_epoch_millis(START_MS).expect("a valid instant");
  if let Some(next) = cron.next_after(start) {
    assert!(
      next.to_epoch_millis() > start.to_epoch_millis(),
      "{text:?}: next fire is not after the start"
    );
    assert!(
      cron.matches(&next),
      "{text:?}: the next fire time does not match"
    );
  }
  if let Some(zone) = timezone::from_str("America/New_York") {
    let zoned = cron.clone().in_zone(zone);
    let _ = zoned.upcoming(start).take(3).count();
  }
});

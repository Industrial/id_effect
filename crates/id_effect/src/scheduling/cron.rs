//! Cron expressions with time-zone- and DST-aware next-fire computation, the [`WallClock`] that
//! maps the executor clock to civil time, and [`Schedule::cron`].
//!
//! # Syntax
//!
//! [`Cron::parse`] accepts the classic five fields `minute hour day-of-month month day-of-week`
//! (the second is then `0`) or six fields with a leading `second`. Each field is a comma list of
//! `*`, a value, a range `a-b`, or a step `*/n`, `a/n`, `a-b/n`. Months and weekdays also take
//! three-letter names (`JAN`..`DEC`, `SUN`..`SAT`, any case); weekday `7` is Sunday, like `0`.
//! `?` is accepted as `*` in the day fields. The macros `@yearly` (`@annually`), `@monthly`,
//! `@weekly`, `@daily` (`@midnight`) and `@hourly` are recognised.
//!
//! As in Vixie cron, when both day-of-month and day-of-week are restricted (neither starts with
//! `*` or `?`), a day matches when **either** does; otherwise both must.
//!
//! # Time zones and DST
//!
//! An expression is matched against **civil (wall-clock) time** in its zone (UTC unless set with
//! [`Cron::in_zone`]); the zone rules come from `jiff` (the same as
//! [`ZonedDateTime`](super::ZonedDateTime)). Every matching civil time fires **at most once**:
//!
//! - in a DST **gap** (spring forward) a civil time that does not exist fires at the same
//!   civil time shifted forward by the gap (`02:30` on a 1-hour gap fires at `03:30`, the
//!   instant `jiff` calls "compatible");
//! - in a DST **fold** (fall back) a civil time that happens twice fires at its first
//!   occurrence only, so a daily `01:30` job runs once that night (an hourly job does not run
//!   again in the repeated hour).
//!
//! ```
//! use id_effect::{Cron, UtcDateTime, timezone};
//!
//! let cron = Cron::parse("30 2 * * *")
//!   .unwrap()
//!   .in_zone(timezone::from_str("America/New_York").unwrap());
//! // 2026-03-08 is the spring-forward day: 02:30 does not exist and fires at 03:30 EDT.
//! let start = UtcDateTime::from_epoch_millis(1_772_928_000_000).unwrap(); // 2026-03-08T00:00Z
//! let next = cron.next_after(start).unwrap();
//! assert_eq!(next.format_iso(), "2026-03-08T03:30:00-04:00[America/New_York]");
//! ```
//!
//! # Wall clock
//!
//! The runtime's clock is a monotonic [`Instant`] (virtual on
//! [`TestExecutor`](crate::TestExecutor)), while a cron expression speaks civil time. A
//! [`WallClock`] maps one to the other: [`WallClock::system`] reads the system clock;
//! [`WallClock::anchored`] (or [`WallClock::starting_at`]) pins an executor instant to a chosen
//! date-time, so under a `TestExecutor` civil time advances exactly with
//! `TestExecutor::advance`, and [`WallClock::jump`] simulates a suspended process or a clock
//! step.

use std::fmt;
use std::future::ready;
use std::str::FromStr;
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};
use std::time::{Duration, Instant};

use jiff::civil::{Date, DateTime};
use jiff::tz::{AmbiguousOffset, TimeZone};
use jiff::{SignedDuration, Timestamp, ToSpan};

use super::datetime::{UtcDateTime, ZonedDateTime};
use super::schedule::Schedule;
use crate::kernel::{Ctx, Effect};

const MONTH_NAMES: [&str; 12] = [
  "JAN", "FEB", "MAR", "APR", "MAY", "JUN", "JUL", "AUG", "SEP", "OCT", "NOV", "DEC",
];
const DAY_NAMES: [&str; 7] = ["SUN", "MON", "TUE", "WED", "THU", "FRI", "SAT"];

/// A leap day can be up to eight years away (1896 → 1904); past that, an expression never
/// fires.
const SEARCH_YEARS: i16 = 9;

// ── errors ──────────────────────────────────────────────────────────────────────────────────

/// A cron expression that [`Cron::parse`] rejected.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CronParseError {
  /// The expression as given.
  pub expression: String,
  /// What is wrong with it.
  pub reason: String,
}

impl fmt::Display for CronParseError {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    write!(
      f,
      "invalid cron expression {:?}: {}",
      self.expression, self.reason
    )
  }
}

impl std::error::Error for CronParseError {}

// ── the expression ──────────────────────────────────────────────────────────────────────────

/// A parsed cron expression and the zone it is evaluated in (see the [module docs](self)).
///
/// Cheap to clone; immutable.
#[derive(Clone)]
pub struct Cron {
  source: Arc<str>,
  seconds: u64,
  minutes: u64,
  hours: u64,
  days: u64,
  months: u64,
  weekdays: u64,
  /// Day-of-month / day-of-week was `*` or `?` (selects AND rather than OR for the days).
  dom_any: bool,
  dow_any: bool,
  zone: TimeZone,
}

impl fmt::Debug for Cron {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("Cron")
      .field("expression", &&*self.source)
      .field("zone", &self.zone.iana_name().unwrap_or("<fixed>"))
      .finish()
  }
}

impl fmt::Display for Cron {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.write_str(&self.source)
  }
}

impl FromStr for Cron {
  type Err = CronParseError;

  fn from_str(s: &str) -> Result<Self, Self::Err> {
    Self::parse(s)
  }
}

impl Cron {
  /// Parses a five- or six-field expression or a macro (see the [module docs](self)), in UTC.
  pub fn parse(expression: &str) -> Result<Self, CronParseError> {
    let err = |reason: String| CronParseError {
      expression: expression.to_string(),
      reason,
    };
    let text = expression.trim();
    let lower = text.to_ascii_lowercase();
    let expanded = match lower.as_str() {
      "@yearly" | "@annually" => "0 0 1 1 *",
      "@monthly" => "0 0 1 * *",
      "@weekly" => "0 0 * * 0",
      "@daily" | "@midnight" => "0 0 * * *",
      "@hourly" => "0 * * * *",
      s if s.starts_with('@') => return Err(err(format!("unknown macro {text:?}"))),
      _ => text,
    };
    let fields: Vec<&str> = expanded.split_whitespace().collect();
    let (second, rest) = match fields.len() {
      5 => ("0", &fields[..]),
      6 => (fields[0], &fields[1..]),
      n => return Err(err(format!("expected 5 or 6 fields, found {n}"))),
    };
    let weekdays = field(rest[4], "day-of-week", 0, 7, &DAY_NAMES, 0, true).map_err(err)?;
    Ok(Self {
      source: Arc::from(text),
      seconds: field(second, "second", 0, 59, &[], 0, false).map_err(err)?,
      minutes: field(rest[0], "minute", 0, 59, &[], 0, false).map_err(err)?,
      hours: field(rest[1], "hour", 0, 23, &[], 0, false).map_err(err)?,
      days: field(rest[2], "day-of-month", 1, 31, &[], 0, true).map_err(err)?,
      months: field(rest[3], "month", 1, 12, &MONTH_NAMES, 1, false).map_err(err)?,
      // Weekday 7 is Sunday (bit 0).
      weekdays: (weekdays & 0x7f) | ((weekdays >> 7) & 1),
      dom_any: rest[2].starts_with(['*', '?']),
      dow_any: rest[4].starts_with(['*', '?']),
      zone: TimeZone::UTC,
    })
  }

  /// This expression evaluated in `zone` (an IANA zone follows its DST rules).
  pub fn in_zone(mut self, zone: TimeZone) -> Self {
    self.zone = zone;
    self
  }

  /// The zone this expression is evaluated in.
  pub fn zone(&self) -> &TimeZone {
    &self.zone
  }

  /// The expression text as parsed.
  pub fn expression(&self) -> &str {
    &self.source
  }

  /// `true` when the civil time of `at` in this expression's zone matches every field.
  pub fn matches(&self, at: &ZonedDateTime) -> bool {
    let dt = at.0.timestamp().to_zoned(self.zone.clone()).datetime();
    self.day_matches(dt.date()) && self.time_after(dt) == Some(dt)
  }

  /// The first fire time strictly after `after`, in this expression's zone, or `None` when the
  /// expression never fires again (for example `0 0 30 2 *`).
  pub fn next_after(&self, after: UtcDateTime) -> Option<ZonedDateTime> {
    let after = after.0;
    let local = after.to_zoned(self.zone.clone()).datetime();
    let mut from = local
      .date()
      .at(local.hour(), local.minute(), local.second(), 0)
      .checked_add(1.second())
      .ok()?;
    loop {
      let dt = self.next_civil(from)?;
      if let Some(t) = self.resolve(dt, after) {
        return Some(ZonedDateTime(t.to_zoned(self.zone.clone())));
      }
      // Only a fold's first occurrence can lie before `after`; search on past it.
      from = dt.checked_add(1.second()).ok()?;
    }
  }

  /// The fire times strictly after `after`, in order.
  pub fn upcoming(&self, after: UtcDateTime) -> impl Iterator<Item = ZonedDateTime> + '_ {
    std::iter::successors(self.next_after(after), move |z| {
      self.next_after(UtcDateTime(z.0.timestamp()))
    })
  }

  /// The instant a matching civil time `dt` fires at, if it is after `after`.
  fn resolve(&self, dt: DateTime, after: Timestamp) -> Option<Timestamp> {
    let amb = self.zone.to_ambiguous_timestamp(dt);
    match amb.offset() {
      AmbiguousOffset::Fold { .. } => [amb.earlier().ok(), amb.later().ok()]
        .into_iter()
        .flatten()
        .find(|t| *t > after),
      _ => amb.compatible().ok().filter(|t| *t > after),
    }
  }

  /// The first civil date-time at or after `from` matching every field.
  fn next_civil(&self, from: DateTime) -> Option<DateTime> {
    let limit = from.year().saturating_add(SEARCH_YEARS);
    let mut date = from.date();
    let mut time = Some(from);
    while date.year() <= limit {
      if !has(self.months, date.month()) {
        date = date.first_of_month().checked_add(1.month()).ok()?;
        time = None;
        continue;
      }
      if self.day_matches(date) {
        let start = time.unwrap_or_else(|| date.at(0, 0, 0, 0));
        if let Some(dt) = self.time_after(start) {
          return Some(dt);
        }
      }
      date = date.tomorrow().ok()?;
      time = None;
    }
    None
  }

  fn day_matches(&self, date: Date) -> bool {
    let dom = has(self.days, date.day());
    let dow = has(self.weekdays, date.weekday().to_sunday_zero_offset());
    if self.dom_any || self.dow_any {
      dom && dow
    } else {
      dom || dow
    }
  }

  /// The first time of `start`'s day at or after `start` matching hour, minute and second.
  fn time_after(&self, start: DateTime) -> Option<DateTime> {
    let (h0, m0, s0) = (start.hour(), start.minute(), start.second());
    for h in h0..24 {
      if !has(self.hours, h) {
        continue;
      }
      let first_m = if h == h0 { m0 } else { 0 };
      for m in first_m..60 {
        if !has(self.minutes, m) {
          continue;
        }
        let first_s = if h == h0 && m == m0 { s0 } else { 0 };
        if let Some(s) = first_set(self.seconds, first_s) {
          return Some(start.date().at(h, m, s, 0));
        }
      }
    }
    None
  }
}

fn has(mask: u64, v: i8) -> bool {
  (0..64).contains(&v) && mask & (1 << v) != 0
}

/// The lowest set bit of `mask` at or above `from`.
fn first_set(mask: u64, from: i8) -> Option<i8> {
  let from = u32::try_from(from).ok().filter(|f| *f < 64)?;
  let rest = mask & (u64::MAX << from);
  (rest != 0).then(|| rest.trailing_zeros() as i8)
}

/// Parses one field into a bit mask over `min..=max`. `names[i]` stands for `i + name_base`.
fn field(
  text: &str,
  name: &str,
  min: u32,
  max: u32,
  names: &[&str],
  name_base: u32,
  question: bool,
) -> Result<u64, String> {
  let value = |s: &str| -> Result<u32, String> {
    let v = match s.parse::<u32>() {
      Ok(v) => v,
      Err(_) => names
        .iter()
        .position(|n| n.eq_ignore_ascii_case(s))
        .map(|i| i as u32 + name_base)
        .ok_or_else(|| format!("{name} field: {s:?} is not a value"))?,
    };
    if (min..=max).contains(&v) {
      Ok(v)
    } else {
      Err(format!("{name} field: {v} is outside {min}-{max}"))
    }
  };
  let mut mask = 0u64;
  for part in text.split(',') {
    let (range, step) = match part.split_once('/') {
      Some((r, s)) => match s.parse::<u32>() {
        Ok(step) if step > 0 => (r, Some(step)),
        _ => return Err(format!("{name} field: bad step {s:?}")),
      },
      None => (part, None),
    };
    let (lo, hi) = if range == "*" || (question && range == "?") {
      (min, max)
    } else if let Some((a, b)) = range.split_once('-') {
      (value(a)?, value(b)?)
    } else if range.is_empty() {
      return Err(format!("{name} field: empty item in {text:?}"));
    } else {
      let v = value(range)?;
      (v, if step.is_some() { max } else { v })
    };
    if lo > hi {
      return Err(format!("{name} field: range {range:?} is backwards"));
    }
    let mut v = lo;
    while v <= hi {
      mask |= 1 << v;
      v += step.unwrap_or(1);
    }
  }
  Ok(mask)
}

// ── wall clock ──────────────────────────────────────────────────────────────────────────────

/// Maps the executor's monotonic clock to civil (UTC) time for cron scheduling (see the
/// [module docs](self)). Cloning gives another handle to the same clock.
#[derive(Clone, Default)]
pub struct WallClock {
  anchor: Option<Arc<Anchor>>,
}

struct Anchor {
  instant: Instant,
  at: Timestamp,
  /// Forward jumps applied by [`WallClock::jump`], in nanoseconds.
  jumped: AtomicI64,
}

impl fmt::Debug for WallClock {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    match &self.anchor {
      None => f.write_str("WallClock::System"),
      Some(a) => f
        .debug_struct("WallClock::Anchored")
        .field("at", &a.at)
        .field("jumped_ns", &a.jumped.load(Ordering::Relaxed))
        .finish(),
    }
  }
}

impl WallClock {
  /// The system's real-time clock (read on every step; it ignores the executor instant).
  pub fn system() -> Self {
    Self { anchor: None }
  }

  /// A clock that reads `at` at executor instant `instant` and advances with the executor
  /// clock from there (virtual time on a `TestExecutor`).
  pub fn anchored(instant: Instant, at: UtcDateTime) -> Self {
    Self {
      anchor: Some(Arc::new(Anchor {
        instant,
        at: at.0,
        jumped: AtomicI64::new(0),
      })),
    }
  }

  /// [`anchored`](Self::anchored) at the running fiber's executor instant.
  pub fn starting_at<E, R>(at: UtcDateTime) -> Effect<Self, E, R>
  where
    E: Send + 'static,
    R: Send + Sync + 'static,
  {
    Effect::from_ctx(move |ctx: Ctx<R>| ready(Ok(Self::anchored(ctx.fiber().executor().now(), at))))
  }

  /// The civil instant at executor instant `now`.
  pub fn at(&self, now: Instant) -> UtcDateTime {
    let Some(a) = &self.anchor else {
      return UtcDateTime(Timestamp::now());
    };
    let elapsed = now.saturating_duration_since(a.instant);
    let jumped = SignedDuration::from_nanos(a.jumped.load(Ordering::SeqCst));
    UtcDateTime(
      a.at
        .checked_add(elapsed)
        .and_then(|t| t.checked_add(jumped))
        .unwrap_or(Timestamp::MAX),
    )
  }

  /// The civil instant now, on the running fiber's executor clock.
  pub fn now<E, R>(&self) -> Effect<UtcDateTime, E, R>
  where
    E: Send + 'static,
    R: Send + Sync + 'static,
  {
    let clock = self.clone();
    Effect::from_ctx(move |ctx: Ctx<R>| ready(Ok(clock.at(ctx.fiber().executor().now()))))
  }

  /// Moves an anchored clock's civil time forward by `by` without moving the executor clock:
  /// a suspended process or a clock step, as seen by a cron scheduler (missed runs).
  ///
  /// # Panics
  ///
  /// On [`WallClock::system`], which cannot be moved.
  pub fn jump(&self, by: Duration) {
    let a = self
      .anchor
      .as_ref()
      .expect("WallClock::jump: the system clock cannot be moved");
    let ns = i64::try_from(by.as_nanos()).unwrap_or(i64::MAX);
    a.jumped.fetch_add(ns, Ordering::SeqCst);
  }
}

/// The delay from `now` until `next` (zero when `next` is not after `now`).
pub(crate) fn delay_until(now: UtcDateTime, next: &ZonedDateTime) -> Duration {
  let d = next.0.timestamp().as_duration() - now.0.as_duration();
  Duration::try_from(d).unwrap_or(Duration::ZERO)
}

impl<In: 'static> Schedule<Option<ZonedDateTime>, In> {
  /// Recurs at each fire time of `cron`, read on `clock`: every step sleeps until the next fire
  /// time after the current civil instant and outputs it; done (output `None`) when `cron`
  /// never fires again.
  ///
  /// `Effect::repeat` runs its effect once before the first step, so the first run is
  /// immediate; use [`CronRegistry`](crate::CronRegistry) for jobs that should only run at fire
  /// times, with overlap and missed-run policies and run history.
  pub fn cron(cron: Cron, clock: WallClock) -> Self {
    Self::from_fn(
      || (),
      move |now, _, ()| {
        let wall = clock.at(now);
        match cron.next_after(wall) {
          Some(next) => {
            let d = delay_until(wall, &next);
            ((), Some(next), Some(d))
          }
          None => ((), None, None),
        }
      },
    )
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  fn utc(s: &str) -> UtcDateTime {
    UtcDateTime(s.parse().expect("timestamp"))
  }

  fn next(expr: &str, after: &str) -> Option<String> {
    Cron::parse(expr)
      .expect("parse")
      .next_after(utc(after))
      .map(|z| z.0.timestamp().to_string())
  }

  #[test]
  fn fields_steps_lists_and_names() {
    assert_eq!(
      next("*/15 * * * *", "2026-01-01T00:07:00Z").as_deref(),
      Some("2026-01-01T00:15:00Z")
    );
    assert_eq!(
      next("0 9-17/4 * * MON-fri", "2026-01-01T10:00:00Z").as_deref(), // a Thursday
      Some("2026-01-01T13:00:00Z")
    );
    assert_eq!(
      next("0 0 1 jan,JUL *", "2026-02-01T00:00:00Z").as_deref(),
      Some("2026-07-01T00:00:00Z")
    );
    assert_eq!(
      next("*/10 * * * * *", "2026-01-01T00:00:05.5Z").as_deref(),
      Some("2026-01-01T00:00:10Z")
    );
    assert_eq!(
      next("@weekly", "2026-01-01T00:00:00Z").as_deref(),
      Some("2026-01-04T00:00:00Z")
    );
    assert_eq!(
      next("0 0 * * 7", "2026-01-01T00:00:00Z").as_deref(),
      Some("2026-01-04T00:00:00Z")
    );
  }

  #[test]
  fn restricted_day_fields_are_or_ed() {
    // The 15th, or any Monday: Monday 2026-01-05 comes first.
    assert_eq!(
      next("0 0 15 * 1", "2026-01-01T00:00:00Z").as_deref(),
      Some("2026-01-05T00:00:00Z")
    );
    // `*/1` starts with `*`: AND semantics, so any day that is the 15th.
    assert_eq!(
      next("0 0 15 * */1", "2026-01-01T00:00:00Z").as_deref(),
      Some("2026-01-15T00:00:00Z")
    );
    assert_eq!(
      next("0 0 15 * ?", "2026-01-01T00:00:00Z").as_deref(),
      Some("2026-01-15T00:00:00Z")
    );
  }

  #[test]
  fn leap_day_and_never() {
    assert_eq!(
      next("0 0 29 2 *", "2026-01-01T00:00:00Z").as_deref(),
      Some("2028-02-29T00:00:00Z")
    );
    assert_eq!(next("0 0 30 2 *", "2026-01-01T00:00:00Z"), None);
  }

  #[test]
  fn parse_errors() {
    for bad in [
      "",
      "* * * *",
      "* * * * * * *",
      "60 * * * *",
      "* 24 * * *",
      "* * 0 * *",
      "* * * 13 *",
      "* * * * 8",
      "5-1 * * * *",
      "*/0 * * * *",
      "a * * * *",
      "1,,2 * * * *",
      "@reboot",
      "? * * * *",
    ] {
      let e = Cron::parse(bad).expect_err(bad);
      assert_eq!(e.expression, bad);
      assert!(e.to_string().contains("invalid cron expression"), "{e}");
    }
  }

  #[test]
  fn matches_display_and_debug() {
    let c: Cron = "0 30 9 * * *".parse().expect("parse");
    assert_eq!(c.to_string(), "0 30 9 * * *");
    assert_eq!(c.expression(), "0 30 9 * * *");
    assert!(format!("{c:?}").contains("UTC"));
    let at = utc("2026-05-05T09:30:00Z").to_zoned(TimeZone::UTC);
    assert!(c.matches(&at));
    assert!(!c.matches(&utc("2026-05-05T09:30:01Z").to_zoned(TimeZone::UTC)));
    let fixed = c.in_zone(crate::timezone::offset(60));
    assert!(format!("{fixed:?}").contains("<fixed>"));
    assert!(!fixed.matches(&at));
  }

  #[test]
  fn upcoming_is_in_order() {
    let c = Cron::parse("@hourly").expect("parse");
    let got: Vec<String> = c
      .upcoming(utc("2026-01-01T00:30:00Z"))
      .take(3)
      .map(|z| z.0.timestamp().to_string())
      .collect();
    assert_eq!(
      got,
      [
        "2026-01-01T01:00:00Z",
        "2026-01-01T02:00:00Z",
        "2026-01-01T03:00:00Z"
      ]
    );
  }

  #[test]
  fn wall_clock_anchor_and_jump() {
    let t0 = Instant::now();
    let clock = WallClock::anchored(t0, utc("2026-01-01T00:00:00Z"));
    assert_eq!(clock.at(t0), utc("2026-01-01T00:00:00Z"));
    assert_eq!(
      clock.at(t0 + Duration::from_secs(60)),
      utc("2026-01-01T00:01:00Z")
    );
    clock.jump(Duration::from_secs(3600));
    assert_eq!(clock.at(t0), utc("2026-01-01T01:00:00Z"));
    assert!(format!("{clock:?}").contains("Anchored"));
    assert!(format!("{:?}", WallClock::system()).contains("System"));
  }

  #[test]
  #[should_panic(expected = "system clock cannot be moved")]
  fn system_clock_cannot_jump() {
    WallClock::system().jump(Duration::from_secs(1));
  }
}

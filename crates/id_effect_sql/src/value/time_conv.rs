//! jiff → `time` for the sqlx MySQL driver (`time` never appears in the public API). PostgreSQL
//! has its own wire codecs (`pg::datetime`).
//!
//! Both crates span years -9999..=9999, so these conversions do not fail in practice; a failure
//! is still reported as an error, never a panic.
#![cfg(feature = "mysql")]

/// An instant as a UTC `OffsetDateTime` (via unix nanoseconds).
pub(crate) fn timestamp(ts: jiff::Timestamp) -> Result<time::OffsetDateTime, String> {
  time::OffsetDateTime::from_unix_timestamp_nanos(ts.as_nanosecond()).map_err(|e| e.to_string())
}

/// A calendar date.
pub(crate) fn date(d: jiff::civil::Date) -> Result<time::Date, String> {
  let month = time::Month::try_from(d.month().unsigned_abs()).map_err(|e| e.to_string())?;
  time::Date::from_calendar_date(i32::from(d.year()), month, d.day().unsigned_abs())
    .map_err(|e| e.to_string())
}

/// A time of day (nanosecond precision).
pub(crate) fn time_of_day(t: jiff::civil::Time) -> Result<time::Time, String> {
  time::Time::from_hms_nano(
    t.hour().unsigned_abs(),
    t.minute().unsigned_abs(),
    t.second().unsigned_abs(),
    t.subsec_nanosecond().unsigned_abs(),
  )
  .map_err(|e| e.to_string())
}

/// A wall-clock date and time.
pub(crate) fn datetime(dt: jiff::civil::DateTime) -> Result<time::PrimitiveDateTime, String> {
  Ok(time::PrimitiveDateTime::new(
    date(dt.date())?,
    time_of_day(dt.time())?,
  ))
}

#[cfg(test)]
mod tests {
  use jiff::civil::{date as jdate, time as jtime};

  use super::*;

  #[test]
  fn converts_the_extremes_and_a_leap_day() {
    let ts = jiff::Timestamp::from_microsecond(-1_234_567).unwrap();
    assert_eq!(
      timestamp(ts).unwrap().unix_timestamp_nanos(),
      -1_234_567_000
    );
    for d in [jdate(-9999, 1, 1), jdate(2024, 2, 29), jdate(9999, 12, 31)] {
      let t = date(d).unwrap();
      assert_eq!(
        (t.year(), u8::from(t.month()), t.day()),
        (
          i32::from(d.year()),
          d.month().unsigned_abs(),
          d.day().unsigned_abs()
        )
      );
    }
    let t = time_of_day(jtime(23, 59, 59, 999_999_999)).unwrap();
    assert_eq!(
      (t.hour(), t.minute(), t.second(), t.nanosecond()),
      (23, 59, 59, 999_999_999)
    );
    let dt = datetime(jdate(2000, 1, 1).at(0, 0, 0, 1_000)).unwrap();
    assert_eq!(
      dt.assume_utc().unix_timestamp_nanos(),
      946_684_800_000_001_000
    );
  }
}

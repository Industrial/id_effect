//! sea-query's `time` values → jiff (the public API speaks jiff; sea-query's own jiff values need
//! no conversion).
//!
//! Without time's `large-dates` both crates span years -9999..=9999, so only instants near the
//! ends of that range (where jiff's `Timestamp` stops a day short) fail; a failure is an error
//! message, never a panic.

/// An instant (unix seconds + nanoseconds; the offset is dropped, the instant kept). Not via
/// `Timestamp::from_nanosecond`: jiff-core 0.1.1 trips a debug assertion there for out-of-range
/// instants instead of returning its error.
pub(super) fn timestamp(odt: time::OffsetDateTime) -> Result<jiff::Timestamp, String> {
  jiff::Timestamp::new(odt.unix_timestamp(), odt.nanosecond() as i32).map_err(|e| e.to_string())
}

/// A calendar date.
pub(super) fn date(d: time::Date) -> Result<jiff::civil::Date, String> {
  let year = i16::try_from(d.year()).map_err(|e| e.to_string())?;
  jiff::civil::Date::new(year, u8::from(d.month()) as i8, d.day() as i8).map_err(|e| e.to_string())
}

/// A time of day (nanosecond precision; always in range).
pub(super) fn time_of_day(t: time::Time) -> jiff::civil::Time {
  jiff::civil::Time::constant(
    t.hour() as i8,
    t.minute() as i8,
    t.second() as i8,
    t.nanosecond() as i32,
  )
}

/// A wall-clock date and time.
pub(super) fn datetime(dt: time::PrimitiveDateTime) -> Result<jiff::civil::DateTime, String> {
  Ok(date(dt.date())?.to_datetime(time_of_day(dt.time())))
}

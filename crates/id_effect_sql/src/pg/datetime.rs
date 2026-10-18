//! PostgreSQL's binary date-time format ↔ jiff, as sqlx `Type` / `Encode` / `Decode` impls of
//! our own (sqlx's `time` or `chrono` codecs are not used).
//!
//! On the wire `TIMESTAMPTZ` (UTC) and `TIMESTAMP` are an `i64` of microseconds since
//! 2000-01-01T00:00:00, `DATE` an `i32` of days since 2000-01-01 and `TIME` an `i64` of
//! microseconds since midnight. The maximum and minimum of those integers are `infinity` and
//! `-infinity`.
//!
//! Every conversion is checked: `±infinity`, values beyond jiff's years ±9999 and values before
//! pg's earliest (4714-11-24 BC) are an error, never a panic. Encoding drops sub-microsecond
//! nanoseconds (towards the past, so the civil fields are truncated).

use jiff::SignedDuration;
use jiff::civil::{Date, DateTime, Time, date};
use sqlx::decode::Decode;
use sqlx::encode::{Encode, IsNull};
use sqlx::error::BoxDynError;
use sqlx::postgres::types::Oid;
use sqlx::postgres::{PgArgumentBuffer, PgTypeInfo, PgValueFormat, PgValueRef, Postgres};

/// pg's epoch, 2000-01-01T00:00:00 (UTC for `TIMESTAMPTZ`).
const PG_EPOCH_DATE: Date = date(2000, 1, 1);
const PG_EPOCH_DATETIME: DateTime = PG_EPOCH_DATE.at(0, 0, 0, 0);
/// Nanoseconds from the unix epoch to pg's.
const PG_EPOCH_UNIX_NANOS: i128 = 946_684_800_000_000_000;
const DAY_MICROS: i64 = 86_400_000_000;
/// pg's earliest timestamp and date, 4714-11-24 00:00:00 BC (julian day 0), on the wire.
const PG_MIN_MICROS: i64 = -211_813_488_000_000_000;
const PG_MIN_DAYS: i32 = -2_451_545;

/// A `TIMESTAMPTZ`: an instant.
pub(super) struct PgTimestamp(pub(super) jiff::Timestamp);

/// A `TIMESTAMP`: a wall-clock date and time.
pub(super) struct PgDateTime(pub(super) DateTime);

/// A `DATE`.
pub(super) struct PgDate(pub(super) Date);

/// A `TIME` (without time zone).
pub(super) struct PgTime(pub(super) Time);

/// `v` saturated to `i64` (never reached for jiff's years ±9999: ~3·10^17 µs).
fn saturate(v: i128) -> i64 {
  i64::try_from(v).unwrap_or(if v < 0 { i64::MIN } else { i64::MAX })
}

/// Microseconds since pg's epoch (UTC), rounded towards the past.
fn timestamp_micros(ts: jiff::Timestamp) -> i64 {
  saturate((ts.as_nanosecond() - PG_EPOCH_UNIX_NANOS).div_euclid(1_000))
}

/// Microseconds since pg's epoch (civil), rounded towards the past.
fn datetime_micros(dt: DateTime) -> i64 {
  saturate(
    dt.duration_since(PG_EPOCH_DATETIME)
      .as_nanos()
      .div_euclid(1_000),
  )
}

/// Days since pg's epoch.
fn date_days(d: Date) -> i64 {
  d.duration_since(PG_EPOCH_DATE).as_secs() / 86_400
}

/// `micros`, unless it is before pg's earliest timestamp.
fn checked_micros(micros: i64, what: &dyn std::fmt::Display) -> Result<i64, String> {
  if micros < PG_MIN_MICROS {
    return Err(format!(
      "{what} is before PostgreSQL's earliest, 4714-11-24 00:00:00 BC"
    ));
  }
  Ok(micros)
}

fn timestamp_to_wire(ts: jiff::Timestamp) -> Result<i64, String> {
  checked_micros(timestamp_micros(ts), &ts)
}

fn datetime_to_wire(dt: DateTime) -> Result<i64, String> {
  checked_micros(datetime_micros(dt), &dt)
}

fn date_to_wire(d: Date) -> Result<i32, String> {
  i32::try_from(date_days(d))
    .ok()
    .filter(|days| *days >= PG_MIN_DAYS)
    .ok_or_else(|| format!("{d} is before PostgreSQL's earliest date, 4714-11-24 BC"))
}

fn time_to_wire(t: Time) -> Result<i64, String> {
  i64::try_from(t.duration_since(Time::midnight()).as_micros()).map_err(|e| e.to_string())
}

/// An error for pg's `infinity` / `-infinity` (the integer's `max` / `min`) and for wire values
/// outside `jiff` (jiff's range), else `Ok`. jiff is only handed values in its range: some of its
/// constructors assert rather than fail.
fn representable<T: PartialOrd + std::fmt::Display>(
  ty: &str,
  wire: T,
  (min, max): (T, T),
  jiff: (T, T),
) -> Result<(), String> {
  let infinity = match &wire {
    w if *w == max => "infinity",
    w if *w == min => "-infinity",
    w if *w < jiff.0 || *w > jiff.1 => {
      return Err(format!(
        "{ty} {wire} on the wire is beyond jiff's years ±9999"
      ));
    }
    _ => return Ok(()),
  };
  Err(format!("{ty} is {infinity}, which has no jiff value"))
}

/// pg's binary `TIMESTAMPTZ` as an instant.
fn timestamp_from_wire(micros: i64) -> Result<jiff::Timestamp, String> {
  let jiff = (
    timestamp_micros(jiff::Timestamp::MIN),
    timestamp_micros(jiff::Timestamp::MAX),
  );
  representable("timestamptz", micros, (i64::MIN, i64::MAX), jiff)?;
  // Within jiff's range, so no i128 overflow either.
  jiff::Timestamp::from_nanosecond(i128::from(micros) * 1_000 + PG_EPOCH_UNIX_NANOS)
    .map_err(|e| e.to_string())
}

/// pg's binary `TIMESTAMP` as a wall-clock date and time.
fn datetime_from_wire(micros: i64) -> Result<DateTime, String> {
  let jiff = (
    datetime_micros(DateTime::MIN),
    datetime_micros(DateTime::MAX),
  );
  representable("timestamp", micros, (i64::MIN, i64::MAX), jiff)?;
  PG_EPOCH_DATETIME
    .checked_add(SignedDuration::from_micros(micros))
    .map_err(|e| e.to_string())
}

/// pg's binary `DATE` as a calendar date.
fn date_from_wire(days: i32) -> Result<Date, String> {
  let jiff = (date_days(Date::MIN), date_days(Date::MAX));
  representable(
    "date",
    i64::from(days),
    (i32::MIN.into(), i32::MAX.into()),
    jiff,
  )?;
  PG_EPOCH_DATE
    .checked_add(SignedDuration::from_hours(i64::from(days) * 24))
    .map_err(|e| e.to_string())
}

/// pg's binary `TIME` as a time of day (`24:00:00`, which pg accepts, has no jiff value).
fn time_from_wire(micros: i64) -> Result<Time, String> {
  if !(0..DAY_MICROS).contains(&micros) {
    return Err(format!("time of day {micros}µs is not before 24:00"));
  }
  Time::midnight()
    .checked_add(SignedDuration::from_micros(micros))
    .map_err(|e| e.to_string())
}

impl PgTimestamp {
  /// `ts`, checked to be within pg's range (so binding it cannot fail).
  pub(super) fn checked(ts: jiff::Timestamp) -> Result<Self, String> {
    timestamp_to_wire(ts).map(|_| Self(ts))
  }
}

impl PgDateTime {
  /// `dt`, checked to be within pg's range (so binding it cannot fail).
  pub(super) fn checked(dt: DateTime) -> Result<Self, String> {
    datetime_to_wire(dt).map(|_| Self(dt))
  }
}

impl PgDate {
  /// `d`, checked to be within pg's range (so binding it cannot fail).
  pub(super) fn checked(d: Date) -> Result<Self, String> {
    date_to_wire(d).map(|_| Self(d))
  }
}

/// The binary value's bytes as `N` big-endian bytes.
fn wire_bytes<const N: usize>(value: &PgValueRef<'_>, ty: &str) -> Result<[u8; N], BoxDynError> {
  if value.format() == PgValueFormat::Text {
    return Err(format!("{ty} arrived in text format; only the binary format is decoded").into());
  }
  let bytes = value.as_bytes()?;
  <[u8; N]>::try_from(bytes)
    .map_err(|_| format!("{ty} has {} bytes on the wire, not {N}", bytes.len()).into())
}

/// `Type`, `Encode` and `Decode` for a wrapper: its OID, and its conversions to and from the
/// wire integer.
macro_rules! pg_codec {
  ($wrapper:ident, $oid:literal, $name:literal, $int:ty, $to_wire:expr, $from_wire:expr) => {
    impl sqlx::Type<Postgres> for $wrapper {
      fn type_info() -> PgTypeInfo {
        PgTypeInfo::with_oid(Oid($oid))
      }
    }

    impl Encode<'_, Postgres> for $wrapper {
      fn encode_by_ref(&self, buf: &mut PgArgumentBuffer) -> Result<IsNull, BoxDynError> {
        let wire: $int = $to_wire(self.0)?;
        Encode::<Postgres>::encode_by_ref(&wire, buf)
      }

      fn size_hint(&self) -> usize {
        size_of::<$int>()
      }
    }

    impl<'r> Decode<'r, Postgres> for $wrapper {
      fn decode(value: PgValueRef<'r>) -> Result<Self, BoxDynError> {
        let wire = <$int>::from_be_bytes(wire_bytes(&value, $name)?);
        Ok(Self($from_wire(wire)?))
      }
    }
  };
}

pg_codec!(
  PgTimestamp,
  1184,
  "timestamptz",
  i64,
  timestamp_to_wire,
  timestamp_from_wire
);
pg_codec!(
  PgDateTime,
  1114,
  "timestamp",
  i64,
  datetime_to_wire,
  datetime_from_wire
);
pg_codec!(PgDate, 1082, "date", i32, date_to_wire, date_from_wire);
pg_codec!(PgTime, 1083, "time", i64, time_to_wire, time_from_wire);

#[cfg(test)]
mod tests {
  use id_effect::{Cause, Effect, Exit};
  use id_effect_tokio::TokioRuntime;
  use jiff::civil::{date, time};

  use super::*;
  use crate::pg::{PgPoolConfig, PgSqlClient, pg_pool_from_config};
  use crate::{SqlClient, SqlError, SqlValue};

  #[test]
  fn binary_temporals_decode_with_checked_arithmetic() {
    assert_eq!(
      timestamp_from_wire(0).unwrap(),
      "2000-01-01T00:00:00Z".parse::<jiff::Timestamp>().unwrap()
    );
    assert_eq!(
      datetime_from_wire(-1).unwrap(),
      date(1999, 12, 31).at(23, 59, 59, 999_999_000)
    );
    assert_eq!(date_from_wire(366).unwrap(), date(2001, 1, 1));
    assert_eq!(time_from_wire(3_600_000_001).unwrap(), time(1, 0, 0, 1_000));
    assert!(time_from_wire(DAY_MICROS).unwrap_err().contains("24:00"));
    assert!(time_from_wire(-1).is_err());
  }

  #[test]
  fn encoding_truncates_sub_microseconds_towards_the_past() {
    let before_epoch = date(1999, 12, 31).at(23, 59, 59, 999_999_999);
    assert_eq!(datetime_to_wire(before_epoch), Ok(-1));
    let ts = "1999-12-31T23:59:59.9999995Z"
      .parse::<jiff::Timestamp>()
      .unwrap();
    assert_eq!(timestamp_to_wire(ts), Ok(-1));
    assert_eq!(time_to_wire(time(0, 0, 0, 1_999)), Ok(1));
  }

  /// Runs `eff` on tokio (sqlx needs its IO driver).
  async fn run<A: Send + 'static>(eff: Effect<A, SqlError>) -> Result<A, SqlError> {
    match TokioRuntime::current().expect("tokio").run(eff).await {
      Exit::Success(a) => Ok(a),
      Exit::Failure(Cause::Fail(e)) => Err(e),
      Exit::Failure(c) => panic!("unexpected cause: {c:?}"),
    }
  }

  /// A client for `DATABASE_URL`, or `None` (skip) unless `ID_EFFECT_REQUIRE_PG=1`.
  async fn live_pg(test: &str) -> Option<PgSqlClient> {
    let unavailable = |why: String| {
      assert!(
        !std::env::var("ID_EFFECT_REQUIRE_PG").is_ok_and(|v| v == "1"),
        "{test}: ID_EFFECT_REQUIRE_PG=1 but Postgres is unavailable: {why}"
      );
      eprintln!("SKIP {test}: {why} (set DATABASE_URL, and ID_EFFECT_REQUIRE_PG=1 to require it)");
      None
    };
    let Ok(url) = std::env::var("DATABASE_URL") else {
      return unavailable("DATABASE_URL is not set".into());
    };
    let pool = match pg_pool_from_config(PgPoolConfig::from_url(url).with_max_size(1)).await {
      Ok(pool) => pool,
      Err(e) => return unavailable(format!("cannot build pool: {}", e.0)),
    };
    let client = PgSqlClient::new(pool);
    match run(client.connect()).await {
      Ok(()) => Some(client),
      Err(e) => unavailable(format!("cannot connect: {e:?}")),
    }
  }

  async fn one(client: &PgSqlClient, sql: &str, params: &[SqlValue]) -> Result<SqlValue, SqlError> {
    let rows = run(client.query(sql, params)).await?;
    Ok(rows[0].values()[0].clone())
  }

  #[tokio::test]
  async fn pg_wire_datetime_round_trip_bounds() {
    let jiff_max = jiff::Timestamp::from_microsecond(jiff::Timestamp::MAX.as_microsecond()).ok();
    let pg_min_ts = "-004713-11-24T00:00:00Z"
      .parse::<jiff::Timestamp>()
      .unwrap();
    let mut values = vec![
      SqlValue::Timestamp(pg_min_ts),
      SqlValue::Timestamp("1999-12-31T23:59:59.999999Z".parse().unwrap()),
      SqlValue::Timestamp("2024-02-29T12:34:56.123456Z".parse().unwrap()),
      SqlValue::DateTime(date(-4713, 11, 24).at(0, 0, 0, 0)),
      SqlValue::DateTime(date(9999, 12, 31).at(23, 59, 59, 999_999_000)),
      SqlValue::Date(date(-4713, 11, 24)),
      SqlValue::Date(date(1, 1, 1)),
      SqlValue::Date(date(9999, 12, 31)),
      SqlValue::Time(time(0, 0, 0, 0)),
      SqlValue::Time(time(23, 59, 59, 999_999_000)),
    ];
    values.extend(jiff_max.map(SqlValue::Timestamp));

    // Pure wire round trip.
    assert_eq!(timestamp_to_wire(pg_min_ts), Ok(PG_MIN_MICROS));
    assert_eq!(date_to_wire(date(-4713, 11, 24)), Ok(PG_MIN_DAYS));
    assert_eq!(timestamp_from_wire(PG_MIN_MICROS), Ok(pg_min_ts));
    assert_eq!(date_from_wire(PG_MIN_DAYS), Ok(date(-4713, 11, 24)));
    // Before pg's earliest: an error at bind time, not a server error or a panic.
    assert!(PgTimestamp::checked(jiff::Timestamp::MIN).is_err());
    assert!(PgDateTime::checked(date(-4713, 11, 23).at(23, 59, 59, 0)).is_err());
    assert!(PgDate::checked(date(-9999, 1, 1)).is_err());

    let Some(client) = live_pg("pg_wire_datetime_round_trip_bounds").await else {
      return;
    };
    // One statement per value: sqlx caches a statement (and its parameter types) by its SQL.
    for (n, value) in values.iter().enumerate() {
      let sql = format!("SELECT $1 AS v{n}");
      let back = one(&client, &sql, std::slice::from_ref(value)).await;
      assert_eq!(back.as_ref(), Ok(value), "{sql}");
    }
    // The epoch is right in both directions (a symmetric offset would still round-trip).
    for (literal, value) in [
      (
        "'2000-01-01 00:00:00+00'::timestamptz",
        SqlValue::Timestamp("2000-01-01T00:00:00Z".parse().unwrap()),
      ),
      (
        "'4714-11-24 00:00:00+00 BC'::timestamptz",
        SqlValue::Timestamp(pg_min_ts),
      ),
      (
        "'1970-01-01 00:00:00.000001'::timestamp",
        SqlValue::DateTime(date(1970, 1, 1).at(0, 0, 0, 1_000)),
      ),
      ("'4714-11-24 BC'::date", SqlValue::Date(date(-4713, 11, 24))),
      ("'2024-02-29'::date", SqlValue::Date(date(2024, 2, 29))),
      (
        "'23:59:59.999999'::time",
        SqlValue::Time(time(23, 59, 59, 999_999_000)),
      ),
    ] {
      assert_eq!(
        one(&client, &format!("SELECT {literal}"), &[]).await,
        Ok(value.clone()),
        "{literal}"
      );
      let same = one(&client, &format!("SELECT $1 = {literal}"), &[value]).await;
      assert_eq!(same, Ok(SqlValue::Bool(true)), "{literal}");
    }
    let err = one(
      &client,
      "SELECT $1::date",
      &[SqlValue::Date(date(-9999, 1, 1))],
    )
    .await;
    assert!(
      matches!(&err, Err(SqlError::ValueOutOfRange { column, .. }) if column == "$1"),
      "{err:?}"
    );
  }

  #[tokio::test]
  async fn pg_infinity_is_an_error() {
    assert!(
      timestamp_from_wire(i64::MAX)
        .unwrap_err()
        .contains("is infinity")
    );
    assert!(
      timestamp_from_wire(i64::MIN)
        .unwrap_err()
        .contains("-infinity")
    );
    assert!(
      datetime_from_wire(i64::MAX)
        .unwrap_err()
        .contains("infinity")
    );
    assert!(date_from_wire(i32::MAX).unwrap_err().contains("infinity"));
    assert!(date_from_wire(i32::MIN).unwrap_err().contains("-infinity"));
    // Finite but beyond jiff's years ±9999.
    assert!(date_from_wire(5_000_000).unwrap_err().contains("beyond"));
    assert!(
      timestamp_from_wire(i64::MAX - 1)
        .unwrap_err()
        .contains("beyond")
    );
    assert!(
      datetime_from_wire(i64::MIN + 1)
        .unwrap_err()
        .contains("beyond")
    );

    let Some(client) = live_pg("pg_infinity_is_an_error").await else {
      return;
    };
    for (sql, needle) in [
      ("SELECT 'infinity'::timestamptz AS v", "infinity"),
      ("SELECT '-infinity'::timestamptz AS v", "-infinity"),
      ("SELECT 'infinity'::timestamp AS v", "infinity"),
      ("SELECT '-infinity'::timestamp AS v", "-infinity"),
      ("SELECT 'infinity'::date AS v", "infinity"),
      ("SELECT '-infinity'::date AS v", "-infinity"),
      ("SELECT '294276-12-31'::date AS v", "beyond"),
      (
        "SELECT '294276-12-31 00:00:00+00'::timestamptz AS v",
        "beyond",
      ),
      ("SELECT '24:00:00'::time AS v", "24:00"),
    ] {
      match one(&client, sql, &[]).await {
        Err(SqlError::ValueOutOfRange { column, message }) => {
          assert_eq!(column, "v", "{sql}");
          assert!(message.contains(needle), "{sql}: {message}");
        }
        other => panic!("{sql}: {other:?}"),
      }
    }
  }
}

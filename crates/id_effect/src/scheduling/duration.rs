//! Duration constructors, helpers, and string decode — mirrors Effect.ts `Duration`.
//!
//! `std::time::Duration` is re-exported as the base type; this module adds the
//! Effect.ts naming layer (`millis`, `seconds`, `minutes`, …), a `decode` function
//! that parses human-readable strings ("2 seconds", "100ms", etc.), a `format`
//! function, and the standard math/extraction helpers.

pub use std::time::Duration;

// ── Parse error ───────────────────────────────────────────────────────────────

/// Returned by [`duration::decode`] when a string cannot be parsed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DurationParseError {
  /// Original string that failed to parse.
  pub input: String,
}

impl std::fmt::Display for DurationParseError {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    write!(f, "cannot parse {:?} as a duration", self.input)
  }
}

impl std::error::Error for DurationParseError {}

// ── duration module ───────────────────────────────────────────────────────────

/// Free functions on `Duration` — mirrors the Effect.ts `Duration` namespace.
#[allow(clippy::module_inception)] // intentional `duration::duration::…` mirror of Effect.ts
pub mod duration {
  use super::{Duration, DurationParseError};

  // ── Constructors ──────────────────────────────────────────────────────────

  /// `Duration.nanos(n)` — *n* nanoseconds.
  pub fn nanos(n: u64) -> Duration {
    Duration::from_nanos(n)
  }

  /// `Duration.micros(n)` — *n* microseconds.
  pub fn micros(n: u64) -> Duration {
    Duration::from_micros(n)
  }

  /// `Duration.millis(n)` — *n* milliseconds.
  pub fn millis(n: u64) -> Duration {
    Duration::from_millis(n)
  }

  /// `Duration.seconds(n)` — *n* whole seconds.
  pub fn seconds(n: u64) -> Duration {
    Duration::from_secs(n)
  }

  /// `Duration.seconds_f64(n)` — *n* seconds as a floating-point number.
  pub fn seconds_f64(n: f64) -> Duration {
    Duration::from_secs_f64(n)
  }

  /// `Duration.minutes(n)` — *n* minutes.
  pub fn minutes(n: u64) -> Duration {
    Duration::from_secs(n * 60)
  }

  /// `Duration.hours(n)` — *n* hours.
  pub fn hours(n: u64) -> Duration {
    Duration::from_secs(n * 3_600)
  }

  /// `Duration.days(n)` — *n* calendar days (24 h each).
  pub fn days(n: u64) -> Duration {
    Duration::from_secs(n * 86_400)
  }

  /// `Duration.weeks(n)` — *n* weeks (7 days each).
  pub fn weeks(n: u64) -> Duration {
    Duration::from_secs(n * 604_800)
  }

  /// `Duration.infinity` — the maximum representable duration.
  pub const INFINITY: Duration = Duration::MAX;

  /// `Duration.zero` — zero duration.
  pub const ZERO: Duration = Duration::ZERO;

  // ── Decode ────────────────────────────────────────────────────────────────

  /// Parse a human-readable duration string.
  ///
  /// Accepted forms (case-insensitive, optional space between number and unit):
  ///
  /// | Input examples | Unit |
  /// |---|---|
  /// | `"100ns"` / `"100 nanos"` / `"100 nanoseconds"` | nanoseconds |
  /// | `"5us"` / `"5 micros"` / `"5 microseconds"` | microseconds |
  /// | `"30ms"` / `"30 millis"` / `"30 milliseconds"` | milliseconds |
  /// | `"2s"` / `"2 sec"` / `"2 secs"` / `"2 seconds"` | seconds |
  /// | `"5m"` / `"5 min"` / `"5 mins"` / `"5 minutes"` | minutes |
  /// | `"1h"` / `"1 hr"` / `"1 hrs"` / `"1 hour"` / `"1 hours"` | hours |
  /// | `"1d"` / `"1 day"` / `"1 days"` | days |
  /// | `"1w"` / `"1 week"` / `"1 weeks"` | weeks |
  ///
  /// A bare number (e.g. `"500"`) is interpreted as milliseconds (Effect.ts behaviour).
  pub fn decode(input: &str) -> Result<Duration, DurationParseError> {
    let err = || DurationParseError {
      input: input.to_string(),
    };
    let s = input.trim();
    if s.is_empty() {
      return Err(err());
    }

    // Bare number → milliseconds
    if let Ok(n) = s.parse::<f64>() {
      if n < 0.0 {
        return Err(err());
      }
      return Ok(Duration::from_secs_f64(n / 1_000.0));
    }

    // Find where digits (and optional leading minus / decimal point) end
    let split_pos = s.find(|c: char| c.is_alphabetic()).ok_or_else(err)?;

    if split_pos == 0 {
      return Err(err());
    }

    let num_str = s[..split_pos].trim();
    let unit_str = s[split_pos..].trim().to_lowercase();

    let n: f64 = num_str.parse().map_err(|_| err())?;
    if n < 0.0 {
      return Err(err());
    }

    let d = match unit_str.as_str() {
      "ns" | "nanos" | "nanosecond" | "nanoseconds" => Duration::from_secs_f64(n / 1_000_000_000.0),
      "us" | "\u{b5}s" | "micros" | "microsecond" | "microseconds" => {
        Duration::from_secs_f64(n / 1_000_000.0)
      }
      "ms" | "millis" | "millisecond" | "milliseconds" => Duration::from_secs_f64(n / 1_000.0),
      "s" | "sec" | "secs" | "second" | "seconds" => Duration::from_secs_f64(n),
      "m" | "min" | "mins" | "minute" | "minutes" => Duration::from_secs_f64(n * 60.0),
      "h" | "hr" | "hrs" | "hour" | "hours" => Duration::from_secs_f64(n * 3_600.0),
      "d" | "day" | "days" => Duration::from_secs_f64(n * 86_400.0),
      "w" | "week" | "weeks" => Duration::from_secs_f64(n * 604_800.0),
      _ => return Err(err()),
    };
    Ok(d)
  }

  // ── Math ──────────────────────────────────────────────────────────────────

  /// `Duration.sum` — add two durations.
  pub fn sum(a: Duration, b: Duration) -> Duration {
    a + b
  }

  /// `Duration.subtract` — saturating subtraction (never goes below zero).
  pub fn subtract(a: Duration, b: Duration) -> Duration {
    a.saturating_sub(b)
  }

  /// `Duration.times` / `Duration.multiply` — multiply by a scalar.
  pub fn times(a: Duration, n: u32) -> Duration {
    a * n
  }

  /// Return the shorter duration.
  pub fn min(a: Duration, b: Duration) -> Duration {
    a.min(b)
  }

  /// Return the longer duration.
  pub fn max(a: Duration, b: Duration) -> Duration {
    a.max(b)
  }

  /// Clamp `d` to `[minimum, maximum]`.
  pub fn clamp(d: Duration, minimum: Duration, maximum: Duration) -> Duration {
    d.max(minimum).min(maximum)
  }

  /// True when `minimum <= d <= maximum`.
  pub fn between(d: Duration, minimum: Duration, maximum: Duration) -> bool {
    d >= minimum && d <= maximum
  }

  // ── Extraction ────────────────────────────────────────────────────────────

  /// Total milliseconds as `f64`.
  pub fn to_millis(d: Duration) -> f64 {
    d.as_millis() as f64
  }

  /// Total nanoseconds as `u128`.
  pub fn to_nanos(d: Duration) -> u128 {
    d.as_nanos()
  }

  /// Total seconds as `f64`.
  pub fn to_seconds(d: Duration) -> f64 {
    d.as_secs_f64()
  }

  /// Total hours as `f64`.
  pub fn to_hours(d: Duration) -> f64 {
    d.as_secs_f64() / 3_600.0
  }

  // ── Format ────────────────────────────────────────────────────────────────

  /// Format a duration as a compact human-readable string, e.g. `"1h 2m 3.004s"`.
  pub fn format(d: Duration) -> String {
    let total_secs = d.as_secs();
    let subsec_nanos = d.subsec_nanos();

    let weeks = total_secs / 604_800;
    let rem = total_secs % 604_800;
    let days = rem / 86_400;
    let rem = rem % 86_400;
    let hours = rem / 3_600;
    let rem = rem % 3_600;
    let minutes = rem / 60;
    let secs = rem % 60;
    let millis = subsec_nanos / 1_000_000;

    let mut parts = Vec::new();
    if weeks > 0 {
      parts.push(format!("{weeks}w"));
    }
    if days > 0 {
      parts.push(format!("{days}d"));
    }
    if hours > 0 {
      parts.push(format!("{hours}h"));
    }
    if minutes > 0 {
      parts.push(format!("{minutes}m"));
    }
    match (secs, millis) {
      (0, 0) => {}
      (s, 0) => parts.push(format!("{s}s")),
      (0, ms) => parts.push(format!("0.{ms:03}s")),
      (s, ms) => parts.push(format!("{s}.{ms:03}s")),
    }

    if parts.is_empty() {
      "0s".to_string()
    } else {
      parts.join(" ")
    }
  }

  // ── Checks ────────────────────────────────────────────────────────────────

  /// True when `d` is zero.
  pub fn is_zero(d: Duration) -> bool {
    d.is_zero()
  }

  /// True when `d` is not `Duration::MAX` (the sentinel for infinity).
  pub fn is_finite(d: Duration) -> bool {
    d != Duration::MAX
  }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
  use super::Duration;
  use super::duration;
  use rstest::rstest;

  // ── constructors ─────────────────────────────────────────────────────────

  mod constructors {
    use super::*;

    #[test]
    fn nanos_round_trips_to_nanos() {
      assert_eq!(duration::nanos(500).as_nanos(), 500);
    }

    #[test]
    fn micros_round_trips() {
      assert_eq!(duration::micros(200).as_micros(), 200);
    }

    #[test]
    fn millis_round_trips() {
      assert_eq!(duration::millis(1_000).as_millis(), 1_000);
    }

    #[test]
    fn seconds_round_trips() {
      assert_eq!(duration::seconds(60).as_secs(), 60);
    }

    #[test]
    fn minutes_is_60_seconds() {
      assert_eq!(duration::minutes(1), duration::seconds(60));
    }

    #[test]
    fn hours_is_3600_seconds() {
      assert_eq!(duration::hours(1), duration::seconds(3_600));
    }

    #[test]
    fn days_is_86400_seconds() {
      assert_eq!(duration::days(1), duration::seconds(86_400));
    }

    #[test]
    fn weeks_is_7_days() {
      assert_eq!(duration::weeks(1), duration::days(7));
    }

    #[test]
    fn zero_constant_is_zero_duration() {
      assert!(duration::ZERO.is_zero());
    }

    #[test]
    fn infinity_constant_is_max_duration() {
      assert_eq!(duration::INFINITY, Duration::MAX);
    }

    #[test]
    fn seconds_f64_half_second() {
      let d = duration::seconds_f64(0.5);
      assert_eq!(d.as_millis(), 500);
    }
  }

  // ── decode ────────────────────────────────────────────────────────────────

  mod decode {
    use super::*;

    #[rstest]
    #[case::millis_abbrev("100ms", 100)]
    #[case::millis_word("100 millis", 100)]
    #[case::millis_full("100 milliseconds", 100)]
    fn millis_forms(#[case] input: &str, #[case] expected_ms: u64) {
      let d = duration::decode(input).expect("should parse");
      assert_eq!(d.as_millis(), expected_ms as u128);
    }

    #[rstest]
    #[case::s_abbrev("2s", 2)]
    #[case::sec("2 sec", 2)]
    #[case::secs("2 secs", 2)]
    #[case::second("2 second", 2)]
    #[case::seconds("2 seconds", 2)]
    fn seconds_forms(#[case] input: &str, #[case] expected_s: u64) {
      let d = duration::decode(input).expect("should parse");
      assert_eq!(d.as_secs(), expected_s);
    }

    #[rstest]
    #[case::m("5m", 5)]
    #[case::min("5 min", 5)]
    #[case::mins("5 mins", 5)]
    #[case::minute("5 minute", 5)]
    #[case::minutes("5 minutes", 5)]
    fn minutes_forms(#[case] input: &str, #[case] expected_m: u64) {
      let d = duration::decode(input).expect("should parse");
      assert_eq!(d.as_secs(), expected_m * 60);
    }

    #[rstest]
    #[case::h("1h")]
    #[case::hr("1 hr")]
    #[case::hrs("1 hrs")]
    #[case::hour("1 hour")]
    #[case::hours("1 hours")]
    fn hours_forms(#[case] input: &str) {
      let d = duration::decode(input).expect("should parse");
      assert_eq!(d.as_secs(), 3_600);
    }

    #[test]
    fn days_form() {
      assert_eq!(duration::decode("1d").unwrap().as_secs(), 86_400);
      assert_eq!(duration::decode("1 day").unwrap().as_secs(), 86_400);
      assert_eq!(duration::decode("1 days").unwrap().as_secs(), 86_400);
    }

    #[test]
    fn weeks_form() {
      assert_eq!(duration::decode("1w").unwrap().as_secs(), 604_800);
      assert_eq!(duration::decode("1 week").unwrap().as_secs(), 604_800);
      assert_eq!(duration::decode("1 weeks").unwrap().as_secs(), 604_800);
    }

    #[test]
    fn nanos_form() {
      let d = duration::decode("500ns").unwrap();
      assert_eq!(d.as_nanos(), 500);
    }

    #[test]
    fn micros_form() {
      let d = duration::decode("10us").unwrap();
      assert_eq!(d.as_micros(), 10);
    }

    #[test]
    fn bare_number_is_millis() {
      let d = duration::decode("500").unwrap();
      assert_eq!(d.as_millis(), 500);
    }

    #[test]
    fn fractional_seconds() {
      let d = duration::decode("1.5s").unwrap();
      assert_eq!(d.as_millis(), 1_500);
    }

    #[test]
    fn empty_string_returns_error() {
      assert!(duration::decode("").is_err());
    }

    #[test]
    fn whitespace_only_returns_error() {
      assert!(duration::decode("   ").is_err());
    }

    #[test]
    fn unknown_unit_returns_error() {
      assert!(duration::decode("5 fortnights").is_err());
    }

    #[test]
    fn unit_only_no_number_returns_error() {
      assert!(duration::decode("ms").is_err());
    }

    #[test]
    fn error_carries_original_input() {
      let err = duration::decode("bad input").unwrap_err();
      assert_eq!(err.input, "bad input");
    }
  }

  // ── math ─────────────────────────────────────────────────────────────────

  mod math {
    use super::*;

    #[test]
    fn sum_adds_durations() {
      let a = duration::seconds(1);
      let b = duration::millis(500);
      assert_eq!(duration::sum(a, b).as_millis(), 1_500);
    }

    #[test]
    fn subtract_normal_case() {
      let a = duration::seconds(2);
      let b = duration::seconds(1);
      assert_eq!(duration::subtract(a, b), duration::seconds(1));
    }

    #[test]
    fn subtract_saturates_at_zero() {
      assert_eq!(
        duration::subtract(duration::seconds(1), duration::seconds(5)),
        duration::ZERO
      );
    }

    #[test]
    fn times_multiplies() {
      assert_eq!(
        duration::times(duration::seconds(3), 4),
        duration::seconds(12)
      );
    }

    #[test]
    fn min_returns_shorter() {
      assert_eq!(
        duration::min(duration::seconds(1), duration::seconds(5)),
        duration::seconds(1)
      );
    }

    #[test]
    fn max_returns_longer() {
      assert_eq!(
        duration::max(duration::seconds(1), duration::seconds(5)),
        duration::seconds(5)
      );
    }

    #[rstest]
    #[case::below_min(
      duration::ZERO,
      duration::seconds(1),
      duration::seconds(10),
      duration::seconds(1)
    )]
    #[case::in_range(
      duration::seconds(5),
      duration::seconds(1),
      duration::seconds(10),
      duration::seconds(5)
    )]
    #[case::above_max(
      duration::seconds(20),
      duration::seconds(1),
      duration::seconds(10),
      duration::seconds(10)
    )]
    #[case::at_min(
      duration::seconds(1),
      duration::seconds(1),
      duration::seconds(10),
      duration::seconds(1)
    )]
    #[case::at_max(
      duration::seconds(10),
      duration::seconds(1),
      duration::seconds(10),
      duration::seconds(10)
    )]
    fn clamp_cases(
      #[case] d: Duration,
      #[case] min: Duration,
      #[case] max: Duration,
      #[case] expected: Duration,
    ) {
      assert_eq!(duration::clamp(d, min, max), expected);
    }

    #[rstest]
    #[case::in_range(
      duration::seconds(5),
      duration::seconds(1),
      duration::seconds(10),
      true
    )]
    #[case::below(duration::ZERO, duration::seconds(1), duration::seconds(10), false)]
    #[case::above(
      duration::seconds(20),
      duration::seconds(1),
      duration::seconds(10),
      false
    )]
    #[case::at_min(
      duration::seconds(1),
      duration::seconds(1),
      duration::seconds(10),
      true
    )]
    #[case::at_max(
      duration::seconds(10),
      duration::seconds(1),
      duration::seconds(10),
      true
    )]
    fn between_cases(
      #[case] d: Duration,
      #[case] min: Duration,
      #[case] max: Duration,
      #[case] expected: bool,
    ) {
      assert_eq!(duration::between(d, min, max), expected);
    }
  }

  // ── extraction ────────────────────────────────────────────────────────────

  mod extraction {
    use super::*;

    #[test]
    fn to_millis_converts_correctly() {
      assert_eq!(duration::to_millis(duration::seconds(2)), 2_000.0);
    }

    #[test]
    fn to_nanos_converts_correctly() {
      assert_eq!(duration::to_nanos(duration::millis(1)), 1_000_000);
    }

    #[test]
    fn to_seconds_converts_correctly() {
      assert!((duration::to_seconds(duration::millis(500)) - 0.5).abs() < 1e-10);
    }

    #[test]
    fn to_hours_converts_correctly() {
      assert!((duration::to_hours(duration::hours(2)) - 2.0).abs() < 1e-10);
    }
  }

  // ── format ────────────────────────────────────────────────────────────────

  mod format {
    use super::*;

    #[test]
    fn zero_formats_as_0s() {
      assert_eq!(duration::format(duration::ZERO), "0s");
    }

    #[test]
    fn whole_seconds_format() {
      assert_eq!(duration::format(duration::seconds(5)), "5s");
    }

    #[test]
    fn millis_format() {
      assert_eq!(duration::format(duration::millis(500)), "0.500s");
    }

    #[test]
    fn minutes_format() {
      assert_eq!(duration::format(duration::minutes(3)), "3m");
    }

    #[test]
    fn hours_format() {
      assert_eq!(duration::format(duration::hours(2)), "2h");
    }

    #[test]
    fn combined_format() {
      let d = duration::hours(1) + duration::minutes(2) + duration::seconds(3);
      assert_eq!(duration::format(d), "1h 2m 3s");
    }

    #[test]
    fn days_format() {
      assert_eq!(duration::format(duration::days(1)), "1d");
    }

    #[test]
    fn weeks_format() {
      assert_eq!(duration::format(duration::weeks(1)), "1w");
    }

    #[test]
    fn seconds_with_millis_format() {
      let d = duration::seconds(3) + duration::millis(4);
      assert_eq!(duration::format(d), "3.004s");
    }
  }

  // ── checks ────────────────────────────────────────────────────────────────

  mod checks {
    use super::*;

    #[test]
    fn is_zero_true_for_zero() {
      assert!(duration::is_zero(duration::ZERO));
    }

    #[test]
    fn is_zero_false_for_nonzero() {
      assert!(!duration::is_zero(duration::millis(1)));
    }

    #[test]
    fn is_finite_true_for_ordinary_duration() {
      assert!(duration::is_finite(duration::seconds(100)));
    }

    #[test]
    fn is_finite_false_for_max_duration() {
      assert!(!duration::is_finite(duration::INFINITY));
    }
  }
}
